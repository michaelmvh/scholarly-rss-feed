use crate::config::Config;
use crate::reader::{static_render, Publication};
use crate::{build_feed, resolve_feed_request, GeneratedFeed, Provider, ResolveFeedError};
use rss::GuidBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use url::Url;

const SCHEMA_VERSION: u32 = 1;

pub(crate) struct ExportArgs {
    output: PathBuf,
    site: Site,
    previous: Option<PathBuf>,
}

impl ExportArgs {
    pub(crate) fn new(
        output: PathBuf,
        url: &str,
        previous: Option<PathBuf>,
    ) -> Result<Self, String> {
        Ok(Self {
            output,
            site: Site::new(url)?,
            previous,
        })
    }
}

pub(crate) struct Site(Url);

impl Site {
    pub(crate) fn new(value: &str) -> Result<Self, String> {
        let mut url = Url::parse(value).map_err(|error| format!("Invalid site URL: {error}"))?;
        if url.scheme() != "https"
            && !(url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1")))
        {
            return Err("Site URL must use HTTPS (HTTP is allowed for localhost).".to_string());
        }
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url
                .path()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/-_".contains(&byte))
        {
            return Err("Site URL must have a host and a plain path, without credentials, query, or fragment.".to_string());
        }
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        Ok(Self(url))
    }

    pub(crate) fn base_path(&self) -> &str {
        self.0.path()
    }
    pub(crate) fn path(&self, path: &str) -> String {
        format!("{}{path}", self.base_path())
    }
    fn absolute(&self, path: &str) -> String {
        format!("{}{path}", self.0)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Article {
    pub id: String,
    pub feed: String,
    pub keys: BTreeSet<String>,
    pub publication: Publication,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Archive {
    schema_version: u32,
    articles: BTreeMap<String, Article>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    pub value: String,
    pub label: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FeedDocument {
    pub schema_version: u32,
    pub name: String,
    pub title: String,
    pub description: String,
    pub generated_at: String,
    pub sources: Vec<Source>,
    pub articles: Vec<Article>,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !name.starts_with('-')
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("Invalid {}: {error}", path.display()))
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Cannot create {}: {error}", parent.display()))?;
    }
    fs::write(path, bytes).map_err(|error| format!("Cannot write {}: {error}", path.display()))
}

fn json(value: &impl Serialize) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|error| format!("Cannot serialize export: {error}"))
}

fn validate_publication(publication: &Publication) -> Result<(), String> {
    if publication.title.trim().is_empty() {
        return Err("A publication has no title.".to_string());
    }
    for link in publication
        .link
        .iter()
        .chain(&publication.pdf_url)
        .chain(
            publication
                .collection_date
                .iter()
                .map(|date| &date.commit_url),
        )
        .chain(
            publication
                .discovery_sources
                .iter()
                .filter_map(|source| source.url.as_ref()),
        )
    {
        let url = Url::parse(link).map_err(|error| format!("Invalid publication URL: {error}"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(
                "Publication links must be public HTTP(S) URLs without credentials.".to_string(),
            );
        }
    }
    Ok(())
}

fn load_archive(previous: Option<&Path>) -> Result<Archive, String> {
    let Some(previous) = previous else {
        return Ok(Archive {
            schema_version: SCHEMA_VERSION,
            articles: BTreeMap::new(),
        });
    };
    let archive: Archive = read_json(&previous.join("data/archive.json"))?;
    if archive.schema_version != SCHEMA_VERSION {
        return Err(
            "Unsupported previous archive schema; migrate it before exporting.".to_string(),
        );
    }
    for (id, article) in &archive.articles {
        if id != &article.id
            || id.len() != 64
            || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !valid_name(&article.feed)
            || article.keys.is_empty()
        {
            return Err("Invalid retained article identity.".to_string());
        }
        validate_publication(&article.publication)?;
    }
    Ok(archive)
}

fn identify(
    publication: &Publication,
    feed: &str,
    archive: &mut Archive,
) -> Result<Article, String> {
    validate_publication(publication)?;
    let keys = publication
        .id
        .iter()
        .chain(&publication.link)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .collect::<BTreeSet<_>>();
    if keys.is_empty() {
        return Err(format!(
            "Publication {:?} has no stable ID or link.",
            publication.title
        ));
    }
    let matched = archive
        .articles
        .values()
        .filter(|article| !article.keys.is_disjoint(&keys))
        .map(|article| article.id.clone())
        .collect::<Vec<_>>();
    if matched.len() > 1 {
        return Err(
            "Multiple published identities match one record; review the archive before publishing."
                .to_string(),
        );
    }
    let id = matched.into_iter().next().unwrap_or_else(|| {
        format!(
            "{:x}",
            Sha256::digest(keys.first().expect("nonempty identity").as_bytes())
        )
    });
    let mut article = Article {
        id: id.clone(),
        feed: feed.to_string(),
        keys,
        publication: publication.clone(),
    };
    if let Some(previous) = archive.articles.get(&id) {
        article.keys.extend(previous.keys.iter().cloned());
    }
    archive.articles.insert(id, article.clone());
    Ok(article)
}

pub(crate) async fn run(args: &ExportArgs, config_path: &Path) -> Result<(), String> {
    if args.output.exists() {
        return Err(format!(
            "Export directory {} already exists; choose a fresh staging path.",
            args.output.display()
        ));
    }
    let config = Config::load(config_path)?;
    let default = config
        .default_feed
        .as_deref()
        .ok_or("Static export requires a configured default_feed.")?;
    if !config.feeds.contains_key(default) || config.feeds.keys().any(|name| !valid_name(name)) {
        return Err(
            "Static feeds need safe lowercase names and an existing default_feed.".to_string(),
        );
    }
    let provider = Provider::configured()?;
    let mut feeds = BTreeMap::new();
    for name in config.feeds.keys().collect::<BTreeSet<_>>() {
        let request =
            resolve_feed_request(&[("feed".to_string(), name.clone())], &config, provider)
                .await
                .map_err(|error| match error {
                    ResolveFeedError::InvalidRequest(message)
                    | ResolveFeedError::Provider(message) => message,
                })?
                .ok_or_else(|| format!("Feed {name} did not resolve."))?;
        let generated = build_feed(&request).await?;
        if generated.reader.publications.is_empty() {
            return Err(format!(
                "Refusing to publish an empty required feed: {name}."
            ));
        }
        feeds.insert(name.clone(), generated);
    }
    export(args, default, feeds)
}

fn export(
    args: &ExportArgs,
    default: &str,
    feeds: BTreeMap<String, GeneratedFeed>,
) -> Result<(), String> {
    if args.output.exists() {
        return Err("Export destination must not exist.".to_string());
    }
    if !feeds.contains_key(default) || feeds.keys().any(|name| !valid_name(name)) {
        return Err("Invalid static feed names or default feed.".to_string());
    }
    let mut archive = load_archive(args.previous.as_deref())?;
    let generated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let navigation = feeds
        .iter()
        .map(|(name, feed)| (name.clone(), feed.reader.title.clone()))
        .collect::<Vec<_>>();
    // Build and validate all bytes before creating output. A failed export never replaces a deployment.
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut counts = BTreeMap::new();
    for (name, mut generated) in feeds {
        if generated.channel.items().len() != generated.reader.publications.len() {
            return Err(format!("Reader/RSS item count mismatch in {name}."));
        }
        let mut document = FeedDocument {
            schema_version: SCHEMA_VERSION,
            name: name.clone(),
            title: generated.reader.title,
            description: generated.reader.description,
            generated_at: generated_at.clone(),
            sources: generated
                .reader
                .paper_sources
                .into_iter()
                .filter(|source| source.selected)
                .map(|source| Source {
                    value: source.value,
                    label: source.label,
                })
                .collect(),
            articles: Vec::new(),
        };
        let mut ids = BTreeSet::new();
        for (publication, item) in generated
            .reader
            .publications
            .iter()
            .zip(generated.channel.items_mut())
        {
            let article = identify(publication, &name, &mut archive)?;
            if !ids.insert(article.id.clone()) {
                return Err(format!("Duplicate publication identity in {name}."));
            }
            item.set_guid(
                GuidBuilder::default()
                    .value(format!("urn:sha256:{}", article.id))
                    .permalink(false)
                    .build(),
            );
            document.articles.push(article);
        }
        let feed_path = format!("feeds/{name}/feed.xml");
        generated
            .channel
            .set_link(args.site.absolute(&format!("feeds/{name}/")));
        generated.channel.set_ttl("1440".to_string());
        generated.channel.set_pub_date(None::<String>);
        generated.channel.set_last_build_date(None::<String>);
        let xml = generated.channel.to_string();
        rss::Channel::read_from(xml.as_bytes())
            .map_err(|error| format!("Invalid exported RSS: {error}"))?;
        let html = static_render::feed(&args.site, &document, &navigation);
        if name == default {
            files.insert("index.html".to_string(), html.as_bytes().to_vec());
            files.insert("feed.xml".to_string(), xml.as_bytes().to_vec());
        }
        files.insert(format!("feeds/{name}/index.html"), html.into_bytes());
        files.insert(feed_path, xml.into_bytes());
        files.insert(format!("data/{name}.json"), json(&document)?);
        counts.insert(name, document.articles.len());
    }
    for article in archive.articles.values_mut() {
        if !counts.contains_key(&article.feed) {
            article.feed = default.to_string();
        }
        files.insert(
            format!("article/{}/index.html", article.id),
            static_render::article(&args.site, article).into_bytes(),
        );
    }
    files.insert("data/archive.json".to_string(), json(&archive)?);
    files.insert(
        "404.html".to_string(),
        static_render::not_found(&args.site).into_bytes(),
    );
    files.insert(
        "reader.css".to_string(),
        crate::reader::READER_CSS.as_bytes().to_vec(),
    );
    files.insert(
        "favicon.svg".to_string(),
        crate::reader::FAVICON.as_bytes().to_vec(),
    );
    files.insert(
        "apple-touch-icon.png".to_string(),
        crate::APPLE_TOUCH_ICON.to_vec(),
    );
    files.insert(
        "status.json".to_string(),
        json(&serde_json::json!({
            "schema_version": SCHEMA_VERSION, "generated_at": generated_at,
            "generator_version": env!("CARGO_PKG_VERSION"), "feeds": counts,
            "source_freshness": "Not verified: providers may reuse cached source snapshots."
        }))?,
    );
    for (name, bytes) in files {
        write(&args.output.join(name), bytes)?;
    }
    println!("Static site exported to {}", args.output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::{Author, Feed};
    use rss::{ChannelBuilder, ItemBuilder};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "static-export-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn sample(id: &str) -> GeneratedFeed {
        let publication = Publication {
            id: Some(id.to_string()),
            title: "<Research & Science>".to_string(),
            link: Some(format!("https://example.org/{id}")),
            pdf_url: None,
            publication_date: Some("2026-10-01".to_string()),
            collection_date: None,
            venue: None,
            abstract_text: Some("An abstract.".to_string()),
            discovery_sources: vec![],
            curated_categories: vec![],
            authors: vec![Author {
                name: "Optional".to_string(),
                filter_id: "a".to_string(),
                matched_feed: true,
                optional: true,
            }],
        };
        GeneratedFeed {
            channel: ChannelBuilder::default()
                .title("Feed")
                .items(vec![ItemBuilder::default()
                    .title(publication.title.clone())
                    .build()])
                .build(),
            reader: Feed {
                title: "Feed".to_string(),
                description: "Description".to_string(),
                publications: vec![publication],
                paper_sources: vec![],
            },
        }
    }

    #[test]
    #[ignore = "writes a synthetic preview to GSRF_TEST_EXPORT_DIR"]
    fn export_fixture() {
        let output = std::env::var_os("GSRF_TEST_EXPORT_DIR")
            .map(PathBuf::from)
            .expect("set GSRF_TEST_EXPORT_DIR to a fresh directory");
        let site = std::env::var("GSRF_TEST_SITE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080/".to_string());
        let mut generated = sample("optional");
        let mut required = sample("required");
        required.reader.publications[0].authors[0].optional = false;
        required.reader.publications[0].authors[0].filter_id = "required".to_string();
        required.reader.publications[0].authors[0].name = "Required Author".to_string();
        generated
            .reader
            .publications
            .extend(required.reader.publications);
        let items = generated
            .channel
            .items()
            .iter()
            .chain(required.channel.items())
            .cloned()
            .collect::<Vec<_>>();
        generated.channel.set_items(items);
        let args = ExportArgs::new(output, &site, None).unwrap();
        export(
            &args,
            "example",
            BTreeMap::from([("example".to_string(), generated)]),
        )
        .unwrap();
    }

    #[test]
    fn export_cli_rejects_missing_or_conflicting_arguments() {
        for args in [
            vec!["--export", "site"],
            vec!["--site-url", "https://example.org"],
            vec![
                "--export",
                "site",
                "--site-url",
                "https://example.org",
                "127.0.0.1:3005",
            ],
            vec![
                "--export",
                "site",
                "--site-url",
                "https://example.org",
                "--compare-curated",
                "a",
                "b",
            ],
        ] {
            assert!(crate::parse_cli_args_from(args.into_iter().map(str::to_string)).is_err());
        }
        let args = crate::parse_cli_args_from(
            [
                "--export",
                "site",
                "--site-url",
                "https://example.org",
                "--previous",
                "old",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        assert!(args.export.is_some());
    }

    #[test]
    fn rejects_bad_site_urls_and_names() {
        for url in [
            "javascript:bad",
            "https://user:pass@example.org/",
            "https://example.org/?token=secret",
            "https://example.org/#x",
            "http://example.org/",
        ] {
            assert!(Site::new(url).is_err(), "{url}");
        }
        for name in ["../escape", "Upper", "", "-bad", "a/b"] {
            assert!(!valid_name(name));
        }
        assert_eq!(
            Site::new("https://example.org/preview")
                .unwrap()
                .path("feed.xml"),
            "/preview/feed.xml"
        );
    }

    #[test]
    fn exports_real_pages_and_rss_and_retains_expired_articles() {
        let output = directory();
        let next = directory();
        let args = ExportArgs::new(output.clone(), "https://example.org/preview/", None).unwrap();
        export(
            &args,
            "feed",
            BTreeMap::from([("feed".to_string(), sample("one"))]),
        )
        .unwrap();
        let first: Archive = read_json(&output.join("data/archive.json")).unwrap();
        let id = first.articles.keys().next().unwrap();
        let html = fs::read_to_string(output.join("index.html")).unwrap();
        assert!(html.contains("/preview/apple-touch-icon.png"));
        assert!(!html.contains("?rss"));
        assert!(!html.contains("&lt;Research")); // optional-only paper hidden in the default reader
        let detail = fs::read_to_string(output.join(format!("article/{id}/index.html"))).unwrap();
        assert!(detail.contains("&lt;Research &amp; Science&gt;"));
        let channel =
            rss::Channel::read_from(fs::read(output.join("feed.xml")).unwrap().as_slice()).unwrap();
        assert_eq!(channel.items().len(), 1); // optional authors stay in RSS
        assert!(!channel.items()[0].guid().unwrap().is_permalink());
        assert!(export(&args, "feed", BTreeMap::new()).is_err());
        let args =
            ExportArgs::new(next.clone(), "https://example.org/", Some(output.clone())).unwrap();
        export(
            &args,
            "feed",
            BTreeMap::from([("feed".to_string(), sample("two"))]),
        )
        .unwrap();
        assert!(next.join(format!("article/{id}/index.html")).is_file());
        assert_eq!(
            read_json::<Archive>(&next.join("data/archive.json"))
                .unwrap()
                .articles
                .len(),
            2
        );
        fs::remove_dir_all(output).unwrap();
        fs::remove_dir_all(next).unwrap();
    }

    #[test]
    fn malformed_archive_or_unsafe_link_cannot_publish() {
        let output = directory();
        let previous = directory();
        write(&previous.join("data/archive.json"), b"not json").unwrap();
        let args = ExportArgs::new(
            output.clone(),
            "https://example.org/",
            Some(previous.clone()),
        )
        .unwrap();
        assert!(export(
            &args,
            "feed",
            BTreeMap::from([("feed".to_string(), sample("one"))])
        )
        .is_err());
        assert!(!output.exists());
        fs::remove_dir_all(previous).unwrap();
        let mut feed = sample("one");
        feed.reader.publications[0].link = Some("javascript:alert(1)".to_string());
        let args = ExportArgs::new(output.clone(), "https://example.org/", None).unwrap();
        assert!(export(&args, "feed", BTreeMap::from([("feed".to_string(), feed)])).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn retains_identity_when_provider_id_changes_but_link_matches() {
        let mut archive = Archive {
            schema_version: 1,
            articles: BTreeMap::new(),
        };
        let mut publication = sample("one").reader.publications.remove(0);
        let first = identify(&publication, "feed", &mut archive).unwrap();
        publication.id = Some("richer-provider-id".to_string());
        let next = identify(&publication, "feed", &mut archive).unwrap();
        assert_eq!(first.id, next.id);
        assert!(next.keys.contains("one"));
    }
}
