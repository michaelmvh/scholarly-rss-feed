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

fn validate_article(article: &Article) -> Result<(), String> {
    if !valid_id(&article.id)
        || article.identity_keys.is_empty()
        || article.title.trim().is_empty()
        || article
            .identity_keys
            .iter()
            .any(|key| key.trim().is_empty())
        || article
            .authors
            .iter()
            .any(|author| author.key.is_empty() || author.name.trim().is_empty())
        || !["day", "month", "year", "unknown"]
            .contains(&article.publication_date_precision.as_str())
    {
        return Err("Invalid article identity, title, author, or date precision.".to_string());
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
        let url = url::Url::parse(link).map_err(|error| format!("Invalid article URL: {error}"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("Article links must be HTTP(S) without credentials.".to_string());
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
            validate_article(&article)?;
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
    Ok(archive)
}

fn identify(
    publication: &Publication,
    keys: &BTreeSet<String>,
    archive: &mut BTreeMap<String, Article>,
    index: &mut BTreeMap<String, String>,
    current: &mut BTreeSet<String>,
) -> Result<String, String> {
    if keys.is_empty() {
        return Err("A publication has no stable identity.".to_string());
    }
    let matches = keys
        .iter()
        .filter_map(|key| index.get(key))
        .cloned()
        .collect::<BTreeSet<_>>();
    if matches.len() > 1 {
        return Err(
            "Multiple retained identities match one record; resolve the archive before publishing."
                .to_string(),
        );
    }
    let id = matches
        .into_iter()
        .next()
        .unwrap_or_else(|| digest(keys.first().expect("identity exists").as_bytes()));
    let mut article = Article {
        id: id.clone(),
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
        article
            .identity_keys
            .extend(old.identity_keys.iter().cloned());
        if current.contains(&id) {
            let mut preferred = old.clone();
            preferred.identity_keys = article.identity_keys;
            preferred
                .discovery_sources
                .extend(article.discovery_sources);
            preferred.discovery_sources.sort();
            preferred.discovery_sources.dedup();
            preferred
                .curated_categories
                .extend(article.curated_categories);
            preferred.curated_categories.sort();
            preferred.curated_categories.dedup();
            article = preferred;
        }
    }
    validate_article(&article)?;
    for key in &article.identity_keys {
        index.insert(key.clone(), id.clone());
    }
    archive.insert(id.clone(), article);
    current.insert(id.clone());
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
        let works = crate::fetch_works(&request, RefreshPolicy::RequireFresh).await?;
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
    let mut current = BTreeSet::new();
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
        let mut matches = BTreeMap::new();
        let mut article_ids = Vec::new();
        let mut authors: BTreeMap<String, AuthorOption> = BTreeMap::new();
        for (publication, keys) in feed.publications.iter().zip(&keys) {
            let id = identify(publication, keys, &mut archive, &mut index, &mut current)?;
            if matches.contains_key(&id) {
                return Err(format!("Duplicate article identity in {name}."));
            }
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
            matches.insert(id.clone(), matched.into_iter().collect());
            article_ids.push(id);
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "data-export-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
        let required = sample("required", false);
        input.feed.publications.extend(required.feed.publications);
        input.keys.extend(required.keys);
        export(
            &ExportArgs::new(output, None),
            "example",
            BTreeMap::from([("example".to_string(), input)]),
        )
        .unwrap();
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
        let mut seen = BTreeSet::new();
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
