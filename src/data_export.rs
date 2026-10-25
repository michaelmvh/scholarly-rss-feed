use crate::config::Config;
use crate::discovery::snapshot::RefreshPolicy;
use crate::provenance::DiscoverySource;
use crate::reader::{CollectionDate, Publication};
use crate::{Provider, ResolveFeedError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: u32 = 1;
const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 500 * 1024 * 1024;

pub(crate) struct ExportArgs {
    output: PathBuf,
    previous: Option<PathBuf>,
}

impl ExportArgs {
    pub(crate) fn new(output: PathBuf, previous: Option<PathBuf>) -> Self {
        Self { output, previous }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Author {
    key: String,
    name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Article {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    canonical_id: Option<String>,
    identity_keys: BTreeSet<String>,
    title: String,
    link: Option<String>,
    pdf_url: Option<String>,
    publication_date: Option<String>,
    publication_date_precision: String,
    collection_date: Option<CollectionDate>,
    venue: Option<String>,
    authors: Vec<Author>,
    abstract_text: Option<String>,
    discovery_sources: Vec<DiscoverySource>,
    curated_categories: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AuthorOption {
    id: String,
    label: String,
    default_selected: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceOption {
    value: String,
    label: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Feed {
    id: String,
    title: String,
    description: String,
    collected_at: String,
    article_ids: Vec<String>,
    authors: Vec<AuthorOption>,
    matches: BTreeMap<String, Vec<String>>,
    sources: Vec<SourceOption>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    path: String,
    sha256: String,
    count: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    generated_at: String,
    generator_version: String,
    freshness_policy: String,
    default_feed: String,
    feeds: Vec<Feed>,
    article_files: Vec<FileEntry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Shard {
    schema_version: u32,
    articles: Vec<Article>,
}

#[derive(Clone)]
struct Input {
    feed: crate::reader::Feed,
    keys: Vec<BTreeSet<String>>,
    collected_at: String,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
fn json(value: &impl Serialize) -> Result<Vec<u8>, String> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|error| format!("Cannot encode data: {error}"))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_FILE_BYTES {
        return Err("JSON file exceeds the 50 MiB limit.".to_string());
    }
    Ok(bytes)
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path)
        .map_err(|error| format!("Cannot inspect {}: {error}", path.display()))?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES as u64 {
        return Err(format!(
            "Expected a regular JSON file under 50 MiB: {}",
            path.display()
        ));
    }
    fs::read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))
}

fn article_url(link: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(link).map_err(|error| format!("Invalid article URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Article links must be HTTP(S) without credentials.".to_string());
    }
    Ok(url)
}

fn normalize_article_urls(article: &mut Article) -> Result<(), String> {
    for link in article
        .link
        .iter_mut()
        .chain(&mut article.pdf_url)
        .chain(article.collection_date.iter_mut().map(|date| &mut date.commit_url))
        .chain(
            article
                .discovery_sources
                .iter_mut()
                .filter_map(|source| source.url.as_mut()),
        )
    {
        *link = article_url(link)?.into();
    }
    Ok(())
}

fn validate_article(article: &Article) -> Result<(), String> {
    let invalid = |field: &str| format!("Article {:?}: invalid {field}.", article.id);
    if !valid_id(&article.id) {
        return Err(invalid("id"));
    }
    if let Some(canonical_id) = &article.canonical_id {
        if !valid_id(canonical_id)
            || canonical_id == &article.id
            || !article.identity_keys.is_empty()
        {
            return Err(invalid("canonical_id"));
        }
    } else if article.identity_keys.is_empty() {
        return Err(invalid("identity_keys"));
    }
    if article
        .identity_keys
        .iter()
        .any(|key| key.trim().is_empty())
    {
        return Err(invalid("identity_keys"));
    }
    if article.title.trim().is_empty() {
        return Err(invalid("title"));
    }
    for (index, author) in article.authors.iter().enumerate() {
        if author.key.is_empty() {
            return Err(invalid(&format!("authors[{index}].key")));
        }
        if author.name.trim().is_empty() {
            return Err(invalid(&format!("authors[{index}].name")));
        }
    }
    if !["day", "month", "year", "unknown"]
        .contains(&article.publication_date_precision.as_str())
    {
        return Err(invalid("publication_date_precision"));
    }
    for date in article
        .publication_date
        .iter()
        .chain(article.collection_date.iter().map(|date| &date.date))
    {
        chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|error| format!("Invalid article date: {error}"))?;
    }
    for link in article
        .link
        .iter()
        .chain(&article.pdf_url)
        .chain(article.collection_date.iter().map(|date| &date.commit_url))
        .chain(
            article
                .discovery_sources
                .iter()
                .filter_map(|source| source.url.as_ref()),
        )
    {
        article_url(link)?;
    }
    Ok(())
}

fn validate_aliases(archive: &BTreeMap<String, Article>) -> Result<(), String> {
    for article in archive.values() {
        if let Some(id) = &article.canonical_id {
            if !archive
                .get(id)
                .is_some_and(|target| target.canonical_id.is_none())
            {
                return Err(format!(
                    "Article {:?}: canonical_id must reference an existing canonical article.",
                    article.id
                ));
            }
        }
    }
    Ok(())
}

fn load_archive(previous: Option<&Path>) -> Result<BTreeMap<String, Article>, String> {
    let Some(previous) = previous else {
        return Ok(BTreeMap::new());
    };
    let manifest = read(&previous.join("snapshot.json"))?;
    let snapshot: Snapshot = serde_json::from_slice(&manifest)
        .map_err(|error| format!("Invalid previous snapshot: {error}"))?;
    if snapshot.schema_version != SCHEMA_VERSION {
        return Err("Unsupported previous data schema.".to_string());
    }
    let mut archive = BTreeMap::new();
    let mut files = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut total = manifest.len();
    let shard_dir = fs::symlink_metadata(previous.join("articles"))
        .map_err(|error| format!("Missing article shard directory: {error}"))?;
    if !shard_dir.is_dir() {
        return Err("Article shards must use a real directory.".to_string());
    }
    for entry in snapshot.article_files {
        let bucket = entry
            .path
            .strip_prefix("articles/")
            .and_then(|name| name.strip_suffix(".json"))
            .filter(|name| {
                name.len() == 2
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or("Invalid previous shard path.")?;
        if !files.insert(entry.path.clone()) {
            return Err("Repeated article shard.".to_string());
        }
        let bytes = read(&previous.join(&entry.path))?;
        total += bytes.len();
        if total > MAX_SNAPSHOT_BYTES || digest(&bytes) != entry.sha256 {
            return Err(
                "Previous snapshot exceeds size limit or has a checksum mismatch.".to_string(),
            );
        }
        let shard: Shard = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Invalid previous article shard: {error}"))?;
        if shard.schema_version != SCHEMA_VERSION || shard.articles.len() != entry.count {
            return Err("Previous article shard count/schema mismatch.".to_string());
        }
        for article in shard.articles {
            validate_article(&article)
                .map_err(|error| format!("Previous shard {:?}: {error}", entry.path))?;
            if !article.id.starts_with(bucket)
                || archive.contains_key(&article.id)
                || article
                    .identity_keys
                    .iter()
                    .any(|key| !keys.insert(key.clone()))
            {
                return Err("Repeated or misplaced retained article identity.".to_string());
            }
            archive.insert(article.id.clone(), article);
        }
    }
    validate_aliases(&archive)?;
    Ok(archive)
}

fn identify(
    publication: &Publication,
    keys: &BTreeSet<String>,
    archive: &mut BTreeMap<String, Article>,
    index: &mut BTreeMap<String, String>,
    current: &mut BTreeMap<String, usize>,
) -> Result<String, String> {
    if keys.is_empty() {
        return Err("A publication has no stable identity.".to_string());
    }
    let matches = keys
        .iter()
        .filter_map(|key| index.get(key))
        .cloned()
        .collect::<BTreeSet<_>>();
    let id = matches
        .iter()
        .min_by_key(|id| current.get(*id).copied().unwrap_or(usize::MAX))
        .cloned()
        .unwrap_or_else(|| digest(keys.first().expect("identity exists").as_bytes()));
    let mut article = Article {
        id: id.clone(),
        canonical_id: None,
        identity_keys: keys.clone(),
        title: publication.title.clone(),
        link: publication.link.clone(),
        pdf_url: publication.pdf_url.clone(),
        publication_date: publication.publication_date.clone(),
        publication_date_precision: "unknown".to_string(),
        collection_date: publication.collection_date.clone(),
        venue: publication.venue.clone(),
        authors: publication
            .authors
            .iter()
            .map(|author| Author {
                key: author.filter_id.clone(),
                name: author.name.clone(),
            })
            .collect(),
        abstract_text: publication.abstract_text.clone(),
        discovery_sources: publication.discovery_sources.clone(),
        curated_categories: publication.curated_categories.clone(),
    };
    if let Some(old) = archive.get(&id) {
        if current.contains_key(&id) {
            let mut preferred = old.clone();
            if preferred.collection_date.is_none() {
                preferred.collection_date = article.collection_date;
            }
            preferred.identity_keys.extend(article.identity_keys);
            preferred
                .discovery_sources
                .extend(article.discovery_sources);
            preferred
                .curated_categories
                .extend(article.curated_categories);
            article = preferred;
        }
    }
    for matched in &matches {
        let old = &archive[matched];
        article
            .identity_keys
            .extend(old.identity_keys.iter().cloned());
        article
            .discovery_sources
            .extend(old.discovery_sources.iter().cloned());
        article
            .curated_categories
            .extend(old.curated_categories.iter().cloned());
        if article.collection_date.is_none() {
            article.collection_date = old.collection_date.clone();
        }
    }
    article.curated_categories.sort();
    article.curated_categories.dedup();
    normalize_article_urls(&mut article)
        .and_then(|()| {
            article.discovery_sources.sort();
            article.discovery_sources.dedup();
            validate_article(&article)
        })
        .map_err(|error| format!("{error} Source record: {:?}.", publication.id))?;
    if matches.len() > 1 {
        // Retain old URLs, but give every source identity a single canonical owner.
        for old in archive.values_mut() {
            if matches.contains(&old.id) && old.id != id {
                eprintln!(
                    "Reconciling retained article {} into {id}; preserving its article URL as an alias.",
                    old.id
                );
                old.canonical_id = Some(id.clone());
                old.identity_keys.clear();
            } else if old
                .canonical_id
                .as_ref()
                .is_some_and(|target| matches.contains(target))
            {
                old.canonical_id = Some(id.clone());
            }
        }
    }
    for key in &article.identity_keys {
        index.insert(key.clone(), id.clone());
    }
    archive.insert(id.clone(), article);
    let priority = current.len();
    current.entry(id.clone()).or_insert(priority);
    Ok(id)
}

pub(crate) async fn run(args: &ExportArgs, config_path: &Path) -> Result<(), String> {
    if args.output.exists() {
        return Err(
            "Data destination already exists; choose a fresh staging directory.".to_string(),
        );
    }
    let config = Config::load(config_path)?;
    let default = config
        .default_feed
        .as_deref()
        .ok_or("Data export requires a configured default_feed.")?;
    if !config.feeds.contains_key(default) || config.feeds.keys().any(|name| !valid_name(name)) {
        return Err(
            "Data export needs safe lowercase feed names and an existing default feed.".to_string(),
        );
    }
    let mut inputs = BTreeMap::new();
    for name in config.feeds.keys().collect::<BTreeSet<_>>() {
        let request = crate::resolve_feed_request(
            &[("feed".to_string(), name.clone())],
            &config,
            Provider::configured()?,
        )
        .await
        .map_err(|error| match error {
            ResolveFeedError::InvalidRequest(message) | ResolveFeedError::Provider(message) => {
                message
            }
        })?
        .ok_or_else(|| format!("Feed {name} did not resolve."))?;
        println!("Collecting JSON data for {name}");
        let works = crate::fetch_works(&request, RefreshPolicy::RequireFresh)
            .await
            .map_err(|error| format!("Feed {name:?} discovery failed: {error}"))?;
        let keys = works
            .iter()
            .map(|work| {
                work.id
                    .iter()
                    .chain(&work.doi)
                    .chain(&work.alternate_links)
                    .cloned()
                    .chain(work.best_link())
                    .filter(|key| !key.trim().is_empty())
                    .collect()
            })
            .collect();
        inputs.insert(
            name.clone(),
            Input {
                feed: crate::reader_feed_from_works(&request, &works),
                keys,
                collected_at: now(),
            },
        );
    }
    export(args, default, inputs)
}

fn export(
    args: &ExportArgs,
    default: &str,
    mut inputs: BTreeMap<String, Input>,
) -> Result<(), String> {
    if args.output.exists()
        || !inputs.contains_key(default)
        || inputs.keys().any(|name| !valid_name(name))
    {
        return Err("Data destination must be fresh and feed/default names valid.".to_string());
    }
    let mut archive = load_archive(args.previous.as_deref())?;
    let mut index = archive
        .values()
        .flat_map(|article| {
            article
                .identity_keys
                .iter()
                .map(|key| (key.clone(), article.id.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut current = BTreeMap::new();
    let mut feeds = Vec::new();
    // Prefer the default feed's canonical metadata, not whichever feed sorts last.
    let mut order = vec![default.to_string()];
    order.extend(
        inputs
            .keys()
            .filter(|name| name.as_str() != default)
            .cloned(),
    );
    for name in order {
        let Input {
            feed,
            keys,
            collected_at,
        } = inputs.remove(&name).expect("feed exists");
        if feed.publications.is_empty() || feed.publications.len() != keys.len() {
            return Err(format!("Empty or mismatched required feed: {name}."));
        }
        let mut matches: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut article_ids = Vec::new();
        let mut authors: BTreeMap<String, AuthorOption> = BTreeMap::new();
        for (publication, keys) in feed.publications.iter().zip(&keys) {
            let id = identify(publication, keys, &mut archive, &mut index, &mut current)
                .map_err(|error| format!("Feed {name:?}: {error}"))?;
            let mut matched = BTreeSet::new();
            for author in publication
                .authors
                .iter()
                .filter(|author| author.matched_feed)
            {
                matched.insert(author.filter_id.clone());
                authors
                    .entry(author.filter_id.clone())
                    .and_modify(|option| option.default_selected |= !author.optional)
                    .or_insert_with(|| AuthorOption {
                        id: author.filter_id.clone(),
                        label: author.name.clone(),
                        default_selected: !author.optional,
                    });
            }
            let associations = matches.entry(id.clone()).or_insert_with(|| {
                article_ids.push(id);
                Vec::new()
            });
            associations.extend(matched);
            associations.sort();
            associations.dedup();
        }
        let sources = feed
            .paper_sources
            .into_iter()
            .filter(|source| source.selected)
            .map(|source| SourceOption {
                value: source.value,
                label: source.label,
            })
            .collect::<Vec<_>>();
        if sources.is_empty() {
            return Err(format!("No collected sources for {name}."));
        }
        feeds.push(Feed {
            id: name,
            title: feed.title,
            description: feed.description,
            collected_at,
            article_ids,
            authors: authors.into_values().collect(),
            matches,
            sources,
        });
    }
    validate_aliases(&archive)?;
    for feed in &mut feeds {
        let mut matches: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut ids = Vec::new();
        for old in &feed.article_ids {
            let id = archive[old].canonical_id.as_ref().unwrap_or(old);
            let associations = matches.entry(id.clone()).or_insert_with(|| {
                ids.push(id.clone());
                Vec::new()
            });
            associations.extend(feed.matches[old].iter().cloned());
            associations.sort();
            associations.dedup();
        }
        feed.article_ids = ids;
        feed.matches = matches;
    }
    feeds.sort_by(|a, b| a.id.cmp(&b.id));
    let mut buckets: BTreeMap<String, Vec<Article>> = BTreeMap::new();
    for article in archive.into_values() {
        buckets
            .entry(article.id[..2].to_string())
            .or_default()
            .push(article);
    }
    let mut files = BTreeMap::new();
    let mut article_files = Vec::new();
    for (bucket, articles) in buckets {
        let count = articles.len();
        let path = format!("articles/{bucket}.json");
        let bytes = json(&Shard {
            schema_version: SCHEMA_VERSION,
            articles,
        })?;
        article_files.push(FileEntry {
            path: path.clone(),
            sha256: digest(&bytes),
            count,
        });
        files.insert(path, bytes);
    }
    files.insert(
        "snapshot.json".to_string(),
        json(&Snapshot {
            schema_version: SCHEMA_VERSION,
            generated_at: now(),
            generator_version: env!("CARGO_PKG_VERSION").to_string(),
            freshness_policy: "required_discovery_refresh".to_string(),
            default_feed: default.to_string(),
            feeds,
            article_files,
        })?,
    );
    if files.values().map(Vec::len).sum::<usize>() > MAX_SNAPSHOT_BYTES {
        return Err("JSON snapshot exceeds 500 MiB.".to_string());
    }
    for (relative, bytes) in files {
        let path = args.output.join(relative);
        fs::create_dir_all(path.parent().expect("file parent"))
            .map_err(|error| format!("Cannot create data directory: {error}"))?;
        fs::write(&path, bytes)
            .map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
    }
    println!("JSON data exported to {}", args.output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::{Author as ReaderAuthor, Feed as ReaderFeed, PaperSourceOption};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn directory() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "data-export-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }
    fn sample(id: &str, optional: bool) -> Input {
        Input {
            feed: ReaderFeed {
                title: "Research & Science".to_string(),
                description: "Synthetic integration fixture".to_string(),
                paper_sources: vec![PaperSourceOption {
                    value: "core".to_string(),
                    label: "Tracked authors".to_string(),
                    group: "Core feed",
                    selected: true,
                }],
                publications: vec![Publication {
                    id: Some(id.to_string()),
                    title: format!("Paper <{id}> & research"),
                    link: Some(format!("https://example.org/{id}")),
                    pdf_url: None,
                    publication_date: Some("2026-10-01".to_string()),
                    collection_date: None,
                    venue: Some("Example journal".to_string()),
                    authors: vec![ReaderAuthor {
                        name: if optional {
                            "Optional Author"
                        } else {
                            "Required Author"
                        }
                        .to_string(),
                        filter_id: id.to_string(),
                        matched_feed: true,
                        optional,
                    }],
                    abstract_text: Some("An abstract, not HTML.".to_string()),
                    discovery_sources: vec![DiscoverySource::openalex()],
                    curated_categories: vec![],
                }],
            },
            keys: vec![BTreeSet::from([
                id.to_string(),
                format!("https://example.org/{id}"),
            ])],
            collected_at: now(),
        }
    }

    fn punctuation_byline_input() -> Input {
        let mut enriched: crate::openalex::Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W7168259423",
            "doi": "https://doi.org/10.48550/arxiv.2607.09998",
            "title": "Vilya-1: An all-atom foundation model for macrocycle structure prediction and design",
            "publication_date": "2026-07-10",
            "primary_location": {"pdf_url": "https://arxiv.org/pdf/2607.09998"},
            "authorships": [
                {"author": {"display_name": "Vilya Research"}},
                {"author": {"id": "https://openalex.org/A5140784106", "display_name": ":"}, "raw_author_name": ":"},
                {"author": {"display_name": "Pascal Sturmfels"}}
            ]
        }))
        .unwrap();
        enriched.add_discovery_source(DiscoverySource::openalex());
        let curated: crate::openalex::Work = serde_json::from_value(serde_json::json!({
            "id": "arxiv:2607.09998",
            "title": enriched.title,
            "publication_date": "2026-07-01",
            "primary_location": {"landing_page_url": "https://arxiv.org/abs/2607.09998"},
            "authorships": [{"raw_author_name": "Vilya Research: Pascal Sturmfels"}]
        }))
        .unwrap();
        let works = crate::works::merge_works(vec![enriched], vec![curated]);
        assert_eq!(works.len(), 1);
        assert_eq!(works[0].id.as_deref(), Some("https://openalex.org/W7168259423"));
        let mut input = sample("punctuation-byline", false);
        input.keys = vec![BTreeSet::from([
            works[0].id.clone().unwrap(),
            works[0].doi.clone().unwrap(),
        ])];
        input.feed.publications = vec![crate::work_to_publication(&works[0], &[])];
        input
    }

    fn doi_alias_input() -> Input {
        let enriched: crate::openalex::Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W-fixture",
            "doi": "https://doi.org/10.1000/published",
            "title": "Published paper",
            "alternate_links": ["https://doi.org/10.1000/preprint"],
            "best_oa_location": {"pdf_url": "https://example.org/published.pdf"}
        })).unwrap();
        let mut curated: crate::openalex::Work = serde_json::from_value(serde_json::json!({
            "id": "curated:preprint",
            "doi": "https://doi.org/10.1000/preprint",
            "title": "Earlier preprint title"
        })).unwrap();
        curated.add_discovery_source(DiscoverySource::curated_collection(
            "example".to_string(), "Example collection".to_string(), "https://example.org".to_string(),
        ));
        let works = crate::works::merge_works(vec![enriched], vec![curated]);
        assert_eq!(works.len(), 1);
        let work = &works[0];
        let mut input = sample("doi-alias", false);
        input.keys = vec![work.id.iter().chain(&work.doi).chain(&work.alternate_links).cloned().collect()];
        input.feed.publications = vec![crate::work_to_publication(work, &[])];
        input.feed.paper_sources = vec![PaperSourceOption {
            value: "curated:example".to_string(),
            label: "Example collection".to_string(),
            group: "Core feed",
            selected: true,
        }];
        input
    }

    #[test]
    fn punctuation_only_enriched_byline_does_not_abort_export() {
        let output = directory();
        export(
            &ExportArgs::new(output.clone(), None),
            "feed",
            BTreeMap::from([("feed".to_string(), punctuation_byline_input())]),
        )
        .unwrap();
        let archive = load_archive(Some(&output)).unwrap();
        assert_eq!(archive.len(), 1);
        let article = archive.values().next().unwrap();
        assert_eq!(
            article
                .authors
                .iter()
                .map(|author| author.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Vilya Research", "Pascal Sturmfels"]
        );
        assert!(article.identity_keys.contains("https://openalex.org/W7168259423"));
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn article_validation_reports_the_invalid_field() {
        let input = sample("one", false);
        let mut archive = BTreeMap::new();
        let id = identify(
            &input.feed.publications[0],
            &input.keys[0],
            &mut archive,
            &mut BTreeMap::new(),
            &mut BTreeMap::new(),
        )
        .unwrap();
        for field in [
            "id",
            "identity_keys",
            "title",
            "authors[0].key",
            "authors[0].name",
            "publication_date_precision",
        ] {
            let mut article = archive[&id].clone();
            match field {
                "id" => article.id = "invalid".to_string(),
                "identity_keys" => article.identity_keys.clear(),
                "title" => article.title = " \n ".to_string(),
                "authors[0].key" => article.authors[0].key.clear(),
                "authors[0].name" => article.authors[0].name = " \t ".to_string(),
                "publication_date_precision" => article.publication_date_precision.clear(),
                _ => unreachable!(),
            }
            assert_eq!(
                validate_article(&article).unwrap_err(),
                format!("Article {:?}: invalid {field}.", article.id)
            );
        }
        let mut article = archive[&id].clone();
        article.identity_keys.insert(" ".to_string());
        assert!(validate_article(&article).unwrap_err().contains("identity_keys"));
    }

    #[test]
    fn invalid_byline_error_identifies_feed_and_source_before_output() {
        let output = directory();
        let mut input = sample("one", false);
        input.feed.publications[0].authors[0].filter_id.clear();
        let error = export(
            &ExportArgs::new(output.clone(), None),
            "feed",
            BTreeMap::from([("feed".to_string(), input)]),
        )
        .unwrap_err();
        assert!(error.contains("Feed \"feed\""));
        assert!(error.contains("authors[0].key"));
        assert!(error.contains(&digest(b"https://example.org/one")));
        assert!(error.contains("Source record: Some(\"one\")"));
        assert!(!output.exists());
    }

    #[test]
    #[ignore = "writes synthetic JSON to GSRF_TEST_EXPORT_DIR"]
    fn export_fixture() {
        let output = PathBuf::from(
            std::env::var_os("GSRF_TEST_EXPORT_DIR")
                .expect("set GSRF_TEST_EXPORT_DIR to a fresh directory"),
        );
        let mut input = sample("optional", true);
        input.feed.publications[0].discovery_sources = vec![DiscoverySource::curated_collection(
            "example".to_string(),
            "Example collection".to_string(),
            "https://example.org/".to_string(),
        )];
        input.feed.paper_sources.push(PaperSourceOption {
            value: "curated:example".to_string(),
            label: "Example collection".to_string(),
            group: "Core feed",
            selected: true,
        });
        let mut required = sample("required", false);
        required.feed.publications[0].pdf_url =
            Some("https://example.org/Supplement 1/paper.pdf".to_string());
        let previous = directory();
        let mut versions = required.clone();
        let preprint = sample("preprint-version", false);
        versions.feed.publications.extend(preprint.feed.publications);
        versions.keys.extend(preprint.keys.clone());
        export(
            &ExportArgs::new(previous.clone(), None),
            "example",
            BTreeMap::from([("example".to_string(), versions)]),
        )
        .unwrap();
        required.keys[0].extend(preprint.keys.into_iter().flatten());
        input.feed.publications.extend(required.feed.publications);
        input.keys.extend(required.keys);
        let punctuation = punctuation_byline_input();
        input.feed.publications.extend(punctuation.feed.publications);
        input.keys.extend(punctuation.keys);
        export(
            &ExportArgs::new(output, Some(previous.clone())),
            "example",
            BTreeMap::from([
                ("example".to_string(), input),
                ("archive".to_string(), doi_alias_input()),
            ]),
        )
        .unwrap();
        fs::remove_dir_all(previous).unwrap();
    }

    #[tokio::test]
    async fn protein_design_export_filters_membership_without_duplicating_shared_articles() {
        let config: Config = toml::from_str(include_str!("../feeds.toml")).unwrap();
        let candidates: Vec<crate::openalex::Work> = serde_json::from_value(serde_json::json!([
            {
                "id": "https://openalex.org/W-design",
                "title": "Boltz-2: Towards Accurate and Efficient Binding Affinity Prediction",
                "publication_date": "2026-10-01",
                "authorships": [{"author": {"id": "https://openalex.org/A5010124873", "display_name": "Regina Barzilay"}}]
            },
            {
                "id": "https://openalex.org/W-field",
                "title": "De novo protein design",
                "publication_date": "2026-10-01",
                "authorships": [{"author": {"id": "https://openalex.org/A-other", "display_name": "Other Researcher"}}]
            },
            {
                "id": "https://openalex.org/W-clinical",
                "title": "Deep learning for mammography screening",
                "publication_date": "2026-10-01",
                "authorships": [{"author": {"id": "https://openalex.org/A5010124873", "display_name": "Regina Barzilay"}}]
            }
        ]))
        .unwrap();
        let mut inputs = BTreeMap::new();
        for name in ["bioml", "protein-design"] {
            let request = crate::resolve_feed_request(
                &[("feed".into(), name.to_string())],
                &config,
                Provider::OpenAlex,
            )
            .await
            .unwrap()
            .unwrap();
            let mut works = candidates.clone();
            if request.relevance_filter == Some(crate::config::RelevanceFilter::ProteinDesign) {
                crate::protein_design::retain_relevant(&mut works);
            }
            crate::works::mark_feed_authors(&mut works, &request.author_ids);
            for work in &mut works {
                work.add_discovery_source(DiscoverySource::openalex());
            }
            inputs.insert(
                name.to_string(),
                Input {
                    feed: crate::reader_feed_from_works(&request, &works),
                    keys: works
                        .iter()
                        .map(|work| work.id.iter().cloned().collect())
                        .collect(),
                    collected_at: now(),
                },
            );
        }
        let output = directory();
        export(&ExportArgs::new(output.clone(), None), "bioml", inputs).unwrap();
        let snapshot: Snapshot =
            serde_json::from_slice(&fs::read(output.join("snapshot.json")).unwrap()).unwrap();
        let main = snapshot.feeds.iter().find(|feed| feed.id == "bioml").unwrap();
        let design = snapshot
            .feeds
            .iter()
            .find(|feed| feed.id == "protein-design")
            .unwrap();
        assert_eq!(main.article_ids.len(), 3);
        assert_eq!(design.article_ids.len(), 2);
        assert!(design
            .article_ids
            .iter()
            .all(|id| main.article_ids.contains(id)));
        assert_eq!(
            snapshot.article_files.iter().map(|file| file.count).sum::<usize>(),
            3
        );
        fs::remove_dir_all(output).unwrap();
    }

    #[test]
    fn exports_json_only_and_retains_articles_with_stable_shards() {
        let first = directory();
        let next = directory();
        export(
            &ExportArgs::new(first.clone(), None),
            "feed",
            BTreeMap::from([("feed".to_string(), sample("one", true))]),
        )
        .unwrap();
        let previous = load_archive(Some(&first)).unwrap();
        let id = previous.keys().next().unwrap();
        assert_eq!(fs::read_dir(&first).unwrap().count(), 2);
        assert!(!first.join("index.html").exists());
        assert!(!first.join("feed.xml").exists());
        export(
            &ExportArgs::new(next.clone(), Some(first.clone())),
            "feed",
            BTreeMap::from([("feed".to_string(), sample("two", false))]),
        )
        .unwrap();
        let retained = load_archive(Some(&next)).unwrap();
        assert!(retained.contains_key(id));
        assert_eq!(retained.len(), 2);
        let old_bytes = fs::read(first.join(format!("articles/{}.json", &id[..2]))).unwrap();
        if id[..2] != digest(b"https://example.org/two")[..2] {
            assert_eq!(
                old_bytes,
                fs::read(next.join(format!("articles/{}.json", &id[..2]))).unwrap()
            );
        }
        fs::remove_dir_all(first).unwrap();
        fs::remove_dir_all(next).unwrap();
    }

    #[test]
    fn exported_urls_encode_spaces_and_unicode_without_changing_identity() {
        let output = directory();
        let next = directory();
        let mut input = sample("one", false);
        let publication = &mut input.feed.publications[0];
        publication.link = Some("https://example.org/article title".to_string());
        publication.pdf_url = Some(
            "https://ashpublications.org/blood/article-pdf/146/Supplement 1/1696/2456088/blood-4443-main.pdf".to_string(),
        );
        publication.collection_date = Some(CollectionDate {
            date: "2026-10-01".to_string(),
            commit_url: "https://example.org/collection changes".to_string(),
        });
        publication.discovery_sources[0].url = Some("https://example.org/caf\u{e9}".to_string());
        let keys = input.keys[0].clone();
        export(
            &ExportArgs::new(output.clone(), None),
            "feed",
            BTreeMap::from([("feed".to_string(), input)]),
        ).unwrap();
        let archive = load_archive(Some(&output)).unwrap();
        let id = digest(b"https://example.org/one");
        let article = &archive[&id];
        assert_eq!(article.identity_keys, keys);
        assert_eq!(article.link.as_deref(), Some("https://example.org/article%20title"));
        assert_eq!(article.pdf_url.as_deref(), Some(
            "https://ashpublications.org/blood/article-pdf/146/Supplement%201/1696/2456088/blood-4443-main.pdf"
        ));
        assert_eq!(article.collection_date.as_ref().unwrap().commit_url, "https://example.org/collection%20changes");
        assert_eq!(article.discovery_sources[0].url.as_deref(), Some("https://example.org/caf%C3%A9"));
        export(
            &ExportArgs::new(next.clone(), Some(output.clone())),
            "feed",
            BTreeMap::from([("feed".to_string(), sample("one", false))]),
        ).unwrap();
        assert!(load_archive(Some(&next)).unwrap().contains_key(&id));
        fs::remove_dir_all(output).unwrap();
        fs::remove_dir_all(next).unwrap();
    }

    #[test]
    fn malformed_previous_snapshot_and_unsafe_urls_fail_before_output() {
        let root = directory();
        let output = directory();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("snapshot.json"), b"not json").unwrap();
        assert!(export(
            &ExportArgs::new(output.clone(), Some(root.clone())),
            "feed",
            BTreeMap::from([("feed".to_string(), sample("one", false))])
        )
        .is_err());
        assert!(!output.exists());
        let mut input = sample("one", false);
        input.feed.publications[0].link = Some("javascript:alert(1)".to_string());
        assert!(export(
            &ExportArgs::new(output.clone(), None),
            "feed",
            BTreeMap::from([("feed".to_string(), input)])
        )
        .is_err());
        assert!(!output.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn provider_version_aliases_retain_identity_and_author_defaults_are_feed_specific() {
        let mut archive = BTreeMap::new();
        let mut index = BTreeMap::new();
        let mut seen = BTreeMap::new();
        let input = sample("one", true);
        let first = identify(
            &input.feed.publications[0],
            &input.keys[0],
            &mut archive,
            &mut index,
            &mut seen,
        )
        .unwrap();
        let mut changed = sample("published", false);
        changed.keys[0].insert("https://example.org/one".to_string());
        seen.clear();
        assert_eq!(
            first,
            identify(
                &changed.feed.publications[0],
                &changed.keys[0],
                &mut archive,
                &mut index,
                &mut seen
            )
            .unwrap()
        );
        let root = directory();
        export(
            &ExportArgs::new(root.clone(), None),
            "default",
            BTreeMap::from([
                ("default".to_string(), sample("one", true)),
                ("other".to_string(), sample("one", false)),
            ]),
        )
        .unwrap();
        let snapshot: Snapshot =
            serde_json::from_slice(&fs::read(root.join("snapshot.json")).unwrap()).unwrap();
        assert!(!snapshot.feeds[0].authors[0].default_selected);
        assert!(snapshot.feeds[1].authors[0].default_selected);
        assert_eq!(
            snapshot
                .article_files
                .iter()
                .map(|file| file.count)
                .sum::<usize>(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bridged_retained_versions_preserve_urls_and_reconcile_prior_feed_membership() {
        let previous = directory();
        let output = directory();
        let next = directory();
        let mut arxiv = sample("arxiv", false);
        arxiv.keys[0] = BTreeSet::from([
            "https://arxiv.org/abs/2603.02406".to_string(),
            "https://doi.org/10.48550/arxiv.2603.02406".to_string(),
            "https://openalex.org/W7133495554".to_string(),
        ]);
        let mut biorxiv = sample("biorxiv", true);
        biorxiv.keys[0] = BTreeSet::from([
            "biorxiv:10.64898/2026.03.02.708991v3".to_string(),
            "https://doi.org/10.64898/2026.03.02.708991".to_string(),
        ]);
        biorxiv.feed.publications[0].discovery_sources = vec![DiscoverySource::curated_collection(
            "example".to_string(),
            "Example".to_string(),
            "https://example.org".to_string(),
        )];
        biorxiv.feed.publications[0].collection_date = Some(CollectionDate {
            date: "2026-10-01".to_string(),
            commit_url: "https://example.org/commit".to_string(),
        });
        let mut main = arxiv;
        main.feed.publications.extend(biorxiv.feed.publications);
        main.keys.extend(biorxiv.keys);
        export(
            &ExportArgs::new(previous.clone(), None),
            "main",
            BTreeMap::from([("main".to_string(), main.clone())]),
        )
        .unwrap();
        let before = load_archive(Some(&previous)).unwrap();
        let mut merged = sample("https://openalex.org/W7133570701", false);
        merged.feed.publications[0].link = Some("https://arxiv.org/abs/2603.02406".to_string());
        merged.keys[0] = main.keys.iter().flatten().cloned().collect();
        merged.keys[0].insert("https://openalex.org/W7133570701".to_string());
        let repeated = sample("second-match", true);
        merged.feed.publications.extend(repeated.feed.publications);
        merged.keys.push(main.keys[1].clone());
        export(
            &ExportArgs::new(output.clone(), Some(previous.clone())),
            "main",
            BTreeMap::from([
                ("main".to_string(), main),
                ("protein-design".to_string(), merged),
            ]),
        )
        .unwrap();
        let after = load_archive(Some(&output)).unwrap();
        assert_eq!(
            before.keys().collect::<Vec<_>>(),
            after.keys().collect::<Vec<_>>()
        );
        let canonical = after
            .values()
            .find(|article| article.canonical_id.is_none())
            .unwrap();
        let alias = after
            .values()
            .find(|article| article.canonical_id.is_some())
            .unwrap();
        assert_eq!(alias.canonical_id.as_ref(), Some(&canonical.id));
        assert!(canonical
            .identity_keys
            .contains("https://arxiv.org/abs/2603.02406"));
        assert_eq!(canonical.title, "Paper <arxiv> & research");
        assert!(alias.identity_keys.is_empty());
        assert!(before
            .values()
            .all(|article| article.identity_keys.is_subset(&canonical.identity_keys)));
        assert_eq!(canonical.discovery_sources.len(), 2);
        assert!(canonical.collection_date.is_some());
        let snapshot: Snapshot =
            serde_json::from_slice(&fs::read(output.join("snapshot.json")).unwrap()).unwrap();
        for feed in &snapshot.feeds {
            assert_eq!(feed.article_ids, vec![canonical.id.clone()]);
        }
        assert_eq!(
            snapshot.feeds[0].matches[&canonical.id],
            vec!["arxiv", "biorxiv"]
        );
        assert_eq!(snapshot.feeds[1].matches[&canonical.id].len(), 2);
        let mut reappearing = sample("old-version", false);
        reappearing.keys[0] = before[&alias.id].identity_keys.clone();
        export(
            &ExportArgs::new(next.clone(), Some(output.clone())),
            "main",
            BTreeMap::from([("main".to_string(), reappearing)]),
        )
        .unwrap();
        let repeated = load_archive(Some(&next)).unwrap();
        assert_eq!(
            repeated[&alias.id].canonical_id.as_ref(),
            Some(&canonical.id)
        );
        assert!(canonical
            .identity_keys
            .is_subset(&repeated[&canonical.id].identity_keys));
        for path in [previous, output, next] {
            fs::remove_dir_all(path).unwrap();
        }
    }

    #[test]
    fn subsequent_merges_flatten_aliases_and_keep_identity_ownership_unique() {
        let mut archive = BTreeMap::new();
        let mut index = BTreeMap::new();
        let mut current = BTreeMap::new();
        let mut inputs = vec![
            sample("one", false),
            sample("two", false),
            sample("three", false),
        ];
        let ids = inputs
            .iter()
            .map(|input| {
                identify(
                    &input.feed.publications[0],
                    &input.keys[0],
                    &mut archive,
                    &mut index,
                    &mut current,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let third = ids[2].clone();
        let two_keys = inputs[1].keys[0].clone();
        inputs[0].keys[0].extend(two_keys);
        let first_merge = identify(
            &inputs[0].feed.publications[0],
            &inputs[0].keys[0],
            &mut archive,
            &mut index,
            &mut current,
        )
        .unwrap();
        let all_keys = inputs[0].keys[0].clone();
        inputs[2].keys[0].extend(all_keys);
        current = BTreeMap::from([(third.clone(), 0)]);
        let final_id = identify(
            &inputs[2].feed.publications[0],
            &inputs[2].keys[0],
            &mut archive,
            &mut index,
            &mut current,
        )
        .unwrap();
        assert_eq!(final_id, third);
        assert_ne!(first_merge, final_id);
        validate_aliases(&archive).unwrap();
        assert!(index.values().all(|id| id == &final_id));
        for article in archive.values() {
            validate_article(article).unwrap();
            if article.id != final_id {
                assert_eq!(article.canonical_id.as_ref(), Some(&final_id));
            }
        }
        archive.get_mut(&first_merge).unwrap().canonical_id = Some(digest(b"missing"));
        assert!(validate_aliases(&archive)
            .unwrap_err()
            .contains("existing canonical article"));
    }

    #[test]
    fn cli_no_longer_accepts_reader_url_configuration() {
        assert!(
            crate::parse_cli_args_from(["--export", "data"].into_iter().map(str::to_string))
                .unwrap()
                .export
                .is_some()
        );
        for args in [
            vec!["--site-url", "https://example.org"],
            vec!["--previous", "old"],
            vec!["--export", "data", "127.0.0.1:3005"],
            vec!["--export", "data", "--compare-curated", "a", "b"],
        ] {
            assert!(crate::parse_cli_args_from(args.into_iter().map(str::to_string)).is_err());
        }
    }
}
