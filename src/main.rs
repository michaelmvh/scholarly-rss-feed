mod config;
mod curated;
pub mod discovery;
mod evaluation;
mod github_history;
mod google_scholar;
mod openalex;
mod openalex_enrichment;
mod provenance;
mod reader;
mod snapshot_store;
mod data_export;
mod works;

use crate::config::{Config, FeedConfig};
use crate::openalex::{
    normalize_id, Author, AuthorsResponse, SourceRecord, SourcesResponse, Work,
    API_BASE,
};
use crate::provenance::DiscoverySource;
use crate::works::{
    mark_authors_by_name, mark_feed_authors, merge_works, normalize_author_name, resolve_author_match,
    resolve_openalex_author_match,
};
use chrono::{Duration, NaiveDate, NaiveTime, Utc};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::http::Error;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use lazy_static::lazy_static;
use parking_lot::RwLock;
use rss::extension::dublincore::DublinCoreExtension;
use rss::{Category, Channel, ChannelBuilder, GuidBuilder, ItemBuilder, Source, TextInput};
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration as StdDuration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::time::Instant;

lazy_static! {
    static ref RSS_CHANNELS: Arc<RwLock<HashMap<FeedCacheKey, CachedFeed>>> =
        Arc::new(RwLock::new(HashMap::new()));
    static ref FEED_BUILDS: FeedBuilds = Arc::new(RwLock::new(HashMap::new()));
    static ref CONNECTION_LIMIT: Arc<Semaphore> =
        Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    static ref FEED_BUILD_LIMIT: Arc<Semaphore> =
        Arc::new(Semaphore::new(MAX_CONCURRENT_FEED_BUILDS));
    static ref CLIENT_REQUESTS: Arc<RwLock<HashMap<IpAddr, VecDeque<Instant>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    pub static ref CLIENT: reqwest::Client = reqwest::Client::builder()
        .user_agent("scholarly-rss-feed")
        .connect_timeout(StdDuration::from_secs(10))
        .timeout(StdDuration::from_secs(30))
        .build()
        .expect("failed to build HTTP client");
}

static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

const DEFAULT_FROM_DAYS: u32 = 365;
const DEFAULT_NATIVE_FROM_DAYS: u32 = 7;
const DEFAULT_MAX_OPENALEX_WORKS: usize = 20_000;
const OPENALEX_PAGE_SIZE: usize = 100;
const MAX_CONCURRENT_CONNECTIONS: usize = 64;
const MAX_CONCURRENT_FEED_BUILDS: usize = 8;
const MAX_GENERATED_FEEDS: usize = 128;
const MAX_QUERY_LENGTH: usize = 8 * 1024;
const MAX_QUERY_PARAMS: usize = 100;
const MAX_READER_AUTHOR_PARAMS: usize = 256;
const MAX_DYNAMIC_FILTER_VALUES: usize = 25;
const MAX_QUERY_VALUE_LENGTH: usize = 256;
const MAX_REQUESTS_PER_MINUTE: usize = 60;
const MAX_RATE_LIMIT_CLIENTS: usize = 1024;
const HEADER_READ_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const CONNECTION_TIMEOUT: StdDuration = StdDuration::from_secs(5 * 60);
const DYNAMIC_BROWSER_CACHE_CONTROL: &str = "public, max-age=0, must-revalidate";
const DYNAMIC_CDN_CACHE_CONTROL: &str = "public, max-age=7200, stale-while-revalidate=86400";
pub(crate) const BIORXIV_CATEGORY_PARAM: &str = "biorxiv_category";
pub(crate) const ARXIV_CATEGORY_PARAM: &str = "arxiv_category";
pub(crate) const NATIVE_DAYS_PARAM: &str = "native_days";
pub(crate) const PAPER_SOURCE_MODE_PARAM: &str = "paper_sources";
pub(crate) const PAPER_SOURCE_PARAM: &str = "paper_source";
const CUSTOM_PAPER_SOURCES: &str = "custom";
const CORE_PAPER_SOURCE: &str = "core";
const CURATED_SOURCE_PREFIX: &str = "curated:";
const BIORXIV_SOURCE_PREFIX: &str = "biorxiv:";
const ARXIV_SOURCE_PREFIX: &str = "arxiv:";
const APPLE_TOUCH_ICON: &[u8] = include_bytes!("assets/apple-touch-icon.png");

#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
enum Provider {
    OpenAlex,
    GoogleScholar,
}

#[derive(Debug)]
enum ResolveFeedError {
    InvalidRequest(String),
    Provider(String),
}

#[derive(Clone, Debug)]
enum GenerateFeedError {
    Busy,
    Build(String),
}

impl Provider {
    fn configured() -> Result<Self, String> {
        Self::parse(&env::var("GSRF_PROVIDER").unwrap_or_else(|_| "openalex".to_string()))
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openalex" => Ok(Self::OpenAlex),
            "google-scholar" | "google_scholar" | "scholar" => Ok(Self::GoogleScholar),
            other => Err(format!(
                "Unknown GSRF_PROVIDER \"{other}\"; expected \"openalex\" or \"google-scholar\"."
            )),
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::OpenAlex => "OpenAlex",
            Self::GoogleScholar => "Google Scholar",
        }
    }

    fn site_url(self) -> &'static str {
        match self {
            Self::OpenAlex => "https://openalex.org",
            Self::GoogleScholar => "https://scholar.google.com",
        }
    }

    fn search_url(self) -> &'static str {
        match self {
            Self::OpenAlex => "https://openalex.org/works",
            Self::GoogleScholar => "https://scholar.google.com/scholar",
        }
    }

    fn discovery_source(self) -> DiscoverySource {
        match self {
            Self::OpenAlex => DiscoverySource::openalex(),
            Self::GoogleScholar => DiscoverySource::google_scholar(),
        }
    }
}

#[derive(Clone)]
struct GeneratedFeed {
    channel: Channel,
    reader: reader::Feed,
}

#[derive(Clone)]
struct CachedFeed {
    feed: GeneratedFeed,
    last_accessed: Instant,
}

struct CliArgs {
    address: String,
    config_path: PathBuf,
    comparison: Option<ComparisonArgs>,
    export: Option<data_export::ExportArgs>,
}

struct ComparisonArgs {
    feed_name: String,
    source_name: String,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct FeedCacheKey {
    provider: Provider,
    title: Option<String>,
    author_ids: Vec<String>,
    google_scholar_authors: Vec<String>,
    tracked_author_names: Vec<String>,
    people: Vec<config::PersonConfig>,
    curated_sources: Vec<String>,
    biorxiv_categories: Vec<String>,
    arxiv_categories: Vec<String>,
    source_ids: Vec<String>,
    from: String,
    native_from: String,
    topics: Vec<String>,
    include_core: bool,
    core_available: bool,
    available_curated_sources: Vec<String>,
}
type FeedBuild = Arc<tokio::sync::OnceCell<Result<GeneratedFeed, GenerateFeedError>>>;
type FeedBuilds = Arc<RwLock<HashMap<FeedCacheKey, FeedBuild>>>;

/// Fully-resolved description of a feed, used as the cache key.
#[derive(Debug, Hash, Eq, PartialEq, Clone)]
struct FeedRequest {
    provider: Provider,
    /// Normalized, deduped, sorted OpenAlex author ids.
    author_ids: Vec<String>,
    /// Author names queried by the archived Google Scholar provider.
    google_scholar_authors: Vec<String>,
    /// Canonical author names and explicit aliases used for provider-neutral highlighting.
    tracked_author_names: Vec<String>,
    /// Configured identities and reader defaults, including non-optional authors.
    people: Vec<config::PersonConfig>,
    /// Curated paper collections included in the feed.
    curated_sources: Vec<String>,
    /// Native bioRxiv categories included in the feed.
    biorxiv_categories: Vec<String>,
    /// Native arXiv categories included in the feed.
    arxiv_categories: Vec<String>,
    /// Normalized, deduped, sorted OpenAlex source (journal) ids.
    source_ids: Vec<String>,
    /// Earliest publication date (YYYY-MM-DD).
    from: String,
    /// Earliest native-source publication date (YYYY-MM-DD).
    native_from: String,
    /// Sorted OpenAlex topic ids.
    topics: Vec<String>,
    /// Whether the configured author/journal provider side is included.
    include_core: bool,
    /// Whether this feed has a configured or ad-hoc provider side available.
    core_available: bool,
    /// Curated sources offered by this feed, including deselected sources.
    available_curated_sources: Vec<String>,
    /// Optional channel title carried alongside (not part of identity below).
    title: Option<String>,
}

impl FeedRequest {
    fn cache_key(&self) -> FeedCacheKey {
        FeedCacheKey {
            provider: self.provider,
            title: self.title.clone(),
            author_ids: self.author_ids.clone(),
            google_scholar_authors: self.google_scholar_authors.clone(),
            tracked_author_names: self.tracked_author_names.clone(),
            people: self.people.clone(),
            curated_sources: self.curated_sources.clone(),
            biorxiv_categories: self.biorxiv_categories.clone(),
            arxiv_categories: self.arxiv_categories.clone(),
            source_ids: self.source_ids.clone(),
            from: self.from.clone(),
            native_from: self.native_from.clone(),
            topics: self.topics.clone(),
            include_core: self.include_core,
            core_available: self.core_available,
            available_curated_sources: self.available_curated_sources.clone(),
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = match parse_cli_args_from(env::args().skip(1)) {
        Ok(cli) => cli,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    CONFIG_PATH.set(cli.config_path.clone()).ok();

    if let Some(export) = cli.export {
        if let Err(error) = data_export::run(&export, &cli.config_path).await {
            eprintln!("Data export failed: {error}");
            std::process::exit(1);
        }
        return;
    }

    if let Some(comparison) = cli.comparison {
        if let Err(error) = run_curated_comparison(&comparison, &cli.config_path).await {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }

    let addr = SocketAddr::from_str(&cli.address).unwrap();

    println!("Listening on {}...", cli.address);
    println!("Using config file: {}", cli.config_path.display());
    let listener = TcpListener::bind(addr).await.unwrap();

    println!("Server started");
    let mut last_update = Instant::now();
    loop {
        // Clear hourly so feeds refresh and the cache doesn't grow unbounded.
        if last_update.elapsed() >= StdDuration::from_secs(3600) {
            println!("Clearing cache");
            RSS_CHANNELS.write().clear();
            FEED_BUILDS
                .write()
                .retain(|_, build| Arc::strong_count(build) > 1);
            last_update = Instant::now();
        }

        let permit = CONNECTION_LIMIT
            .clone()
            .acquire_owned()
            .await
            .expect("connection semaphore closed");
        if let Ok((stream, peer_addr)) = listener.accept().await {
            let io = TokioIo::new(stream);

            tokio::task::spawn(async move {
                let _permit = permit;
                let connection = http1::Builder::new()
                    .timer(TokioTimer::new())
                    .header_read_timeout(HEADER_READ_TIMEOUT)
                    .keep_alive(false)
                    .serve_connection(
                        io,
                        service_fn(move |request| serve_feed(request, peer_addr.ip())),
                    );
                match tokio::time::timeout(CONNECTION_TIMEOUT, connection).await {
                    Ok(Ok(_)) => (),
                    Ok(Err(error)) => eprintln!("Error serving connection: {error:?}"),
                    Err(_) => eprintln!("Connection exceeded the five-minute deadline"),
                }
            });
        } else {
            drop(permit);
        }
    }
}

/// Parse CLI args: first non-flag positional is the bind address, `--config <path>`
/// selects the feeds config file, and `--compare-curated <feed> <source>` runs
/// an evaluation instead of the server. `--export <directory>` emits JSON data;
/// `--previous <directory>` supplies the prior retained data snapshot.
fn parse_cli_args_from(args: impl IntoIterator<Item = String>) -> Result<CliArgs, String> {
    let mut address: Option<String> = None;
    let mut config: Option<String> = None;
    let mut comparison = None;
    let mut export_dir = None;
    let mut previous_dir = None;

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--export" | "--previous" => {
                let value = args.next().ok_or_else(|| format!("{arg} requires a value"))?;
                match arg.as_str() {
                    "--export" => export_dir = Some(PathBuf::from(value)),
                    _ => previous_dir = Some(PathBuf::from(value)),
                }
            }
            "--config" => {
                config = Some(
                    args.next()
                        .ok_or_else(|| "--config requires a path".to_string())?,
                );
            }
            "--compare-curated" => {
                let feed_name = args.next().ok_or_else(|| {
                    "--compare-curated requires a feed name and curated source".to_string()
                })?;
                let source_name = args.next().ok_or_else(|| {
                    "--compare-curated requires a feed name and curated source".to_string()
                })?;
                comparison = Some(ComparisonArgs {
                    feed_name,
                    source_name,
                });
            }
            flag if flag.starts_with("--") => {
                return Err(format!("Unknown option \"{flag}\"."));
            }
            other => {
                if address.is_none() {
                    address = Some(other.to_string());
                } else {
                    return Err(format!("Unexpected positional argument \"{other}\"."));
                }
            }
        }
    }

    if export_dir.is_none() && previous_dir.is_some() {
        return Err("--previous requires --export.".to_string());
    }
    if export_dir.is_some() && (comparison.is_some() || address.is_some()) {
        return Err("--export cannot be combined with a server address or --compare-curated.".to_string());
    }
    let export = export_dir.map(|output| data_export::ExportArgs::new(output, previous_dir));
    let address = address.unwrap_or_else(|| "127.0.0.1:3005".to_string());
    let config = config
        .or_else(|| env::var("GSRF_CONFIG").ok())
        .unwrap_or_else(|| "feeds.toml".to_string());

    Ok(CliArgs {
        address,
        config_path: PathBuf::from(config),
        comparison,
        export,
    })
}

async fn run_curated_comparison(
    comparison: &ComparisonArgs,
    config_path: &std::path::Path,
) -> Result<(), String> {
    let config = Config::load(config_path)?;
    let provider = Provider::configured()?;
    let params = vec![("feed".to_string(), comparison.feed_name.clone())];
    let request = resolve_feed_request(&params, &config, provider)
        .await
        .map_err(|error| match error {
            ResolveFeedError::InvalidRequest(message) | ResolveFeedError::Provider(message) => {
                message
            }
        })?
        .ok_or_else(|| {
            format!(
                "Feed \"{}\" has no provider identifiers to compare.",
                comparison.feed_name
            )
        })?;
    curated::validate_sources(std::slice::from_ref(&comparison.source_name))?;

    let (provider_works, curated_evaluation) = tokio::join!(
        fetch_provider_works(&request),
        curated::fetch_sources_for_evaluation(
            &CLIENT,
            std::slice::from_ref(&comparison.source_name),
            &request.from,
        ),
    );
    let mut provider_works = provider_works?;
    for work in &mut provider_works {
        work.add_discovery_source(request.provider.discovery_source());
    }
    let curated_evaluation = curated_evaluation?;
    let report = evaluation::compare(
        &provider_works,
        &curated_evaluation.works,
        curated_evaluation.diagnostics,
    );
    print!(
        "{}",
        report.render_markdown(
            &comparison.feed_name,
            &comparison.source_name,
            &request.from,
            provider.display_name(),
        )
    );
    Ok(())
}

async fn serve_feed(
    request: Request<Incoming>,
    peer_ip: IpAddr,
) -> Result<Response<Full<Bytes>>, Error> {
    if request.uri().path() == "/favicon.svg" {
        return Response::builder()
            .header("Content-Type", "image/svg+xml")
            .header(
                "Cache-Control",
                "public, max-age=300, s-maxage=7200, stale-while-revalidate=86400",
            )
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from_static(reader::FAVICON.as_bytes())));
    }
    if request.uri().path() == "/reader.css" {
        return Response::builder()
            .header("Content-Type", "text/css; charset=utf-8")
            .header(
                "Cache-Control",
                "public, max-age=300, s-maxage=7200, stale-while-revalidate=86400",
            )
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from_static(reader::READER_CSS.as_bytes())));
    }
    if request.uri().path() == "/reader.js" {
        return Response::builder()
            .header("Content-Type", "text/javascript; charset=utf-8")
            .header(
                "Cache-Control",
                "public, max-age=300, s-maxage=7200, stale-while-revalidate=86400",
            )
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from_static(reader::READER_JS.as_bytes())));
    }
    if request.uri().path() == "/apple-touch-icon.png" {
        return Response::builder()
            .header("Content-Type", "image/png")
            .header(
                "Cache-Control",
                "public, max-age=300, s-maxage=7200, stale-while-revalidate=86400",
            )
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from_static(APPLE_TOUCH_ICON)));
    }

    let forwarded_ip = request
        .headers()
        .get("cf-connecting-ip")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let client_ip = select_client_ip(peer_ip, forwarded_ip, trust_proxy_client_ip());
    if !allow_request(&mut CLIENT_REQUESTS.write(), client_ip, Instant::now()) {
        return Response::builder()
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("Retry-After", "60")
            .status(StatusCode::TOO_MANY_REQUESTS)
            .body(Full::new(Bytes::from(
                "Too many requests; retry in one minute.",
            )));
    }

    // Preserve repeated query params (e.g. multiple ?author_id=).
    let query = request.uri().query().unwrap_or_default();
    let params: Vec<(String, String)> = (!query.is_empty())
        .then_some(query)
        .map(|v| {
            url::form_urlencoded::parse(v.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default();
    let path_article_id = reader::article_id_from_path(request.uri().path());

    if let Err(message) = validate_dynamic_request(query, &params) {
        return Response::builder()
            .header("Content-Type", "text/plain; charset=utf-8")
            .status(StatusCode::BAD_REQUEST)
            .body(Full::new(Bytes::from(message)));
    }
    let config = match Config::load(config_path()) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            return Response::builder()
                .header("Content-Type", "text/plain; charset=utf-8")
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from(message)));
        }
    };
    let provider = match Provider::configured() {
        Ok(provider) => provider,
        Err(message) => {
            return Response::builder()
                .header("Content-Type", "text/plain; charset=utf-8")
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from(message)));
        }
    };

    let feed_request = match resolve_feed_request(&params, &config, provider).await {
        Ok(Some(fr)) => fr,
        Ok(None) => {
            return Response::builder()
                .header("Access-Control-Allow-Origin", "*")
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from(
                    "No authors or journals specified. Provide ?author_id=, ?orcid=, \
                     ?author=, ?source_id=, ?issn=, or ?journal=, or configure feeds in \
                     the config file and use ?feed=<name>.",
                )));
        }
        Err(ResolveFeedError::InvalidRequest(message)) => {
            return Response::builder()
                .header("Access-Control-Allow-Origin", "*")
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from(message)));
        }
        Err(ResolveFeedError::Provider(message)) => {
            eprintln!("{message}");
            return Response::builder()
                .header("Content-Type", "text/plain; charset=utf-8")
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::from(message)));
        }
    };

    let feed = match generate_feed_if_needed(feed_request).await {
        Ok(feed) => feed,
        Err(GenerateFeedError::Busy) => {
            return Response::builder()
                .header("Content-Type", "text/plain; charset=utf-8")
                .header("Retry-After", "5")
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(Full::new(Bytes::from(
                    "The feed builder is busy; retry shortly.",
                )));
        }
        Err(GenerateFeedError::Build(message)) => {
            eprintln!("{message}");
            return Response::builder()
                .header("Content-Type", "text/plain; charset=utf-8")
                .status(StatusCode::BAD_GATEWAY)
                .body(Full::new(Bytes::from(message)));
        }
    };
    let (status, content_type, body) = if has_param(&params, "rss") {
        (
            StatusCode::OK,
            "text/xml; charset=utf-8",
            Bytes::from(feed.channel.to_string()),
        )
    } else if let Some(article_id) = path_article_id {
        match reader::render_article(&feed.reader, &article_id, &params) {
            Some(html) => (
                StatusCode::OK,
                "text/html; charset=utf-8",
                Bytes::from(html),
            ),
            None => (
                StatusCode::NOT_FOUND,
                "text/plain; charset=utf-8",
                Bytes::from("Publication not found in this feed."),
            ),
        }
    } else {
        (
            StatusCode::OK,
            "text/html; charset=utf-8",
            Bytes::from(reader::render_feed(&feed.reader, &params)),
        )
    };

    Response::builder()
        .header("Content-Type", content_type)
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", DYNAMIC_BROWSER_CACHE_CONTROL)
        .header("Cloudflare-CDN-Cache-Control", DYNAMIC_CDN_CACHE_CONTROL)
        .status(status)
        .body(Full::new(body))
}

fn config_path() -> &'static PathBuf {
    CONFIG_PATH.get().expect("config path not initialized")
}

/// Collect the values of a repeated query parameter.
fn collect_param(params: &[(String, String)], key: &str) -> Vec<String> {
    params
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .filter(|v| !v.trim().is_empty())
        .collect()
}

fn first_param(params: &[(String, String)], key: &str) -> Option<String> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .filter(|v| !v.trim().is_empty())
}

fn has_param(params: &[(String, String)], key: &str) -> bool {
    params.iter().any(|(name, _)| name == key)
}

fn validate_dynamic_request(query: &str, params: &[(String, String)]) -> Result<(), String> {
    if query.len() > MAX_QUERY_LENGTH {
        return Err(format!(
            "Query string exceeds the {MAX_QUERY_LENGTH}-byte limit."
        ));
    }
    let reader_author_count = params
        .iter()
        .filter(|(name, _)| name == reader::AUTHOR_PARAM)
        .count();
    if reader_author_count > MAX_READER_AUTHOR_PARAMS {
        return Err(format!(
            "Query contains more than {MAX_READER_AUTHOR_PARAMS} reader author parameters."
        ));
    }
    if params.len() - reader_author_count > MAX_QUERY_PARAMS {
        return Err(format!(
            "Query contains more than {MAX_QUERY_PARAMS} non-author parameters."
        ));
    }
    if params
        .iter()
        .any(|(_, value)| value.len() > MAX_QUERY_VALUE_LENGTH)
    {
        return Err(format!(
            "Query parameter values may not exceed {MAX_QUERY_VALUE_LENGTH} bytes."
        ));
    }
    let dynamic_filter_count = params
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "author_id"
                    | "orcid"
                    | "author"
                    | "source_id"
                    | "issn"
                    | "journal"
                    | "topic"
                    | "concept"
                    | BIORXIV_CATEGORY_PARAM
                    | ARXIV_CATEGORY_PARAM
                    | PAPER_SOURCE_PARAM
            )
        })
        .count();
    if dynamic_filter_count > MAX_DYNAMIC_FILTER_VALUES {
        return Err(format!(
            "Query contains more than {MAX_DYNAMIC_FILTER_VALUES} dynamic filter values."
        ));
    }
    Ok(())
}

fn allow_request(
    clients: &mut HashMap<IpAddr, VecDeque<Instant>>,
    client_ip: IpAddr,
    now: Instant,
) -> bool {
    let window = StdDuration::from_secs(60);
    clients.retain(|_, requests| {
        requests.retain(|requested_at| now.duration_since(*requested_at) < window);
        !requests.is_empty()
    });
    if !clients.contains_key(&client_ip) && clients.len() >= MAX_RATE_LIMIT_CLIENTS {
        return false;
    }
    let requests = clients.entry(client_ip).or_default();
    if requests.len() >= MAX_REQUESTS_PER_MINUTE {
        return false;
    }
    requests.push_back(now);
    true
}

fn trust_proxy_client_ip() -> bool {
    env::var("GSRF_TRUST_PROXY_CLIENT_IP")
        .ok()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn select_client_ip(peer_ip: IpAddr, forwarded_ip: Option<IpAddr>, trust_proxy: bool) -> IpAddr {
    if trust_proxy {
        forwarded_ip.unwrap_or(peer_ip)
    } else {
        peer_ip
    }
}

/// Merge a named feed (if any) with provider-compatible ad-hoc URL params and
/// build the cache key. Returns:
/// - `Ok(Some(req))` when the provider has enough identifiers,
/// - `Ok(None)` when no identifiers were specified at all,
/// - `Err(msg)` when a named feed was requested but not found.
async fn resolve_feed_request(
    params: &[(String, String)],
    config: &Config,
    provider: Provider,
) -> Result<Option<FeedRequest>, ResolveFeedError> {
    let feed_name = first_param(params, "feed");

    // Ad-hoc params.
    let adhoc_author_ids = collect_param(params, "author_id");
    let adhoc_orcids = collect_param(params, "orcid");
    let adhoc_authors = collect_param(params, "author");
    let adhoc_source_ids = collect_param(params, "source_id");
    let adhoc_issns = collect_param(params, "issn");
    let adhoc_journals = collect_param(params, "journal");
    let adhoc_biorxiv_categories = collect_param(params, BIORXIV_CATEGORY_PARAM);
    let adhoc_arxiv_categories = collect_param(params, ARXIV_CATEGORY_PARAM);
    let selected_paper_sources = collect_param(params, PAPER_SOURCE_PARAM);
    let custom_paper_sources = !selected_paper_sources.is_empty()
        && first_param(params, PAPER_SOURCE_MODE_PARAM).as_deref() == Some(CUSTOM_PAPER_SOURCES);
    let mut adhoc_topics = collect_param(params, "topic");
    adhoc_topics.extend(collect_param(params, "concept"));
    let adhoc_from = first_param(params, "from");

    let has_adhoc = !adhoc_author_ids.is_empty()
        || !adhoc_orcids.is_empty()
        || !adhoc_authors.is_empty()
        || !adhoc_source_ids.is_empty()
        || !adhoc_issns.is_empty()
        || !adhoc_journals.is_empty();

    // Determine the feed config to use.
    let feed: Option<&FeedConfig> = match &feed_name {
        Some(name) => match config.feeds.get(name) {
            Some(f) => Some(f),
            None => {
                return Err(ResolveFeedError::InvalidRequest(format!(
                    "Unknown feed \"{name}\"."
                )))
            }
        },
        None => {
            if has_adhoc {
                None
            } else {
                // Bare request: serve the default feed if configured.
                match &config.default_feed {
                    Some(name) => match config.feeds.get(name) {
                        Some(f) => Some(f),
                        None => {
                            return Err(ResolveFeedError::InvalidRequest(format!(
                                "Configured default_feed \"{name}\" not found."
                            )))
                        }
                    },
                    None if !adhoc_biorxiv_categories.is_empty()
                        || !adhoc_arxiv_categories.is_empty()
                        || custom_paper_sources =>
                    {
                        None
                    }
                    None => return Ok(None),
                }
            }
        }
    };

    let empty = FeedConfig::default();
    let feed = feed.unwrap_or(&empty);
    let from = adhoc_from
        .or_else(|| feed.from.clone())
        .unwrap_or_else(|| default_from_date(feed.from_days.or(config.settings.from_days)));
    let core_available = !feed.people.is_empty()
        || !feed.author_ids.is_empty()
        || !feed.orcids.is_empty()
        || !feed.authors.is_empty()
        || !feed.google_scholar_authors.is_empty()
        || !feed.source_ids.is_empty()
        || !feed.issns.is_empty()
        || !feed.journals.is_empty()
        || has_adhoc;
    let include_core = !custom_paper_sources
        || selected_paper_sources
            .iter()
            .any(|source| source == CORE_PAPER_SOURCE);
    let mut available_curated_sources = feed.curated_sources.clone();
    available_curated_sources.extend(
        selected_paper_sources
            .iter()
            .filter_map(|source| source.strip_prefix(CURATED_SOURCE_PREFIX))
            .map(str::to_string),
    );
    available_curated_sources.sort();
    available_curated_sources.dedup();
    curated::validate_sources(&available_curated_sources)
        .map_err(ResolveFeedError::InvalidRequest)?;
    validate_paper_source_values(&selected_paper_sources)
        .map_err(ResolveFeedError::InvalidRequest)?;
    let mut curated_sources = if custom_paper_sources {
        selected_paper_sources
            .iter()
            .filter_map(|source| source.strip_prefix(CURATED_SOURCE_PREFIX))
            .map(str::to_string)
            .collect()
    } else {
        feed.curated_sources.clone()
    };
    curated_sources.sort();
    curated_sources.dedup();
    curated::validate_sources(&curated_sources).map_err(ResolveFeedError::InvalidRequest)?;
    let selected_biorxiv_categories = selected_paper_sources
        .iter()
        .filter_map(|source| source.strip_prefix(BIORXIV_SOURCE_PREFIX))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut biorxiv_categories = if custom_paper_sources {
        selected_biorxiv_categories.iter()
    } else {
        feed.biorxiv_categories.iter()
    }
    .chain(&adhoc_biorxiv_categories)
    .cloned()
    .collect::<Vec<_>>();
    biorxiv_categories.sort();
    biorxiv_categories.dedup();
    discovery::biorxiv::validate_categories(&biorxiv_categories)
        .map_err(ResolveFeedError::InvalidRequest)?;
    let selected_arxiv_categories = selected_paper_sources
        .iter()
        .filter_map(|source| source.strip_prefix(ARXIV_SOURCE_PREFIX))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut arxiv_categories = if custom_paper_sources {
        selected_arxiv_categories.iter()
    } else {
        feed.arxiv_categories.iter()
    }
    .chain(&adhoc_arxiv_categories)
    .cloned()
    .collect::<Vec<_>>();
    arxiv_categories.sort();
    arxiv_categories.dedup();
    discovery::arxiv::validate_categories(&arxiv_categories)
        .map_err(ResolveFeedError::InvalidRequest)?;
    let has_adhoc_native = !adhoc_biorxiv_categories.is_empty()
        || !adhoc_arxiv_categories.is_empty()
        || (custom_paper_sources
            && (!selected_biorxiv_categories.is_empty() || !selected_arxiv_categories.is_empty()));
    let native_from = if has_adhoc_native {
        let days = first_param(params, NATIVE_DAYS_PARAM)
            .map(|value| {
                value.parse::<u32>().map_err(|_| {
                    ResolveFeedError::InvalidRequest(format!(
                        "\"{NATIVE_DAYS_PARAM}\" must be a number from 1 through 30."
                    ))
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_NATIVE_FROM_DAYS);
        if !(1..=30).contains(&days) {
            return Err(ResolveFeedError::InvalidRequest(format!(
                "\"{NATIVE_DAYS_PARAM}\" must be a number from 1 through 30."
            )));
        }
        default_from_date(Some(days))
    } else {
        from.clone()
    };
    let mut tracked_author_names = feed
        .people
        .iter()
        .flat_map(|person| {
            [
                Some(person.name.as_str()),
                person.google_scholar_name.as_deref(),
            ]
        })
        .flatten()
        .chain(feed.authors.iter().map(String::as_str))
        .chain(feed.google_scholar_authors.iter().map(String::as_str))
        .chain(adhoc_authors.iter().map(String::as_str))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    tracked_author_names.sort();
    tracked_author_names.dedup();
    let people = feed.people.clone();

    if provider == Provider::GoogleScholar {
        let mut google_scholar_authors = include_core
            .then_some(())
            .into_iter()
            .flat_map(|_| feed.people.iter())
            .map(|person| {
                person
                    .google_scholar_name
                    .as_deref()
                    .unwrap_or(&person.name)
            })
            .chain(feed.google_scholar_authors.iter().map(String::as_str))
            .chain(feed.authors.iter().map(String::as_str))
            .chain(adhoc_authors.iter().map(String::as_str))
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        if !include_core {
            google_scholar_authors.clear();
        }
        google_scholar_authors.sort();
        google_scholar_authors.dedup();

        if google_scholar_authors.is_empty()
            && curated_sources.is_empty()
            && biorxiv_categories.is_empty()
            && arxiv_categories.is_empty()
            && !custom_paper_sources
        {
            return Err(ResolveFeedError::InvalidRequest(
                "The Google Scholar provider requires at least one \
                 people entry, legacy google_scholar_authors entry, ?author= parameter, \
                 or curated source."
                    .to_string(),
            ));
        }

        return Ok(Some(FeedRequest {
            provider,
            author_ids: Vec::new(),
            google_scholar_authors,
            tracked_author_names,
            people,
            curated_sources,
            biorxiv_categories,
            arxiv_categories,
            source_ids: Vec::new(),
            from,
            native_from,
            topics: Vec::new(),
            include_core,
            core_available,
            available_curated_sources,
            title: feed.title.clone(),
        }));
    }

    // Gather author identifiers from feed config + ad-hoc params.
    let mut author_ids: Vec<String> = Vec::new();
    if include_core {
        for id in feed
            .people
            .iter()
            .filter_map(|person| person.openalex_id.as_deref())
        {
            author_ids.push(normalize_id(id));
        }
        for id in feed.author_ids.iter().chain(adhoc_author_ids.iter()) {
            author_ids.push(normalize_id(id));
        }

        for orcid in feed.orcids.iter().chain(adhoc_orcids.iter()) {
            match resolve_orcid(orcid)
                .await
                .map_err(ResolveFeedError::Provider)?
            {
                Some(id) => author_ids.push(id),
                None => eprintln!("Could not resolve ORCID \"{orcid}\""),
            }
        }

        for name in feed.authors.iter().chain(adhoc_authors.iter()) {
            match resolve_author_name(name)
                .await
                .map_err(ResolveFeedError::Provider)?
            {
                Some((id, display)) => {
                    println!("Resolved author \"{name}\" -> {id} ({display})");
                    author_ids.push(id);
                }
                None => eprintln!("Could not resolve author name \"{name}\""),
            }
        }
    }

    // Normalize the id set: dedupe + sort for a stable cache key.
    author_ids.sort();
    author_ids.dedup();

    // Gather journal (source) identifiers from feed config + ad-hoc params.
    let mut source_ids: Vec<String> = Vec::new();
    if include_core {
        for id in feed.source_ids.iter().chain(adhoc_source_ids.iter()) {
            source_ids.push(normalize_id(id));
        }

        for issn in feed.issns.iter().chain(adhoc_issns.iter()) {
            match resolve_issn(issn)
                .await
                .map_err(ResolveFeedError::Provider)?
            {
                Some(id) => source_ids.push(id),
                None => eprintln!("Could not resolve ISSN \"{issn}\""),
            }
        }

        for name in feed.journals.iter().chain(adhoc_journals.iter()) {
            match resolve_journal_name(name)
                .await
                .map_err(ResolveFeedError::Provider)?
            {
                Some((id, display)) => {
                    println!("Resolved journal \"{name}\" -> {id} ({display})");
                    source_ids.push(id);
                }
                None => eprintln!("Could not resolve journal name \"{name}\""),
            }
        }
    }

    source_ids.sort();
    source_ids.dedup();

    // A feed needs at least one author or journal to produce anything.
    if author_ids.is_empty()
        && source_ids.is_empty()
        && curated_sources.is_empty()
        && biorxiv_categories.is_empty()
        && arxiv_categories.is_empty()
        && !custom_paper_sources
    {
        return Ok(None);
    }

    // Topics.
    let mut topics: Vec<String> = feed
        .topics
        .iter()
        .chain(adhoc_topics.iter())
        .map(|t| normalize_id(t))
        .collect();
    topics.sort();
    topics.dedup();

    Ok(Some(FeedRequest {
        provider,
        author_ids,
        google_scholar_authors: Vec::new(),
        tracked_author_names,
        people,
        curated_sources,
        biorxiv_categories,
        arxiv_categories,
        source_ids,
        from,
        native_from,
        topics,
        include_core,
        core_available,
        available_curated_sources,
        title: feed.title.clone(),
    }))
}

fn validate_paper_source_values(sources: &[String]) -> Result<(), String> {
    for source in sources {
        if source == CORE_PAPER_SOURCE {
            continue;
        }
        if let Some(source_name) = source.strip_prefix(CURATED_SOURCE_PREFIX) {
            curated::validate_sources(&[source_name.to_string()])?;
            continue;
        }
        if let Some(category) = source.strip_prefix(BIORXIV_SOURCE_PREFIX) {
            discovery::biorxiv::validate_categories(&[category.to_string()])?;
            continue;
        }
        if let Some(category) = source.strip_prefix(ARXIV_SOURCE_PREFIX) {
            discovery::arxiv::validate_categories(&[category.to_string()])?;
            continue;
        }
        return Err(format!("Unknown paper source \"{source}\"."));
    }
    Ok(())
}

/// Compute a YYYY-MM-DD date `from_days` (or the default window) in the past.
fn default_from_date(from_days: Option<u32>) -> String {
    let days = from_days.unwrap_or(DEFAULT_FROM_DAYS) as i64;
    let date = Utc::now().date_naive() - Duration::days(days);
    date.format("%Y-%m-%d").to_string()
}

fn mailto() -> Option<String> {
    env::var("GSRF_MAILTO")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Resolve an ORCID to a bare OpenAlex author id.
async fn resolve_orcid(orcid: &str) -> Result<Option<String>, String> {
    let orcid = orcid.trim();
    let orcid = orcid.rsplit('/').next().unwrap_or(orcid);
    let mut url = url::Url::parse(&format!("{API_BASE}/authors/https://orcid.org/{orcid}"))
        .map_err(|error| format!("Failed to build OpenAlex ORCID URL: {error}"))?;
    if let Some(m) = mailto() {
        url.query_pairs_mut().append_pair("mailto", &m);
    }
    let response = openalex::request(&CLIENT, url)?
        .send()
        .await
        .map_err(|error| format!("OpenAlex ORCID lookup failed: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let author = response
        .error_for_status()
        .map_err(|error| format!("OpenAlex ORCID lookup failed: {error}"))?
        .json::<Author>()
        .await
        .map_err(|error| format!("Failed to parse OpenAlex ORCID response: {error}"))?;
    let id = author
        .id
        .ok_or_else(|| "OpenAlex ORCID response did not contain an author ID".to_string())?;
    Ok(Some(normalize_id(&id)))
}

/// Resolve an author display name (best match) to (id, display_name).
async fn resolve_author_name(name: &str) -> Result<Option<(String, String)>, String> {
    let mut pairs = vec![
        ("search".to_string(), name.to_string()),
        ("per_page".to_string(), "1".to_string()),
    ];
    if let Some(m) = mailto() {
        pairs.push(("mailto".to_string(), m));
    }
    let url = url::Url::parse_with_params(&format!("{API_BASE}/authors"), &pairs)
        .map_err(|error| format!("Failed to build OpenAlex author search URL: {error}"))?;
    let response = openalex::request(&CLIENT, url)?
        .send()
        .await
        .map_err(|error| format!("OpenAlex author search failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("OpenAlex author search failed: {error}"))?
        .json::<AuthorsResponse>()
        .await
        .map_err(|error| format!("Failed to parse OpenAlex author search response: {error}"))?;
    let Some(author) = response.results.into_iter().next() else {
        return Ok(None);
    };
    let id = normalize_id(
        &author
            .id
            .ok_or_else(|| "OpenAlex author search result did not contain an ID".to_string())?,
    );
    let display = author.display_name.unwrap_or_else(|| id.clone());
    Ok(Some((id, display)))
}

/// Resolve an ISSN to a bare OpenAlex source id.
async fn resolve_issn(issn: &str) -> Result<Option<String>, String> {
    let issn = issn.trim();
    let mut url = url::Url::parse(&format!("{API_BASE}/sources/issn:{issn}"))
        .map_err(|error| format!("Failed to build OpenAlex ISSN URL: {error}"))?;
    if let Some(m) = mailto() {
        url.query_pairs_mut().append_pair("mailto", &m);
    }
    let response = openalex::request(&CLIENT, url)?
        .send()
        .await
        .map_err(|error| format!("OpenAlex ISSN lookup failed: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let source = response
        .error_for_status()
        .map_err(|error| format!("OpenAlex ISSN lookup failed: {error}"))?
        .json::<SourceRecord>()
        .await
        .map_err(|error| format!("Failed to parse OpenAlex ISSN response: {error}"))?;
    let id = source
        .id
        .ok_or_else(|| "OpenAlex ISSN response did not contain a source ID".to_string())?;
    Ok(Some(normalize_id(&id)))
}

/// Resolve a journal display name (best match) to (id, display_name).
async fn resolve_journal_name(name: &str) -> Result<Option<(String, String)>, String> {
    let mut pairs = vec![
        ("search".to_string(), name.to_string()),
        ("per_page".to_string(), "1".to_string()),
    ];
    if let Some(m) = mailto() {
        pairs.push(("mailto".to_string(), m));
    }
    let url = url::Url::parse_with_params(&format!("{API_BASE}/sources"), &pairs)
        .map_err(|error| format!("Failed to build OpenAlex journal search URL: {error}"))?;
    let response = openalex::request(&CLIENT, url)?
        .send()
        .await
        .map_err(|error| format!("OpenAlex journal search failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("OpenAlex journal search failed: {error}"))?
        .json::<SourcesResponse>()
        .await
        .map_err(|error| format!("Failed to parse OpenAlex journal search response: {error}"))?;
    let Some(source) = response.results.into_iter().next() else {
        return Ok(None);
    };
    let id = normalize_id(
        &source
            .id
            .ok_or_else(|| "OpenAlex journal search result did not contain an ID".to_string())?,
    );
    let display = source.display_name.unwrap_or_else(|| id.clone());
    Ok(Some((id, display)))
}

async fn generate_feed_if_needed(request: FeedRequest) -> Result<GeneratedFeed, GenerateFeedError> {
    let key = request.cache_key();
    if let Some(feed) = find_cached(&key) {
        return Ok(feed);
    }

    let build = FEED_BUILDS
        .write()
        .entry(key.clone())
        .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new()))
        .clone();
    let result = build
        .get_or_init(|| async {
            let _permit = FEED_BUILD_LIMIT
                .try_acquire()
                .map_err(|_| GenerateFeedError::Busy)?;
            build_feed(&request).await.map_err(GenerateFeedError::Build)
        })
        .await
        .clone();
    if let Ok(feed) = &result {
        insert_cached(key.clone(), feed.clone());
    }
    let mut builds = FEED_BUILDS.write();
    if builds
        .get(&key)
        .is_some_and(|current| Arc::ptr_eq(current, &build))
    {
        builds.remove(&key);
    }

    result
}

fn find_cached(key: &FeedCacheKey) -> Option<GeneratedFeed> {
    let mut channels = RSS_CHANNELS.write();
    let entry = channels.get_mut(key)?;
    entry.last_accessed = Instant::now();
    Some(entry.feed.clone())
}

fn insert_cached(key: FeedCacheKey, feed: GeneratedFeed) {
    let mut channels = RSS_CHANNELS.write();
    insert_cache_entry(
        &mut channels,
        key,
        feed,
        Instant::now(),
        MAX_GENERATED_FEEDS,
    );
}

fn insert_cache_entry(
    channels: &mut HashMap<FeedCacheKey, CachedFeed>,
    key: FeedCacheKey,
    feed: GeneratedFeed,
    now: Instant,
    capacity: usize,
) {
    if !channels.contains_key(&key) && channels.len() >= capacity {
        if let Some(oldest) = oldest_cache_key(channels) {
            channels.remove(&oldest);
        }
    }
    channels.insert(
        key,
        CachedFeed {
            feed,
            last_accessed: now,
        },
    );
}

fn oldest_cache_key(channels: &HashMap<FeedCacheKey, CachedFeed>) -> Option<FeedCacheKey> {
    channels
        .iter()
        .min_by_key(|(_, entry)| entry.last_accessed)
        .map(|(key, _)| key.clone())
}

async fn build_feed(request: &FeedRequest) -> Result<GeneratedFeed, String> {
    build_feed_with_policy(request, discovery::snapshot::RefreshPolicy::AllowStale).await
}

async fn build_feed_with_policy(
    request: &FeedRequest,
    policy: discovery::snapshot::RefreshPolicy,
) -> Result<GeneratedFeed, String> {
    println!(
        "Building RSS channel for authors [{}] journals [{}] curated sources [{}] from {}",
        request.author_ids.join(", "),
        request.source_ids.join(", "),
        request.curated_sources.join(", "),
        request.from
    );

    let (title, description) = channel_metadata(request);
    let provider_name = request.provider.display_name();

    let mut channel = ChannelBuilder::default()
        .title(title.clone())
        .link(request.provider.site_url())
        .description(description.clone())
        .language(String::from("en-US"))
        .generator(String::from("scholarly-rss-feed"))
        .ttl(String::from("60"))
        .docs(String::from("https://cyber.harvard.edu/rss/rss.html"))
        .text_input(TextInput {
            title: provider_name.to_string(),
            description: format!("Search {provider_name}"),
            name: String::from("q"),
            link: request.provider.search_url().to_string(),
        })
        .categories(vec![Category::from("Scientific Research")])
        .build();

    let works = fetch_works(request, policy).await?;
    let items = works.iter().map(work_to_item).collect::<Vec<_>>();
    channel.set_items(items);

    let now = Utc::now().to_rfc2822();
    channel.set_pub_date(now.clone());
    channel.set_last_build_date(now);

    Ok(GeneratedFeed {
        channel,
        reader: reader_feed_from_works(request, &works),
    })
}

fn reader_feed_from_works(request: &FeedRequest, works: &[Work]) -> reader::Feed {
    let (title, description) = channel_metadata(request);
    let publications = works
        .iter()
        .map(|work| work_to_publication(work, &request.people))
        .collect();

    let mut paper_sources = Vec::new();
    if request.core_available {
        paper_sources.push(reader::PaperSourceOption {
            value: CORE_PAPER_SOURCE.to_string(),
            label: "Tracked authors and journals".to_string(),
            group: "Core feed",
            selected: request.include_core,
        });
    }
    for source_name in &request.available_curated_sources {
        if let Some((label, _)) = curated::source_details(source_name) {
            paper_sources.push(reader::PaperSourceOption {
                value: format!("{CURATED_SOURCE_PREFIX}{source_name}"),
                label: label.to_string(),
                group: "Core feed",
                selected: request.curated_sources.contains(source_name),
            });
        }
    }
    paper_sources.extend(discovery::biorxiv::CATEGORIES.iter().map(|(value, label)| {
        reader::PaperSourceOption {
            value: format!("{BIORXIV_SOURCE_PREFIX}{value}"),
            label: (*label).to_string(),
            group: "bioRxiv",
            selected: request
                .biorxiv_categories
                .iter()
                .any(|category| category == value),
        }
    }));
    paper_sources.extend(discovery::arxiv::CATEGORIES.iter().map(|(value, label)| {
        reader::PaperSourceOption {
            value: format!("{ARXIV_SOURCE_PREFIX}{value}"),
            label: format!("{label} ({value})"),
            group: "arXiv",
            selected: request
                .arxiv_categories
                .iter()
                .any(|category| category == value),
        }
    }));

    reader::Feed { title, description, publications, paper_sources }
}

fn channel_metadata(request: &FeedRequest) -> (String, String) {
    let provider_name = request.provider.display_name();
    if let Some(title) = &request.title {
        return (
            title.clone(),
            format!("{title}. Recent publications parsed from {provider_name}."),
        );
    }
    let authors = match request.provider {
        Provider::OpenAlex => request.author_ids.len(),
        Provider::GoogleScholar => request.google_scholar_authors.len(),
    };
    let journals = request.source_ids.len();
    let curated_sources = request.curated_sources.len();
    let biorxiv_categories = request.biorxiv_categories.len();
    let arxiv_categories = request.arxiv_categories.len();
    let mut subjects = Vec::new();
    if authors > 0 {
        subjects.push(format!("{authors} author(s)"));
    }
    if journals > 0 {
        subjects.push(format!("{journals} journal(s)"));
    }
    if curated_sources > 0 {
        subjects.push(format!("{curated_sources} curated source(s)"));
    }
    if biorxiv_categories > 0 {
        subjects.push(format!("{biorxiv_categories} bioRxiv category/categories"));
    }
    if arxiv_categories > 0 {
        subjects.push(format!("{arxiv_categories} arXiv category/categories"));
    }
    let subject = subjects.join(" and ");

    let title = format!("Recent publications ({subject})");
    let description =
        format!("An RSS feed of recent publications for {subject}, parsed from {provider_name}.");
    (title, description)
}

async fn fetch_works(request: &FeedRequest, policy: discovery::snapshot::RefreshPolicy) -> Result<Vec<Work>, String> {
    let (provider_works, curated_works, biorxiv_works, arxiv_works) = tokio::join!(
        fetch_provider_works(request),
        curated::fetch_sources(&CLIENT, &request.curated_sources, &request.from, policy),
        discovery::biorxiv::fetch_categories(
            &CLIENT,
            &request.biorxiv_categories,
            &request.native_from,
            policy,
        ),
        discovery::arxiv::fetch_categories(
            &CLIENT,
            &request.arxiv_categories,
            &request.native_from,
            policy,
        ),
    );
    let mut provider_works = provider_works?;
    for work in &mut provider_works {
        work.add_discovery_source(request.provider.discovery_source());
    }
    let mut curated_works = curated_works?;
    let mut biorxiv_works = biorxiv_works?;
    let mut arxiv_works = arxiv_works?;
    let tracked_ids = request
        .people
        .iter()
        .filter_map(|person| person.openalex_id.as_deref())
        .map(normalize_id)
        .chain(request.author_ids.iter().cloned())
        .collect::<Vec<_>>();
    if request.provider == Provider::OpenAlex && !curated_works.is_empty() {
        curated_works = match openalex_enrichment::enrich(
            &CLIENT,
            curated_works.clone(),
            mailto().as_deref(),
            &tracked_ids,
        )
        .await
        {
            Ok(works) => works,
            Err(error) => {
                eprintln!("{error}; serving curated records without OpenAlex enrichment");
                curated_works
            }
        };
    }
    for works in [
        &mut provider_works,
        &mut curated_works,
        &mut biorxiv_works,
        &mut arxiv_works,
    ] {
        mark_feed_authors(works, &tracked_ids);
    }
    let native_works = merge_works(arxiv_works, merge_works(biorxiv_works, curated_works));
    let mut works = merge_works(provider_works, native_works);
    mark_authors_by_name(&mut works, &request.tracked_author_names);
    Ok(works)
}

async fn fetch_provider_works(request: &FeedRequest) -> Result<Vec<Work>, String> {
    match request.provider {
        Provider::OpenAlex => fetch_openalex_works(request).await,
        Provider::GoogleScholar => {
            let works = google_scholar::fetch_works(
                &CLIENT,
                &request.google_scholar_authors,
                &request.from,
            )
            .await?;
            Ok(works)
        }
    }
}

/// Fetch works for the feed as the UNION of an author query and a journal query,
/// merged, deduplicated by work id, and sorted newest-first.
async fn fetch_openalex_works(request: &FeedRequest) -> Result<Vec<Work>, String> {
    let topic_suffix = if request.topics.is_empty() {
        String::new()
    } else {
        format!(",topics.id:{}", request.topics.join("|"))
    };
    let from_suffix = format!(",from_publication_date:{}", request.from);

    let author_filter = (!request.author_ids.is_empty()).then(|| {
        format!(
            "author.id:{}{from_suffix}{topic_suffix}",
            request.author_ids.join("|")
        )
    });
    let journal_filter = (!request.source_ids.is_empty()).then(|| {
        format!(
            "primary_location.source.id:{}{from_suffix}{topic_suffix}",
            request.source_ids.join("|")
        )
    });

    // Run the (up to two) queries concurrently.
    let (author_works, journal_works) = tokio::join!(
        fetch_works_for_filter(author_filter),
        fetch_works_for_filter(journal_filter),
    );
    let mut author_works =
        author_works.map_err(|error| format!("OpenAlex author query: {error}"))?;
    let mut journal_works =
        journal_works.map_err(|error| format!("OpenAlex journal query: {error}"))?;
    mark_feed_authors(&mut author_works, &request.author_ids);
    mark_feed_authors(&mut journal_works, &request.author_ids);

    Ok(merge_works(author_works, journal_works))
}

/// Run a single `/works` query for the given filter (or return empty if `None`).
async fn fetch_works_for_filter(filter: Option<String>) -> Result<Vec<Work>, String> {
    let filter = match filter {
        Some(f) => f,
        None => return Ok(Vec::new()),
    };

    let max_works = env::var("GSRF_MAX_OPENALEX_WORKS")
        .ok()
        .map(|value| {
            value.parse::<usize>().map_err(|_| {
                format!("GSRF_MAX_OPENALEX_WORKS must be a positive integer, got \"{value}\".")
            })
        })
        .transpose()?
        .unwrap_or(DEFAULT_MAX_OPENALEX_WORKS);
    if max_works == 0 {
        return Err("GSRF_MAX_OPENALEX_WORKS must be greater than zero.".to_string());
    }
    fetch_works_for_filter_from(&CLIENT, API_BASE, filter, max_works).await
}

async fn fetch_works_for_filter_from(
    client: &reqwest::Client,
    api_base: &str,
    filter: String,
    max_works: usize,
) -> Result<Vec<Work>, String> {
    let mut works = Vec::new();
    let mut cursor = "*".to_string();
    let mut seen_cursors = HashSet::new();
    loop {
        if !seen_cursors.insert(cursor.clone()) {
            return Err("OpenAlex returned a repeated pagination cursor.".to_string());
        }
        let mut pairs = vec![
            ("filter".to_string(), filter.clone()),
            ("sort".to_string(), "publication_date:desc".to_string()),
            ("per_page".to_string(), OPENALEX_PAGE_SIZE.to_string()),
            ("cursor".to_string(), cursor),
        ];
        if let Some(m) = mailto() {
            pairs.push(("mailto".to_string(), m));
        }
        let url = url::Url::parse_with_params(&format!("{api_base}/works"), &pairs)
            .map_err(|error| format!("Failed to build OpenAlex URL: {error}"))?;
        let mut body = openalex::works_page(client, url, seen_cursors.len(), works.len()).await?;
        let next_cursor = body
            .meta
            .ok_or_else(|| format!(
                "OpenAlex works page {} did not contain pagination metadata.",
                seen_cursors.len()
            ))?
            .next_cursor;
        if works.len() + body.results.len() > max_works {
            return Err(format!(
                "OpenAlex query exceeded the configured {max_works}-work limit."
            ));
        }
        works.append(&mut body.results);
        match next_cursor {
            Some(next) => cursor = next,
            None => return Ok(works),
        }
    }
}

fn work_to_item(work: &Work) -> rss::Item {
    let link = work.best_link();
    let curated_sources = work.curated_discovery_sources().collect::<Vec<_>>();
    let native_sources = work
        .discovery_sources
        .iter()
        .filter(|source| {
            matches!(
                source.kind,
                crate::provenance::DiscoverySourceKind::Biorxiv
                    | crate::provenance::DiscoverySourceKind::Arxiv
            )
        })
        .collect::<Vec<_>>();

    let guid = link
        .clone()
        .map(|value| GuidBuilder::default().value(value).permalink(true).build());

    let source = work.venue().map(|name| Source {
        url: work
            .best_link()
            .unwrap_or_else(|| String::from("https://openalex.org")),
        title: Some(name),
    });

    let mut description = match (work.venue(), work.cited_by_count) {
        (Some(venue), Some(cites)) if cites > 0 => Some(format!("{venue} — cited {cites} times")),
        (Some(venue), _) => Some(venue),
        (None, Some(cites)) if cites > 0 => Some(format!("Cited {cites} times")),
        (None, _) => None,
    };

    if !work.alternate_links.is_empty() {
        let label = if work.alternate_links.len() == 1 {
            "Alternate version"
        } else {
            "Alternate versions"
        };
        let alternates = format!("{label}: {}", work.alternate_links.join(", "));
        description = Some(match description {
            Some(current) => format!("{current}\n{alternates}"),
            None => alternates,
        });
    }

    if let Some(pdf_url) = work.oa_pdf_url() {
        let pdf = format!("Open-access PDF: {pdf_url}");
        description = Some(match description {
            Some(current) => format!("{current}\n{pdf}"),
            None => pdf,
        });
    }

    if !curated_sources.is_empty() {
        let sources = curated_sources
            .iter()
            .map(|source| match &source.url {
                Some(url) => format!("{} ({url})", source.label),
                None => source.label.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let provenance = format!("Curated by: {sources}");
        description = Some(match description {
            Some(current) => format!("{current}\n{provenance}"),
            None => provenance,
        });
    }

    if !native_sources.is_empty() {
        let sources = native_sources
            .iter()
            .map(|source| match &source.url {
                Some(url) => format!("{} ({url})", source.label),
                None => source.label.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let provenance = format!("Preprint source: {sources}");
        description = Some(match description {
            Some(current) => format!("{current}\n{provenance}"),
            None => provenance,
        });
    }

    if work.publication_date.is_none() {
        if let Some(collection_date) = &work.collection_date {
            let added = format!(
                "Added to curated collection: {} ({})",
                collection_date.date, collection_date.commit_url
            );
            description = Some(match description {
                Some(current) => format!("{current}\n{added}"),
                None => added,
            });
        }
    }

    let author_names = work.author_names();
    let dublin_core = (!author_names.is_empty()).then(|| DublinCoreExtension {
        creators: author_names,
        ..DublinCoreExtension::default()
    });
    let category_domain = curated_sources
        .first()
        .and_then(|source| source.url.clone());
    let categories = work
        .curated_categories
        .iter()
        .map(|name| Category {
            name: name.clone(),
            domain: category_domain.clone(),
        })
        .collect::<Vec<_>>();

    ItemBuilder::default()
        .title(Some(work.best_title()))
        .description(description)
        .link(link)
        .guid(guid)
        .source(source)
        .pub_date(work.publication_date.as_deref().and_then(to_rfc2822))
        .dublin_core_ext(dublin_core)
        .categories(categories)
        .content(work.abstract_text())
        .build()
}

fn work_to_publication(work: &Work, people: &[config::PersonConfig]) -> reader::Publication {
    let authorships = work.authorships.as_deref().unwrap_or_default();
    let tracked_ids = people
        .iter()
        .filter_map(|person| person.openalex_id.as_deref())
        .chain(
            work.openalex_author_matches
                .iter()
                .map(|matched| matched.author_id.as_str()),
        )
        .map(normalize_id)
        .collect::<HashSet<_>>();
    let optional_ids = people
        .iter()
        .filter(|person| person.optional)
        .filter_map(|person| person.openalex_id.as_deref())
        .map(normalize_id)
        .filter(|id| {
            !people.iter().any(|person| {
                !person.optional
                    && person
                        .openalex_id
                        .as_deref()
                        .is_some_and(|other| normalize_id(other) == *id)
            })
        })
        .collect::<HashSet<_>>();
    let resolved_id_matches = work
        .openalex_author_matches
        .iter()
        .filter_map(|matched| {
            let configured_name = people
                .iter()
                .find(|person| {
                    person
                        .openalex_id
                        .as_deref()
                        .is_some_and(|id| normalize_id(id) == normalize_id(&matched.author_id))
                })
                .map(|person| person.name.as_str());
            resolve_openalex_author_match(authorships, matched, &tracked_ids, configured_name)
                .map(|index| (index, normalize_id(&matched.author_id)))
        })
        .collect::<Vec<_>>();
    let resolved_matches = work
        .author_matches
        .iter()
        .filter_map(|matched| {
            resolve_author_match(authorships, matched)
                .map(|index| (index, matched.queried_name.as_str()))
        })
        .collect::<Vec<_>>();
    let matched_names = work
        .matched_author_names
        .iter()
        .filter(|name| {
            !work
                .author_matches
                .iter()
                .any(|matched| &matched.matched_name == *name)
                && !work
                    .openalex_author_matches
                    .iter()
                    .any(|matched| &matched.matched_name == *name)
        })
        .map(|name| normalize_author_name(name))
        .collect::<Vec<_>>();
    let authors = authorships
        .iter()
        .enumerate()
        .filter_map(|(index, authorship)| {
            let name_and_key = authorship
                .author
                .as_ref()
                .and_then(|author| author.display_name.as_deref())
                .into_iter()
                .chain(authorship.raw_author_name.as_deref())
                .map(|name| (name, normalize_author_name(name)))
                .find(|(_, key)| !key.is_empty());
            let Some((display_name, filter_id)) = name_and_key else {
                eprintln!(
                    "Skipping unusable author byline {} for work {:?}: no alphanumeric name",
                    index + 1,
                    work.id.as_deref()
                );
                return None;
            };
            let author_names = Some(display_name)
                .into_iter()
                .chain(authorship.raw_author_name.as_deref())
                .collect::<Vec<_>>();
            let matched_queries = resolved_matches
                .iter()
                .filter_map(|(matched_index, query)| (*matched_index == index).then_some(*query))
                .collect::<Vec<_>>();
            let actual_id = authorship
                .author
                .as_ref()
                .and_then(|author| author.id.as_deref())
                .map(normalize_id);
            let direct_id = actual_id.as_ref().filter(|id| tracked_ids.contains(*id));
            let matched_ids = resolved_id_matches
                .iter()
                .filter_map(|(matched_index, id)| (*matched_index == index).then_some(id))
                .collect::<Vec<_>>();
            let matched_feed = direct_id.is_some()
                || !matched_ids.is_empty()
                || !matched_queries.is_empty()
                || author_names
                    .iter()
                    .map(|name| normalize_author_name(name))
                    .any(|name| matched_names.contains(&name));
            let optional = matched_feed
                && if let Some(id) = direct_id {
                    optional_ids.contains(id)
                } else if !matched_ids.is_empty() {
                    matched_ids.iter().all(|id| optional_ids.contains(*id))
                } else if !matched_queries.is_empty() {
                    matched_queries.iter().all(|query| {
                        let mut matching_people = people.iter().filter(|person| {
                            [
                                Some(person.name.as_str()),
                                person.google_scholar_name.as_deref(),
                            ]
                            .into_iter()
                            .flatten()
                            .any(|name| query.trim().eq_ignore_ascii_case(name.trim()))
                        });
                        matching_people.next().is_some_and(|person| person.optional)
                            && matching_people.all(|person| person.optional)
                    })
                } else {
                    let mut matching_people = people.iter().filter(|person| {
                        (actual_id.is_none() || person.openalex_id.is_none())
                            && [
                                Some(person.name.as_str()),
                                person.google_scholar_name.as_deref(),
                            ]
                            .into_iter()
                            .flatten()
                            .any(|name| {
                                let matched = openalex::AuthorMatch {
                                    queried_name: name.to_string(),
                                    matched_name: name.to_string(),
                                };
                                resolve_author_match(authorships, &matched) == Some(index)
                            })
                    });
                    matching_people.next().is_some_and(|person| person.optional)
                        && matching_people.all(|person| person.optional)
                };

            Some(reader::Author {
                name: display_name.to_string(),
                filter_id,
                matched_feed,
                optional,
            })
        })
        .collect();

    reader::Publication {
        id: work.id.clone().or_else(|| work.best_link()),
        title: work.best_title(),
        link: work.best_link(),
        pdf_url: work.oa_pdf_url(),
        publication_date: work.publication_date.clone(),
        collection_date: work.collection_date.as_ref().map(|collection_date| {
            reader::CollectionDate {
                date: collection_date.date.clone(),
                commit_url: collection_date.commit_url.clone(),
            }
        }),
        venue: work.venue(),
        authors,
        abstract_text: work.abstract_text(),
        discovery_sources: work.discovery_sources.clone(),
        curated_categories: work.curated_categories.clone(),
    }
}

/// Convert an OpenAlex `YYYY-MM-DD` date into an RFC-2822 timestamp.
fn to_rfc2822(date: &str) -> Option<String> {
    let naive = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let datetime = naive.and_time(NaiveTime::from_hms_opt(0, 0, 0)?);
    let utc = chrono::DateTime::<Utc>::from_naive_utc_and_offset(datetime, Utc);
    Some(utc.to_rfc2822())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener as StdTcpListener;

    fn work(id: Option<&str>, date: Option<&str>) -> Work {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "publication_date": date,
        }))
        .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn version_work(
        id: &str,
        doi: &str,
        title: &str,
        date: &str,
        author_ids: &[&str],
        version: &str,
        pdf_url: Option<&str>,
        has_abstract: bool,
    ) -> Work {
        let authorships = author_ids
            .iter()
            .map(|id| serde_json::json!({"author": {"id": id}}))
            .collect::<Vec<_>>();
        let best_oa_location = pdf_url.map(|url| {
            serde_json::json!({
                "pdf_url": url,
                "version": version
            })
        });
        let abstract_index = has_abstract.then(|| serde_json::json!({"Abstract": [0]}));

        serde_json::from_value(serde_json::json!({
            "id": id,
            "doi": doi,
            "title": title,
            "publication_date": date,
            "authorships": authorships,
            "primary_location": {
                "landing_page_url": doi,
                "version": version
            },
            "best_oa_location": best_oa_location,
            "abstract_inverted_index": abstract_index
        }))
        .unwrap()
    }

    fn ids(works: &[Work]) -> Vec<Option<String>> {
        works.iter().map(|w| w.id.clone()).collect()
    }

    fn empty_generated_feed() -> GeneratedFeed {
        GeneratedFeed {
            channel: Channel::default(),
            reader: reader::Feed {
                title: String::new(),
                description: String::new(),
                publications: Vec::new(),
                paper_sources: Vec::new(),
            },
        }
    }

    fn spawn_json_server(
        responses: Vec<&'static str>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        spawn_http_server(
            responses.into_iter()
                .map(|body| http_response("200 OK", "application/json", body))
                .collect(),
        )
    }

    fn http_response(status: &str, content_type: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn spawn_http_server(
        responses: Vec<String>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            responses
                .into_iter()
                .map(|response| {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream.set_read_timeout(Some(StdDuration::from_secs(5))).unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).unwrap();
                        request.push(byte[0]);
                        assert!(request.len() < 8192);
                    }
                    stream.write_all(response.as_bytes()).unwrap();
                    String::from_utf8(request).unwrap()
                })
                .collect()
        });
        (address, handle)
    }

    #[test]
    fn merge_dedupes_by_id_across_both_sets() {
        let primary = vec![
            work(Some("W1"), Some("2025-01-01")),
            work(Some("W2"), Some("2025-03-01")),
        ];
        let secondary = vec![
            work(Some("W2"), Some("2025-03-01")),
            work(Some("W3"), Some("2025-02-01")),
        ];

        let merged = merge_works(primary, secondary);

        // W2 appears once; result sorted newest-first: W2 (03), W3 (02), W1 (01).
        assert_eq!(
            ids(&merged),
            vec![
                Some("W2".to_string()),
                Some("W3".to_string()),
                Some("W1".to_string())
            ]
        );
    }

    #[test]
    fn merge_unions_matched_authors_for_identical_provider_results() {
        let mut first = work(Some("W1"), Some("2025-01-01"));
        first.matched_author_names = vec!["Ada Lovelace".to_string()];
        let mut second = work(Some("W1"), Some("2025-01-01"));
        second.matched_author_names = vec!["Grace Hopper".to_string()];

        let merged = merge_works(vec![first], vec![second]);

        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].matched_author_names,
            vec!["Ada Lovelace", "Grace Hopper"]
        );
    }

    #[test]
    fn merge_keeps_items_without_id_and_puts_missing_dates_last() {
        let primary = vec![work(None, Some("2025-05-01")), work(None, None)];
        let secondary = vec![work(Some("W9"), None)];

        let merged = merge_works(primary, secondary);

        assert_eq!(merged.len(), 3);
        // The dated item comes first; the two date-less items follow.
        assert_eq!(merged[0].publication_date.as_deref(), Some("2025-05-01"));
        assert!(merged[1].publication_date.is_none());
        assert!(merged[2].publication_date.is_none());
    }

    #[test]
    fn merge_groups_versions_and_keeps_the_richest_accessible_record() {
        let published = version_work(
            "W-published",
            "https://doi.org/10.1109/example",
            "DNAS-Bench: Deterministic Nucleic Acid Screener Benchmarking",
            "2026-05-21",
            &["A1", "A2", "A3"],
            "publishedVersion",
            None,
            false,
        );
        let preprint = version_work(
            "W-preprint",
            "https://doi.org/10.64898/example",
            "DNAS Bench — Deterministic Nucleic Acid Screener Benchmarking",
            "2026-07-20",
            &["A3", "A1", "A2"],
            "acceptedVersion",
            Some("https://example.com/preprint.pdf"),
            true,
        );

        let merged = merge_works(vec![published], vec![preprint]);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id.as_deref(), Some("W-preprint"));
        assert_eq!(
            merged[0].alternate_links,
            vec!["https://doi.org/10.1109/example"]
        );
        assert!(work_to_item(&merged[0])
            .description
            .as_deref()
            .is_some_and(|description| description.contains("Alternate version:")));
    }

    #[test]
    fn merge_keeps_same_title_with_different_authors_separate() {
        let first = version_work(
            "W1",
            "https://doi.org/10.example/one",
            "Shared title",
            "2026-05-21",
            &["A1", "A2"],
            "publishedVersion",
            None,
            false,
        );
        let second = version_work(
            "W2",
            "https://doi.org/10.example/two",
            "Shared title",
            "2026-05-22",
            &["A1", "A3"],
            "publishedVersion",
            None,
            false,
        );

        assert_eq!(merge_works(vec![first], vec![second]).len(), 2);
    }

    #[test]
    fn merge_groups_duplicate_author_entities_with_same_title() {
        // OpenAlex sometimes mints two author ids for the same person, which used to
        // defeat the author-id-only version key ("Expanding Flow Maps").
        let first: Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W7171268167",
            "title": "Expanding Flow Maps",
            "publication_date": "2026-07-23",
            "authorships": [
                {"author": {"id": "https://openalex.org/A5143604557", "display_name": "Sophia Tang"}},
                {"author": {"id": "https://openalex.org/A5016342562", "display_name": "Pranam Chatterjee"}}
            ]
        }))
        .unwrap();
        let second: Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W7170527570",
            "doi": "https://doi.org/10.48550/arxiv.2607.21585",
            "title": "Expanding Flow Maps",
            "publication_date": "2026-07-23",
            "authorships": [
                {"author": {"id": "https://openalex.org/A5143533688"}, "raw_author_name": "Tang, Sophia"},
                {"author": {"id": "https://openalex.org/A5016342562", "display_name": "Pranam Chatterjee"}}
            ]
        }))
        .unwrap();

        assert_eq!(merge_works(vec![first], vec![second]).len(), 1);
    }

    #[test]
    fn merge_groups_records_sharing_a_doi() {
        let first: Work = serde_json::from_value(serde_json::json!({
            "id": "W1",
            "doi": "https://doi.org/10.1234/Example",
            "title": "One title"
        }))
        .unwrap();
        let second: Work = serde_json::from_value(serde_json::json!({
            "id": "W2",
            "doi": "doi:10.1234/example",
            "title": "A differently recorded title"
        }))
        .unwrap();

        assert_eq!(merge_works(vec![first], vec![second]).len(), 1);
    }

    #[test]
    fn merge_groups_versions_when_only_raw_author_names_agree() {
        // chemRxiv v1/v2: display names diverge (`F Vermeire` vs `Florence H. Vermeire`)
        // while the raw names recorded on the authorship agree.
        let v1: Work = serde_json::from_value(serde_json::json!({
            "id": "W-v1",
            "doi": "https://doi.org/10.26434/chemrxiv.15003516/v1",
            "title": "QuantumPioneer: Scalable generation of quantum chemical data",
            "publication_date": "2026-05-18",
            "authorships": [
                {"author": {"id": "A5090585248", "display_name": "F Vermeire"}, "raw_author_name": "Florence Vermeire"},
                {"author": {"id": "A5051475129", "display_name": "William H. Green"}, "raw_author_name": "William H. Green"}
            ]
        }))
        .unwrap();
        let v2: Work = serde_json::from_value(serde_json::json!({
            "id": "W-v2",
            "doi": "https://doi.org/10.26434/chemrxiv.15003516/v2",
            "title": "QuantumPioneer: Scalable generation of quantum chemical data",
            "publication_date": "2026-07-12",
            "authorships": [
                {"author": {"id": "A5022062241", "display_name": "Florence H. Vermeire"}, "raw_author_name": "Florence Vermeire"},
                {"author": {"id": "A5051475129", "display_name": "William H. Green"}, "raw_author_name": "William H. Green"}
            ]
        }))
        .unwrap();

        assert_eq!(merge_works(vec![v1], vec![v2]).len(), 1);
    }

    #[test]
    fn merge_keeps_same_title_with_different_author_names_separate() {
        let first: Work = serde_json::from_value(serde_json::json!({
            "id": "W1",
            "title": "Shared title",
            "authorships": [{"author": {"display_name": "Ada Lovelace"}}]
        }))
        .unwrap();
        let second: Work = serde_json::from_value(serde_json::json!({
            "id": "W2",
            "title": "Shared title",
            "authorships": [{"author": {"display_name": "Grace Hopper"}}]
        }))
        .unwrap();

        assert_eq!(merge_works(vec![first], vec![second]).len(), 2);
    }

    #[test]
    fn author_names_normalize_order_case_and_initials() {
        assert_eq!(
            normalize_author_name("Tang, Sophia"),
            normalize_author_name("Sophia  TANG")
        );
        assert_eq!(
            normalize_author_name("S. Tang"),
            normalize_author_name("Tang")
        );
        assert_ne!(
            normalize_author_name("Ada Lovelace"),
            normalize_author_name("Grace Hopper")
        );
    }

    #[test]
    fn publication_bylines_use_valid_names_and_keep_author_associations() {
        for (display, raw, expected) in [
            (
                Some("Ada Lovelace"),
                Some("Lovelace, Ada"),
                Some("Ada Lovelace"),
            ),
            (Some(" \n "), Some("Ada Lovelace"), Some("Ada Lovelace")),
            (Some(":"), Some("Ada Lovelace"), Some("Ada Lovelace")),
            (None, Some("Ada Lovelace"), Some("Ada Lovelace")),
            (Some(":"), Some(":"), None),
            (Some(" \t "), Some(""), None),
            (None, None, None),
            (
                Some("\u{738b}\u{5c0f}\u{660e}"),
                None,
                Some("\u{738b}\u{5c0f}\u{660e}"),
            ),
            (Some("A. B."), None, Some("A. B.")),
        ] {
            let work: Work = serde_json::from_value(serde_json::json!({
                "id": "https://openalex.org/W1",
                "authorships": [
                    {"author": {"display_name": ":"}, "raw_author_name": ":"},
                    {"author": {"id": "https://openalex.org/A1", "display_name": display}, "raw_author_name": raw},
                    {"author": {"id": "https://openalex.org/A2", "display_name": "Grace Hopper"}}
                ]
            }))
            .unwrap();
            let people = [
                config::PersonConfig {
                    name: expected.unwrap_or("Ada Lovelace").to_string(),
                    openalex_id: Some("A1".to_string()),
                    google_scholar_name: None,
                    optional: true,
                },
                config::PersonConfig {
                    name: "Grace Hopper".to_string(),
                    openalex_id: Some("A2".to_string()),
                    google_scholar_name: None,
                    optional: false,
                },
            ];
            let publication = work_to_publication(&work, &people);
            assert_eq!(publication.authors.len(), 1 + usize::from(expected.is_some()));
            if let Some(expected) = expected {
                let author = &publication.authors[0];
                assert_eq!(author.name, expected);
                assert_eq!(author.filter_id, normalize_author_name(expected));
                assert!(author.matched_feed);
                assert!(author.optional);
            }
            let author = publication.authors.last().unwrap();
            assert_eq!(author.name, "Grace Hopper");
            assert!(author.matched_feed);
            assert!(!author.optional);
            assert!(publication
                .authors
                .iter()
                .all(|author| !author.filter_id.is_empty()));
        }
    }

    #[test]
    fn abstract_text_is_normalized_in_rss_and_article_output() {
        let work: Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W7169812613",
            "abstract_inverted_index": {
                "Abstract": [0],
                "The": [1],
                "rapid": [2],
                "growth": [3],
                "of": [4],
                "biotechnology": [5]
            }
        }))
        .unwrap();
        let expected = "The rapid growth of biotechnology";
        assert_eq!(work_to_item(&work).content.as_deref(), Some(expected));

        let mut feed = empty_generated_feed().reader;
        feed.publications.push(work_to_publication(&work, &[]));
        let html = reader::render_article(&feed, "https://openalex.org/W7169812613", &[])
            .unwrap();
        assert!(html.contains("<h2 id=\"abstract-heading\">Abstract</h2>"));
        assert!(html.contains(&format!("<p>{expected}</p>")));
        assert!(!html.contains("<p>Abstract The"));
        assert_eq!(
            work.abstract_inverted_index.as_ref().unwrap().get("Abstract"),
            Some(&vec![0])
        );
    }

    #[test]
    fn item_uses_dublin_core_creators_and_omits_unknown_length_enclosure() {
        let work: Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W1",
            "title": "Example",
            "authorships": [
                {"author": {"display_name": "Ada Lovelace"}},
                {"author": {}, "raw_author_name": "Grace Hopper"}
            ],
            "best_oa_location": {
                "pdf_url": "https://example.com/paper.pdf"
            }
        }))
        .unwrap();

        let item = work_to_item(&work);

        assert!(item.author.is_none());
        assert!(item.enclosure.is_none());
        assert_eq!(
            item.dublin_core_ext.unwrap().creators,
            vec!["Ada Lovelace", "Grace Hopper"]
        );
        assert!(item
            .description
            .as_deref()
            .is_some_and(|description| description.contains("https://example.com/paper.pdf")));
    }

    #[test]
    fn publication_marks_every_matching_author_without_changing_rss_categories() {
        let mut works: Vec<Work> = vec![serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W1",
            "title": "Collaborative work",
            "authorships": [
                {"author": {"id": "https://openalex.org/A1", "display_name": "Ada Lovelace"}},
                {"author": {"id": "https://openalex.org/A2"}, "raw_author_name": "Grace Hopper"},
                {"author": {"id": "https://openalex.org/A3", "display_name": "Alan Turing"}}
            ]
        }))
        .unwrap()];
        let feed_author_ids = vec!["A1".to_string(), "A2".to_string()];

        mark_feed_authors(&mut works, &feed_author_ids);
        let publication = work_to_publication(&works[0], &[]);
        let item = work_to_item(&works[0]);

        assert_eq!(
            publication
                .authors
                .iter()
                .filter(|author| author.matched_feed)
                .map(|author| author.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Ada Lovelace", "Grace Hopper"],
        );
        assert!(item.categories.is_empty());
    }

    #[test]
    fn collection_date_is_descriptive_rss_metadata_not_pub_date() {
        let mut work = work(Some("curated:1"), None);
        work.collection_date = Some(crate::openalex::CollectionDate {
            date: "2025-05-03".to_string(),
            commit_url: "https://github.com/example/repo/commit/abc".to_string(),
        });

        let item = work_to_item(&work);

        assert!(item.pub_date.is_none());
        assert!(item.description.as_deref().is_some_and(|description| {
            description.contains("Added to curated collection: 2025-05-03")
                && description.contains("/commit/abc")
        }));
    }

    #[test]
    fn merge_preserves_matched_author_from_a_discarded_version() {
        let first: Work = serde_json::from_value(serde_json::json!({
            "id": "W-original",
            "title": "A shared title",
            "authorships": [
                {"author": {"id": "A-configured", "display_name": "Sophia Tang"}}
            ]
        }))
        .unwrap();
        let richer: Work = serde_json::from_value(serde_json::json!({
            "id": "W-richer",
            "title": "A shared title",
            "authorships": [
                {"author": {"id": "A-duplicate"}, "raw_author_name": "Tang, Sophia"}
            ],
            "abstract_inverted_index": {"Abstract": [0]}
        }))
        .unwrap();
        let mut first_version = vec![first];

        mark_feed_authors(&mut first_version, &["A-configured".to_string()]);
        let merged = merge_works(first_version, vec![richer]);
        let publication = work_to_publication(&merged[0], &[]);

        assert_eq!(merged[0].id.as_deref(), Some("W-richer"));
        assert_eq!(publication.authors.len(), 1);
        assert_eq!(publication.authors[0].name, "Tang, Sophia");
        assert!(publication.authors[0].matched_feed);
    }

    #[test]
    fn provider_names_parse_with_openalex_as_the_default_spelling() {
        assert_eq!(Provider::parse("openalex").unwrap(), Provider::OpenAlex);
        assert_eq!(
            Provider::parse("google-scholar").unwrap(),
            Provider::GoogleScholar
        );
        assert_eq!(
            Provider::parse("google_scholar").unwrap(),
            Provider::GoogleScholar
        );
        assert!(Provider::parse("unknown").is_err());
    }

    fn identity_person(name: &str, id: &str, optional: bool) -> config::PersonConfig {
        config::PersonConfig {
            name: name.to_string(),
            openalex_id: Some(id.to_string()),
            google_scholar_name: None,
            optional,
        }
    }

    fn identity_work(id: &str, authors: &[(Option<&str>, &str)]) -> Work {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "doi": "https://doi.org/10.1234/identity",
            "title": "Identity regression paper",
            "authorships": authors.iter().map(|(id, name)| serde_json::json!({
                "author": {"id": id, "display_name": name},
                "raw_author_name": name,
            })).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn openalex_optional_status_does_not_override_another_configured_identity() {
        let people = [
            identity_person("John A Smith", "A1", true),
            identity_person("John B Smith", "https://openalex.org/A2", false),
        ];
        assert_eq!(
            normalize_author_name(&people[0].name),
            normalize_author_name(&people[1].name)
        );
        for name in [
            "John B Smith",
            "John A Smith",
            "Unrelated provider spelling",
        ] {
            let mut works = vec![identity_work(
                "W1",
                &[(Some("https://openalex.org/A2"), name)],
            )];
            mark_feed_authors(&mut works, &["A1".to_string(), "A2".to_string()]);
            mark_authors_by_name(
                &mut works,
                &people
                    .iter()
                    .map(|person| person.name.clone())
                    .collect::<Vec<_>>(),
            );
            let publication = work_to_publication(&works[0], &people);
            assert!(publication.authors[0].matched_feed);
            assert!(!publication.authors[0].optional, "{name}");
            let feed = reader::Feed {
                title: "Identity".to_string(),
                description: String::new(),
                publications: vec![publication],
                paper_sources: Vec::new(),
            };
            assert!(reader::render_feed(&feed, &[]).contains("Identity regression paper"));
        }
    }

    #[test]
    fn openalex_optional_identity_survives_richer_versions_and_serialization() {
        let people = [identity_person("Sophia Tang", "A1", true)];
        for (original_name, richer_name) in [
            ("Sophia Tang", "Tang, Sophia"),
            ("S Tang", "S. K. Tang"),
            ("Sophia K Tang", "Sophia Tang"),
        ] {
            for richer_id in [Some("A-duplicate"), None] {
                for richer_first in [false, true] {
                    let mut original = vec![identity_work(
                        "W-original",
                        &[(Some("https://openalex.org/A1"), original_name)],
                    )];
                    mark_feed_authors(&mut original, &["A1".to_string()]);
                    mark_feed_authors(&mut original, &["A1".to_string()]);
                    assert_eq!(original[0].openalex_author_matches.len(), 1);
                    let original_matches = original[0].openalex_author_matches.clone();
                    let mut richer = identity_work("W-richer", &[(richer_id, richer_name)]);
                    richer.abstract_text_override = Some("Richer abstract".to_string());
                    richer.best_oa_location = serde_json::from_value(serde_json::json!({
                        "pdf_url": "https://example.com/richer.pdf"
                    }))
                    .unwrap();
                    let mut merged = if richer_first {
                        merge_works(vec![richer], original)
                    } else {
                        merge_works(original, vec![richer])
                    };
                    mark_authors_by_name(&mut merged, &["Sophia Tang".to_string()]);
                    assert_eq!(merged.len(), 1);
                    assert_eq!(merged[0].id.as_deref(), Some("W-richer"));
                    let restored: Work =
                        serde_json::from_str(&serde_json::to_string(&merged[0]).unwrap()).unwrap();
                    assert_eq!(restored.openalex_author_matches, original_matches);
                    let rss_before = work_to_item(&restored);
                    let publication = work_to_publication(&restored, &people);
                    assert!(publication.authors[0].matched_feed);
                    assert!(
                        publication.authors[0].optional,
                        "{original_name} -> {richer_name}"
                    );
                    let filter_id = publication.authors[0].filter_id.clone();
                    let feed = reader::Feed {
                        title: "Identity".to_string(),
                        description: String::new(),
                        publications: vec![publication],
                        paper_sources: Vec::new(),
                    };
                    assert!(!reader::render_feed(&feed, &[]).contains("Identity regression paper"));
                    assert!(
                        reader::render_feed(&feed, &[("view_author".to_string(), filter_id)])
                            .contains("Identity regression paper")
                    );
                    assert_eq!(work_to_item(&restored), rss_before);
                }
            }
        }
    }

    #[test]
    fn openalex_replacement_cannot_inherit_another_configured_identity() {
        let people = [
            identity_person("Sophia Tang", "A1", true),
            identity_person("Sophia Tang", "A2", false),
        ];
        let mut original = vec![identity_work("W-original", &[(Some("A1"), "Sophia Tang")])];
        mark_feed_authors(&mut original, &["A1".to_string()]);
        let mut richer = identity_work(
            "W-richer",
            &[(Some("https://openalex.org/A2"), "Tang, Sophia")],
        );
        richer.best_oa_location = serde_json::from_value(serde_json::json!({
            "pdf_url": "https://example.com/richer.pdf"
        }))
        .unwrap();
        // A2 is configured but has no stored match in this incoming version.
        let merged = merge_works(original, vec![richer]);
        let publication = work_to_publication(&merged[0], &people);
        assert_eq!(merged[0].id.as_deref(), Some("W-richer"));
        assert!(publication.authors[0].matched_feed);
        assert!(!publication.authors[0].optional);
    }

    #[test]
    fn openalex_optional_associations_reject_conflicts_and_ambiguity() {
        let people = [identity_person("John A Smith", "A1", true)];
        for names in [
            vec!["John B Smith"],
            vec!["Jane A Smith"],
            vec!["J A Smith", "John A Smith"],
        ] {
            let mut works = vec![identity_work("W1", &[(Some("A1"), "J Smith")])];
            mark_feed_authors(&mut works, &["A1".to_string()]);
            works[0].authorships = identity_work(
                "W2",
                &names
                    .iter()
                    .map(|name| (Some("A-unknown"), *name))
                    .collect::<Vec<_>>(),
            )
            .authorships;
            let publication = work_to_publication(&works[0], &people);
            assert!(
                publication
                    .authors
                    .iter()
                    .all(|author| !author.matched_feed && !author.optional),
                "{names:?}"
            );
        }
    }

    #[test]
    fn swapped_given_names_do_not_inherit_optional_provider_identity() {
        let people = [identity_person("John Alan Smith", "A1", true)];
        for scholar in [false, true] {
            for (original_name, replacement_name) in [
                ("J A Smith", "A J Smith"),
                ("John Alan Smith", "Alan John Smith"),
            ] {
                let mut original = vec![identity_work(
                    "W-original",
                    &[(if scholar { None } else { Some("A1") }, original_name)],
                )];
                if scholar {
                    original[0]
                        .matched_author_names
                        .push(original_name.to_string());
                    original[0].author_matches.push(openalex::AuthorMatch {
                        queried_name: people[0].name.clone(),
                        matched_name: original_name.to_string(),
                    });
                } else {
                    mark_feed_authors(&mut original, &["A1".to_string()]);
                }
                let mut richer =
                    identity_work("W-richer", &[(Some("A-unknown"), replacement_name)]);
                richer.best_oa_location = serde_json::from_value(serde_json::json!({
                    "pdf_url": "https://example.com/richer.pdf"
                }))
                .unwrap();
                let mut merged = merge_works(original, vec![richer]);
                mark_authors_by_name(&mut merged, &[people[0].name.clone()]);
                assert_eq!(merged.len(), 1);
                assert_eq!(merged[0].id.as_deref(), Some("W-richer"));
                let publication = work_to_publication(&merged[0], &people);
                assert!(
                    !publication.authors[0].optional,
                    "{original_name} -> {replacement_name}"
                );
                let feed = reader::Feed {
                    title: "Ordered names".to_string(),
                    description: String::new(),
                    publications: vec![publication],
                    paper_sources: Vec::new(),
                };
                assert!(reader::render_feed(&feed, &[]).contains("Identity regression paper"));
            }
        }
    }

    #[test]
    fn name_only_optional_matching_preserves_initials_and_rejects_known_id_mismatches() {
        let people = [
            identity_person("John A Smith", "A1", true),
            identity_person("John B Smith", "A2", false),
        ];
        for (id, name, optional) in [
            (None, "John A Smith", true),
            (None, "John B Smith", false),
            (Some("A-unknown"), "John A Smith", false),
        ] {
            let mut works = vec![identity_work("W1", &[(id, name)])];
            mark_authors_by_name(
                &mut works,
                &people
                    .iter()
                    .map(|person| person.name.clone())
                    .collect::<Vec<_>>(),
            );
            let publication = work_to_publication(&works[0], &people);
            assert!(publication.authors[0].matched_feed);
            assert_eq!(publication.authors[0].optional, optional);
        }
    }

    #[tokio::test]
    async fn optional_people_preserve_provider_queries_and_rss_but_change_reader_defaults() {
        let mut config: Config = toml::from_str(
            r#"
default_feed = "example"
[[feeds.example.people]]
name = "Ada Lovelace"
openalex_id = "A1"
[[feeds.example.people]]
name = "Grace Hopper"
google_scholar_name = "Grace Brewster Hopper"
openalex_id = "A2"
optional = true
"#,
        )
        .unwrap();
        for provider in [Provider::OpenAlex, Provider::GoogleScholar] {
            let request = resolve_feed_request(&[], &config, provider)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(request.people.len(), 2);
            assert_eq!(
                request
                    .people
                    .iter()
                    .filter(|person| person.optional)
                    .count(),
                1
            );
            if provider == Provider::OpenAlex {
                assert_eq!(request.author_ids, ["A1", "A2"]);
            } else {
                assert_eq!(
                    request.google_scholar_authors,
                    ["Ada Lovelace", "Grace Brewster Hopper"]
                );
            }
            for (id, name) in [
                (
                    Some("https://openalex.org/A2"),
                    "Different provider spelling",
                ),
                (None, "Grace Brewster Hopper"),
                (None, "Hopper, Grace"),
            ] {
                let mut works: Vec<Work> = vec![serde_json::from_value(serde_json::json!({
                    "title": "Optional paper",
                    "authorships": [{"author": {"id": id, "display_name": name}}]
                }))
                .unwrap()];
                mark_feed_authors(&mut works, &["A2".to_string()]);
                mark_authors_by_name(&mut works, &request.tracked_author_names);
                let work = &works[0];
                let publication = work_to_publication(work, &request.people);
                assert!(publication.authors[0].matched_feed);
                assert!(publication.authors[0].optional);
                assert_eq!(work_to_item(work).title(), Some("Optional paper"));
                assert!(!work_to_publication(work, &[]).authors[0].optional);
            }
            config.feeds.get_mut("example").unwrap().people[1].optional = false;
            let selected = resolve_feed_request(&[], &config, provider)
                .await
                .unwrap()
                .unwrap();
            assert_ne!(request.cache_key(), selected.cache_key());
            assert_eq!(request.author_ids, selected.author_ids);
            assert_eq!(
                request.google_scholar_authors,
                selected.google_scholar_authors
            );
            config.feeds.get_mut("example").unwrap().people[1].optional = true;
        }
    }

    #[test]
    fn feed_titles_are_part_of_cache_identity() {
        let mut first = FeedRequest {
            provider: Provider::OpenAlex,
            author_ids: vec!["A1".to_string()],
            google_scholar_authors: Vec::new(),
            tracked_author_names: Vec::new(),
            people: Vec::new(),
            curated_sources: Vec::new(),
            biorxiv_categories: Vec::new(),
            arxiv_categories: Vec::new(),
            source_ids: Vec::new(),
            from: "2026-01-01".to_string(),
            native_from: "2026-01-01".to_string(),
            topics: Vec::new(),
            include_core: true,
            core_available: true,
            available_curated_sources: Vec::new(),
            title: Some("First title".to_string()),
        };
        let first_key = first.cache_key();
        first.title = Some("Second title".to_string());

        assert_ne!(first_key, first.cache_key());
    }

    #[test]
    fn cache_capacity_evicts_the_least_recently_used_entry() {
        let base = FeedRequest {
            provider: Provider::OpenAlex,
            author_ids: vec!["A1".to_string()],
            google_scholar_authors: Vec::new(),
            tracked_author_names: Vec::new(),
            people: Vec::new(),
            curated_sources: Vec::new(),
            biorxiv_categories: Vec::new(),
            arxiv_categories: Vec::new(),
            source_ids: Vec::new(),
            from: "2026-01-01".to_string(),
            native_from: "2026-01-01".to_string(),
            topics: Vec::new(),
            include_core: true,
            core_available: true,
            available_curated_sources: Vec::new(),
            title: None,
        };
        let mut newer = base.clone();
        newer.author_ids = vec!["A2".to_string()];
        let oldest_key = base.cache_key();
        let newer_key = newer.cache_key();
        let now = Instant::now();
        let mut cache = HashMap::from([
            (
                oldest_key.clone(),
                CachedFeed {
                    feed: empty_generated_feed(),
                    last_accessed: now - StdDuration::from_secs(2),
                },
            ),
            (
                newer_key,
                CachedFeed {
                    feed: empty_generated_feed(),
                    last_accessed: now,
                },
            ),
        ]);

        let mut newest = base.clone();
        newest.author_ids = vec!["A3".to_string()];
        let newest_key = newest.cache_key();
        insert_cache_entry(
            &mut cache,
            newest_key.clone(),
            empty_generated_feed(),
            now + StdDuration::from_secs(1),
            2,
        );

        assert_eq!(cache.len(), 2);
        assert!(!cache.contains_key(&oldest_key));
        assert!(cache.contains_key(&newest_key));
    }

    #[test]
    fn reader_author_parameters_have_separate_bounded_limits() {
        let mut params = (0..MAX_READER_AUTHOR_PARAMS)
            .map(|index| (reader::AUTHOR_PARAM.to_string(), index.to_string()))
            .collect::<Vec<_>>();
        params.extend((0..MAX_QUERY_PARAMS).map(|_| ("x".to_string(), "y".to_string())));
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&params)
            .finish();
        assert!(validate_dynamic_request(&query, &params).is_ok());
        params.push((reader::AUTHOR_PARAM.to_string(), "extra".to_string()));
        assert!(validate_dynamic_request(&query, &params)
            .unwrap_err()
            .contains("reader author parameters"));
        params.pop();
        params.push(("x".to_string(), "y".to_string()));
        assert!(validate_dynamic_request(&query, &params)
            .unwrap_err()
            .contains("non-author parameters"));

        let params = (0..MAX_READER_AUTHOR_PARAMS)
            .map(|_| (reader::AUTHOR_PARAM.to_string(), "a".repeat(32)))
            .collect::<Vec<_>>();
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&params)
            .finish();
        assert!(validate_dynamic_request(&query, &params)
            .unwrap_err()
            .contains("Query string exceeds"));
        let oversized = vec![(
            reader::AUTHOR_PARAM.to_string(),
            "a".repeat(MAX_QUERY_VALUE_LENGTH + 1),
        )];
        assert!(validate_dynamic_request("", &oversized)
            .unwrap_err()
            .contains("may not exceed"));
    }

    #[test]
    fn dynamic_request_limits_filter_count_and_value_size() {
        let too_many = (0..=MAX_DYNAMIC_FILTER_VALUES)
            .map(|index| ("author_id".to_string(), format!("A{index}")))
            .collect::<Vec<_>>();
        assert!(validate_dynamic_request("", &too_many)
            .unwrap_err()
            .contains("dynamic filter values"));

        let oversized = vec![("author".to_string(), "a".repeat(MAX_QUERY_VALUE_LENGTH + 1))];
        assert!(validate_dynamic_request("", &oversized)
            .unwrap_err()
            .contains("may not exceed"));
    }

    #[test]
    fn rate_limit_rejects_excess_requests_and_recovers_after_window() {
        let client_ip = "192.0.2.1".parse().unwrap();
        let now = Instant::now();
        let mut clients = HashMap::new();

        for _ in 0..MAX_REQUESTS_PER_MINUTE {
            assert!(allow_request(&mut clients, client_ip, now));
        }
        assert!(!allow_request(&mut clients, client_ip, now));
        assert!(allow_request(
            &mut clients,
            client_ip,
            now + StdDuration::from_secs(60)
        ));
    }

    #[test]
    fn forwarded_client_ip_requires_explicit_proxy_trust() {
        let peer_ip = "192.0.2.1".parse().unwrap();
        let forwarded_ip = "198.51.100.2".parse().unwrap();

        assert_eq!(
            select_client_ip(peer_ip, Some(forwarded_ip), false),
            peer_ip
        );
        assert_eq!(
            select_client_ip(peer_ip, Some(forwarded_ip), true),
            forwarded_ip
        );
    }

    #[tokio::test]
    async fn openalex_queries_follow_pagination_cursors() {
        let (api_base, server) = spawn_json_server(vec![
            r#"{"results":[{"id":"W1"}],"meta":{"next_cursor":"next-page"}}"#,
            r#"{"results":[{"id":"W2"}],"meta":{"next_cursor":null}}"#,
        ]);

        let works = fetch_works_for_filter_from(&CLIENT, &api_base, "author.id:A1".into(), 10)
            .await
            .unwrap();
        let requests = server.join().unwrap();

        assert_eq!(ids(&works), vec![Some("W1".into()), Some("W2".into())]);
        assert!(requests[0].contains("cursor=*"));
        assert!(requests[1].contains("cursor=next-page"));
        assert!(requests
            .iter()
            .all(|request| request.contains("per_page=100")));
    }

    #[tokio::test]
    async fn openalex_retries_interrupted_and_truncated_pages_without_advancing_the_cursor() {
        let (api_base, server) = spawn_http_server(vec![
            http_response("200 OK", "application/json",
                r#"{"results":[{"id":"W1"}],"meta":{"next_cursor":"next-page"}}"#),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 999\r\nConnection: close\r\n\r\n{\"results\":[".to_string(),
            http_response("200 OK", "application/json", r#"{"results":["#),
            http_response("200 OK", "application/json",
                r#"{"results":[{"id":"W2"}],"meta":{"next_cursor":null}}"#),
        ]);
        let works = fetch_works_for_filter_from(&CLIENT, &api_base, "author.id:A1".into(), 10)
            .await
            .unwrap();
        assert_eq!(ids(&works), vec![Some("W1".into()), Some("W2".into())]);
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests[0].contains("cursor=*"));
        assert!(requests[1].contains("cursor=next-page"));
        assert_eq!(requests[1], requests[2]);
        assert_eq!(requests[2], requests[3]);
    }

    #[tokio::test]
    async fn openalex_decode_failures_include_context_and_exhaust_retries() {
        let response = http_response("200 OK", "application/json", "{");
        let (api_base, server) = spawn_http_server(vec![response; 3]);
        let url = url::Url::parse(&format!(
            "{api_base}/works?mailto=fixture@example.org&api_key=fixture-only"
        )).unwrap();
        let error = openalex::works_page(&CLIENT, url, 7, 600).await.unwrap_err();
        assert!(error.contains("page 7 (600 records received)"), "{error}");
        assert!(error.contains("JSON Eof"));
        assert!(error.contains("HTTP 200"));
        assert!(error.contains("application/json"));
        assert!(error.contains("1 bytes received"));
        assert!(error.contains("exhausted 3 attempts"));
        assert!(!error.contains("fixture@example.org"));
        assert!(!error.contains("fixture-only"));
        assert_eq!(server.join().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn openalex_schema_html_and_http_errors_fail_without_retrying_or_dumping_bodies() {
        for (status, content_type, body, expected) in [
            ("200 OK", "application/json",
                r#"{"results":[{"id":"W1","authorships":[{"author":{"display_name":42}}]}],"meta":{"next_cursor":null}}"#,
                "results[0].authorships[0].author.display_name"),
            ("200 OK", "text/html", "<html>PRIVATE_BODY_MARKER</html>", "JSON Syntax"),
            ("200 OK", "application/json",
                r#"{"results":[],"meta":{"next_cursor":null}} trailing"#, "after the response"),
            ("429 Too Many Requests", "application/json", r#"{"error":"PRIVATE_BODY_MARKER"}"#, "rate or usage budget"),
            ("401 Unauthorized", "application/json", r#"{"error":"PRIVATE_BODY_MARKER"}"#, "authorization failed"),
            ("503 Service Unavailable", "text/html", "PRIVATE_BODY_MARKER", "503"),
        ] {
            let (api_base, server) = spawn_http_server(vec![http_response(status, content_type, body)]);
            let error = fetch_works_for_filter_from(&CLIENT, &api_base, "author.id:A1".into(), 10)
                .await
                .unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(error.contains("page 1 (0 records received)"));
            assert!(!error.contains("PRIVATE_BODY_MARKER"));
            assert_eq!(server.join().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn openalex_queries_fail_instead_of_silently_truncating() {
        let (api_base, server) = spawn_json_server(vec![
            r#"{"results":[{"id":"W1"},{"id":"W2"}],"meta":{"next_cursor":"next-page"}}"#,
        ]);

        let error = fetch_works_for_filter_from(&CLIENT, &api_base, "author.id:A1".into(), 1)
            .await
            .unwrap_err();
        server.join().unwrap();

        assert!(error.contains("configured 1-work limit"));
    }

    #[test]
    fn parses_curated_comparison_cli_mode() {
        let cli = parse_cli_args_from(
            [
                "--compare-curated",
                "bioml",
                "peldom-protein-design",
                "--config",
                "custom.toml",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        let comparison = cli.comparison.unwrap();

        assert_eq!(comparison.feed_name, "bioml");
        assert_eq!(comparison.source_name, "peldom-protein-design");
        assert_eq!(cli.config_path, PathBuf::from("custom.toml"));
    }

    #[test]
    fn rejects_incomplete_curated_comparison_cli_mode() {
        let error = parse_cli_args_from(
            ["--compare-curated", "bioml"]
                .into_iter()
                .map(str::to_string),
        )
        .err()
        .unwrap();

        assert!(error.contains("requires a feed name and curated source"));
    }

    #[tokio::test]
    async fn unified_people_resolve_for_google_scholar_without_openalex_calls() {
        let config: Config = toml::from_str(
            r#"
default_feed = "bioml"

[settings]
from_days = 365

[feeds.bioml]
title = "BioML"
google_scholar_authors = ["Legacy Author"]

[[feeds.bioml.people]]
name = "Pranam Chatterjee"
openalex_id = "A1"

[[feeds.bioml.people]]
name = "David Baker"
openalex_id = "A2"
google_scholar_name = "David W Baker"
"#,
        )
        .unwrap();

        let request = resolve_feed_request(&[], &config, Provider::GoogleScholar)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(request.provider, Provider::GoogleScholar);
        assert!(request.author_ids.is_empty());
        assert_eq!(
            request.google_scholar_authors,
            vec!["David W Baker", "Legacy Author", "Pranam Chatterjee"]
        );
        assert_eq!(
            request.tracked_author_names,
            vec![
                "David Baker",
                "David W Baker",
                "Legacy Author",
                "Pranam Chatterjee"
            ]
        );
        assert!(request.source_ids.is_empty());
    }

    #[tokio::test]
    async fn unified_people_resolve_precise_openalex_ids() {
        let config: Config = toml::from_str(
            r#"
default_feed = "bioml"

[feeds.bioml]
title = "BioML"

[[feeds.bioml.people]]
name = "Pranam Chatterjee"
openalex_id = "https://openalex.org/A5016342562"

[[feeds.bioml.people]]
name = "Scholar only"
"#,
        )
        .unwrap();

        let request = resolve_feed_request(&[], &config, Provider::OpenAlex)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(request.provider, Provider::OpenAlex);
        assert_eq!(request.author_ids, vec!["A5016342562"]);
        assert!(request.google_scholar_authors.is_empty());
        assert_eq!(
            request.tracked_author_names,
            vec!["Pranam Chatterjee", "Scholar only"]
        );
    }

    #[tokio::test]
    async fn curated_only_archive_resolves_without_provider_authors() {
        let config: Config = toml::from_str(
            r#"
[feeds.protein-design-archive]
title = "Deep Learning for Protein Design"
from = "1900-01-01"
curated_sources = ["peldom-protein-design"]
"#,
        )
        .unwrap();
        let params = vec![("feed".to_string(), "protein-design-archive".to_string())];

        let request = resolve_feed_request(&params, &config, Provider::OpenAlex)
            .await
            .unwrap()
            .unwrap();

        assert!(request.author_ids.is_empty());
        assert!(request.source_ids.is_empty());
        assert!(request.tracked_author_names.is_empty());
        assert_eq!(request.curated_sources, vec!["peldom-protein-design"]);
        assert_eq!(request.from, "1900-01-01");
    }

    #[tokio::test]
    async fn feed_specific_recency_overrides_global_default() {
        let config: Config = toml::from_str(
            r#"
[settings]
from_days = 365

[feeds.pilot]
from_days = 7
arxiv_categories = ["q-bio.BM"]
"#,
        )
        .unwrap();
        let params = vec![("feed".to_string(), "pilot".to_string())];

        let request = resolve_feed_request(&params, &config, Provider::OpenAlex)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(request.from, default_from_date(Some(7)));
    }

    #[tokio::test]
    async fn optional_native_categories_extend_the_default_feed() {
        let config: Config = toml::from_str(include_str!("../feeds.toml")).unwrap();
        let params = vec![
            (
                BIORXIV_CATEGORY_PARAM.to_string(),
                "synthetic_biology".to_string(),
            ),
            (ARXIV_CATEGORY_PARAM.to_string(), "q-bio.GN".to_string()),
            (NATIVE_DAYS_PARAM.to_string(), "14".to_string()),
        ];

        let request = resolve_feed_request(&params, &config, Provider::OpenAlex)
            .await
            .unwrap()
            .unwrap();

        assert!(!request.author_ids.is_empty());
        assert_eq!(request.biorxiv_categories, vec!["synthetic_biology"]);
        assert_eq!(request.arxiv_categories, vec!["q-bio.GN"]);
        assert_eq!(request.native_from, default_from_date(Some(14)));
        assert_eq!(request.from, default_from_date(Some(365)));
    }

    #[tokio::test]
    async fn explicit_paper_sources_can_select_only_preprints() {
        let config: Config = toml::from_str(include_str!("../feeds.toml")).unwrap();
        let params = vec![
            (
                PAPER_SOURCE_MODE_PARAM.to_string(),
                CUSTOM_PAPER_SOURCES.to_string(),
            ),
            (
                PAPER_SOURCE_PARAM.to_string(),
                "biorxiv:synthetic_biology".to_string(),
            ),
            (PAPER_SOURCE_PARAM.to_string(), "arxiv:q-bio.BM".to_string()),
        ];

        let request = resolve_feed_request(&params, &config, Provider::OpenAlex)
            .await
            .unwrap()
            .unwrap();

        assert!(!request.include_core);
        assert!(request.core_available);
        assert!(request.author_ids.is_empty());
        assert!(request.source_ids.is_empty());
        assert!(request.curated_sources.is_empty());
        assert_eq!(request.biorxiv_categories, vec!["synthetic_biology"]);
        assert_eq!(request.arxiv_categories, vec!["q-bio.BM"]);
        assert_eq!(
            request.available_curated_sources,
            vec!["peldom-protein-design"]
        );
    }

    #[tokio::test]
    async fn explicit_paper_sources_can_select_core_and_curated_sources() {
        let config: Config = toml::from_str(include_str!("../feeds.toml")).unwrap();
        let params = vec![
            (
                PAPER_SOURCE_MODE_PARAM.to_string(),
                CUSTOM_PAPER_SOURCES.to_string(),
            ),
            (
                PAPER_SOURCE_PARAM.to_string(),
                CORE_PAPER_SOURCE.to_string(),
            ),
            (
                PAPER_SOURCE_PARAM.to_string(),
                "curated:peldom-protein-design".to_string(),
            ),
        ];

        let request = resolve_feed_request(&params, &config, Provider::OpenAlex)
            .await
            .unwrap()
            .unwrap();

        assert!(request.include_core);
        assert!(!request.author_ids.is_empty());
        assert_eq!(request.curated_sources, vec!["peldom-protein-design"]);
        assert!(request.biorxiv_categories.is_empty());
        assert!(request.arxiv_categories.is_empty());
    }

    #[tokio::test]
    async fn explicit_empty_source_selection_restores_defaults() {
        let config: Config = toml::from_str(include_str!("../feeds.toml")).unwrap();
        let params = vec![(
            PAPER_SOURCE_MODE_PARAM.to_string(),
            CUSTOM_PAPER_SOURCES.to_string(),
        )];

        let request = resolve_feed_request(&params, &config, Provider::OpenAlex)
            .await
            .unwrap()
            .unwrap();

        assert!(request.include_core);
        assert!(!request.author_ids.is_empty());
        assert_eq!(request.curated_sources, vec!["peldom-protein-design"]);
        assert!(request.biorxiv_categories.is_empty());
        assert!(request.arxiv_categories.is_empty());
    }

    #[tokio::test]
    async fn rejects_unknown_explicit_paper_sources() {
        let config: Config = toml::from_str(include_str!("../feeds.toml")).unwrap();
        let params = vec![
            (
                PAPER_SOURCE_MODE_PARAM.to_string(),
                CUSTOM_PAPER_SOURCES.to_string(),
            ),
            (
                PAPER_SOURCE_PARAM.to_string(),
                "repository:unknown".to_string(),
            ),
        ];

        let error = resolve_feed_request(&params, &config, Provider::OpenAlex)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            ResolveFeedError::InvalidRequest(message)
                if message == "Unknown paper source \"repository:unknown\"."
        ));
    }

    #[tokio::test]
    async fn rejects_excessive_native_query_windows() {
        let config: Config = toml::from_str(include_str!("../feeds.toml")).unwrap();
        let params = vec![
            (ARXIV_CATEGORY_PARAM.to_string(), "q-bio.BM".to_string()),
            (NATIVE_DAYS_PARAM.to_string(), "365".to_string()),
        ];

        let error = resolve_feed_request(&params, &config, Provider::OpenAlex)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            ResolveFeedError::InvalidRequest(message)
                if message.contains("must be a number from 1 through 30")
        ));
    }

    #[test]
    fn rss_items_identify_native_preprint_categories() {
        let mut work = work(Some("arxiv:2608.30337"), Some("2026-08-31"));
        work.add_discovery_source(DiscoverySource::arxiv_category("q-bio.BM", "Biomolecules"));

        let item = work_to_item(&work);

        assert!(item.description.as_deref().is_some_and(|description| {
            description.contains(
                "Preprint source: arXiv: Biomolecules (https://arxiv.org/list/q-bio.BM/recent)",
            )
        }));
    }

    #[test]
    fn production_bioml_feed_enables_recent_curated_discovery() {
        let config: Config = toml::from_str(include_str!("../feeds.toml")).unwrap();
        let bioml = config.feeds.get("bioml").unwrap();
        let pilot = config.feeds.get("native-preprint-pilot").unwrap();
        let archive = config.feeds.get("protein-design-archive").unwrap();

        assert_eq!(bioml.curated_sources, vec!["peldom-protein-design"]);
        assert!(bioml.biorxiv_categories.is_empty());
        assert!(bioml.arxiv_categories.is_empty());
        assert!(bioml.from.is_none());
        assert_eq!(pilot.from_days, Some(7));
        assert_eq!(
            pilot.biorxiv_categories,
            vec!["synthetic_biology", "bioinformatics"]
        );
        assert_eq!(pilot.arxiv_categories, vec!["q-bio.BM", "q-bio.QM"]);
        assert_eq!(archive.curated_sources, vec!["peldom-protein-design"]);
        assert_eq!(archive.from.as_deref(), Some("1900-01-01"));
    }
}
