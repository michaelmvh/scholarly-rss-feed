use crate::discovery::snapshot::{
    resolve_refresh, DiscoverySnapshot, RefreshBatch, RefreshOutcome, RefreshPolicy,
};
use crate::openalex::{Author, Authorship, Location, Source, Work};
use crate::provenance::DiscoverySource;
use crate::works::merge_works;
use atom_syndication::{Entry, Feed, Link};
use chrono::{DateTime, NaiveDate, Utc};
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::time::{Duration, SystemTime};
use tokio::time::Instant;

const RSS_BASE: &str = "https://rss.arxiv.org/rss";
const API_URL: &str = "https://export.arxiv.org/api/query";
const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const OVERLAP_DAYS: u32 = 2;
const ATOM_PAGE_SIZE: usize = 2_000;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const API_DELAY: Duration = Duration::from_secs(3);
const MAX_RATE_LIMIT_RETRIES: u32 = 3;
const RETRY_BACKOFF: Duration = Duration::from_secs(30);
const MAX_RETRY_WAIT: Duration = Duration::from_secs(5 * 60);
pub const CATEGORIES: &[(&str, &str)] = &[
    ("physics.bio-ph", "Biological Physics"),
    ("q-bio.BM", "Biomolecules"),
    ("q-bio.GN", "Genomics"),
    ("q-bio.MN", "Molecular Networks"),
    ("q-bio.PE", "Populations and Evolution"),
    ("q-bio.QM", "Quantitative Methods"),
];

lazy_static! {
    static ref REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
    static ref API_RATE_LIMITER: tokio::sync::Mutex<Option<Instant>> =
        tokio::sync::Mutex::new(None);
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct SnapshotMetadata {
    categories: Vec<String>,
    covered_from: String,
    atom_entries_fetched: usize,
    rss_entries_fetched: usize,
}

pub fn validate_categories(categories: &[String]) -> Result<(), String> {
    for category in categories {
        if category_label(category).is_none() {
            return Err(format!("Unknown arXiv category \"{category}\"."));
        }
    }
    Ok(())
}

pub async fn fetch_categories(
    client: &reqwest::Client,
    categories: &[String],
    retention_from: &str,
    policy: RefreshPolicy,
) -> Result<Vec<Work>, String> {
    validate_categories(categories)?;
    if categories.is_empty() {
        return Ok(Vec::new());
    }

    let _refresh_guard = REFRESH_LOCK.lock().await;
    let category_key = categories.join("+");
    let file_key = categories
        .iter()
        .map(|category| category.replace(['.', '-'], "_"))
        .collect::<Vec<_>>()
        .join("-");
    let source_key = format!("arxiv:{category_key}");
    let filename = format!("arxiv-{file_key}-snapshot-v1.json");
    let previous = DiscoverySnapshot::<SnapshotMetadata>::load(
        &filename,
        &source_key,
        SNAPSHOT_SCHEMA_VERSION,
    )?;
    let persisted_from = previous
        .as_ref()
        .map(|snapshot| snapshot.metadata().covered_from.as_str());
    let snapshot_retention_from = persisted_from
        .map(|covered_from| std::cmp::min(covered_from, retention_from))
        .unwrap_or(retention_from)
        .to_string();
    let refresh_from = match (&previous, persisted_from) {
        (Some(_), Some(covered_from)) if retention_from < covered_from => {
            retention_from.to_string()
        }
        (Some(snapshot), _) => snapshot.refresh_from(retention_from, OVERLAP_DAYS)?,
        (None, _) => retention_from.to_string(),
    };
    let covered_through = Utc::now().date_naive().format("%Y-%m-%d").to_string();
    let refresh = fetch_refresh(client, categories, &refresh_from, &covered_through).await;
    let outcome = resolve_refresh(
        &source_key,
        SNAPSHOT_SCHEMA_VERSION,
        previous,
        refresh.map(|batch| RefreshBatch {
            covered_through,
            metadata: SnapshotMetadata {
                categories: categories.to_vec(),
                covered_from: snapshot_retention_from.clone(),
                atom_entries_fetched: batch.atom_count,
                rss_entries_fetched: batch.rss_count,
            },
            works: batch.works,
        }),
        &snapshot_retention_from,
    )?;

    match outcome {
        RefreshOutcome::Updated(snapshot) => {
            snapshot.save(&filename)?;
            snapshot.into_works_since(retention_from)
        }
        RefreshOutcome::Stale { snapshot, error } => {
            policy.check_failure(&error)?;
            eprintln!("{error}; using the last successful arXiv snapshot");
            snapshot.into_works_since(retention_from)
        }
    }
}

struct FetchedBatch {
    works: Vec<Work>,
    atom_count: usize,
    rss_count: usize,
}

async fn fetch_refresh(
    client: &reqwest::Client,
    categories: &[String],
    from: &str,
    through: &str,
) -> Result<FetchedBatch, String> {
    let rss_works = fetch_rss(client, categories).await?;
    let atom_works = fetch_atom_pages(client, categories, from, through).await?;
    let rss_count = rss_works.len();
    let atom_count = atom_works.len();
    Ok(FetchedBatch {
        works: merge_works(atom_works, rss_works),
        atom_count,
        rss_count,
    })
}

async fn fetch_rss(client: &reqwest::Client, categories: &[String]) -> Result<Vec<Work>, String> {
    let url = format!("{RSS_BASE}/{}", categories.join("+"));
    let bytes = fetch_bytes(client.get(url), "arXiv RSS").await?;
    let channel = rss::Channel::read_from(Cursor::new(bytes))
        .map_err(|error| format!("Failed parsing arXiv RSS: {error}"))?;
    channel
        .items()
        .iter()
        .map(|item| convert_rss_item(item, categories))
        .collect()
}

async fn fetch_atom_pages(
    client: &reqwest::Client,
    categories: &[String],
    from: &str,
    through: &str,
) -> Result<Vec<Work>, String> {
    let query = atom_query(categories, from, through)?;
    let mut start = 0;
    let mut works = Vec::new();

    loop {
        let start_value = start.to_string();
        let page_size = ATOM_PAGE_SIZE.to_string();
        let bytes = fetch_bytes(
            client.get(API_URL).query(&[
                ("search_query", query.as_str()),
                ("start", start_value.as_str()),
                ("max_results", page_size.as_str()),
                ("sortBy", "submittedDate"),
                ("sortOrder", "descending"),
            ]),
            "arXiv Atom API",
        )
        .await?;
        let feed = Feed::read_from(Cursor::new(bytes))
            .map_err(|error| format!("Failed parsing arXiv Atom response: {error}"))?;
        let count = feed.entries().len();
        works.extend(
            feed.entries()
                .iter()
                .map(|entry| convert_atom_entry(entry, categories))
                .collect::<Result<Vec<_>, _>>()?,
        );
        start += count;
        if count < ATOM_PAGE_SIZE {
            break;
        }
    }
    Ok(works)
}

fn remaining_api_delay(last_request: Option<Instant>, now: Instant) -> Option<Duration> {
    last_request
        .and_then(|last_request| API_DELAY.checked_sub(now.duration_since(last_request)))
        .filter(|delay| !delay.is_zero())
}

async fn fetch_bytes(request: reqwest::RequestBuilder, source: &str) -> Result<Vec<u8>, String> {
    fetch_bytes_with_limiter(request, source, &API_RATE_LIMITER).await
}

async fn fetch_bytes_with_limiter(
    request: reqwest::RequestBuilder,
    source: &str,
    limiter: &tokio::sync::Mutex<Option<Instant>>,
) -> Result<Vec<u8>, String> {
    // RSS and Atom share one connection slot, including response bodies and cooldowns.
    let mut last_request = limiter.lock().await;
    let mut retries = 0;
    let mut waited = Duration::ZERO;
    let response = loop {
        if let Some(delay) = remaining_api_delay(*last_request, Instant::now()) {
            tokio::time::sleep(delay).await;
        }
        let attempt = request
            .try_clone()
            .ok_or_else(|| format!("{source} request cannot be retried safely"))?;
        *last_request = Some(Instant::now());
        let response = attempt
            .send()
            .await
            .map_err(|error| format!("{source} request failed: {error}"))?;
        if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
            break response
                .error_for_status()
                .map_err(|error| format!("{source} request failed: {error}"))?;
        }
        if retries == MAX_RATE_LIMIT_RETRIES {
            return Err(format!(
                "{source} rate limited (HTTP 429) after {} attempts; retry later",
                retries + 1
            ));
        }
        let retry_after = response.headers().get(reqwest::header::RETRY_AFTER);
        let delay = retry_delay(retry_after, retries, waited, SystemTime::now())
            .map_err(|error| format!("{source} rate limited (HTTP 429): {error}"))?;
        let reason = if retry_after.is_some() {
            "Retry-After"
        } else {
            "backoff; no Retry-After"
        };
        eprintln!(
            "{source} rate limited (HTTP 429); waiting {:.1}s ({reason}) before retry {}/{}",
            delay.as_secs_f64(),
            retries + 1,
            MAX_RATE_LIMIT_RETRIES
        );
        drop(response);
        tokio::time::sleep(delay).await;
        waited += delay;
        retries += 1;
    };
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "{source} response exceeded the {MAX_RESPONSE_BYTES}-byte limit"
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("Failed reading {source} response: {error}"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "{source} response exceeded the {MAX_RESPONSE_BYTES}-byte limit"
        ));
    }
    Ok(bytes.to_vec())
}

fn retry_delay(
    retry_after: Option<&reqwest::header::HeaderValue>,
    retries: u32,
    waited: Duration,
    now: SystemTime,
) -> Result<Duration, String> {
    let delay = match retry_after {
        Some(value) => {
            let value = value
                .to_str()
                .map_err(|_| "Invalid Retry-After header; no retry sent")?
                .trim();
            if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
                Duration::from_secs(
                    value
                        .parse()
                        .map_err(|_| "Invalid Retry-After seconds; no retry sent")?,
                )
            } else {
                httpdate::parse_http_date(value)
                    .map_err(|_| "Invalid Retry-After date; no retry sent")?
                    .duration_since(now)
                    .unwrap_or(Duration::ZERO)
            }
        }
        None => RETRY_BACKOFF * 2_u32.pow(retries),
    }
    .max(API_DELAY);
    let remaining = MAX_RETRY_WAIT.saturating_sub(waited);
    if delay > remaining {
        return Err(format!(
            "required cooldown of {:.1}s exceeds the remaining {:.1}s retry budget; \
             no retry sent, wait before running again",
            delay.as_secs_f64(),
            remaining.as_secs_f64()
        ));
    }
    Ok(delay)
}

fn atom_query(categories: &[String], from: &str, through: &str) -> Result<String, String> {
    let from = NaiveDate::parse_from_str(from, "%Y-%m-%d")
        .map_err(|error| format!("Invalid arXiv start date \"{from}\": {error}"))?;
    let through = NaiveDate::parse_from_str(through, "%Y-%m-%d")
        .map_err(|error| format!("Invalid arXiv end date \"{through}\": {error}"))?;
    let categories = categories
        .iter()
        .map(|category| format!("cat:{category}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    Ok(format!(
        "({categories}) AND submittedDate:[{}0000 TO {}2359]",
        from.format("%Y%m%d"),
        through.format("%Y%m%d")
    ))
}

fn convert_atom_entry(entry: &Entry, configured: &[String]) -> Result<Work, String> {
    let arxiv_id = parse_arxiv_id(entry.id())
        .ok_or_else(|| format!("Invalid arXiv entry id \"{}\"", entry.id()))?;
    let categories = entry
        .categories()
        .iter()
        .map(|category| category.term().to_string())
        .collect::<Vec<_>>();
    let published = entry
        .published()
        .ok_or_else(|| format!("arXiv entry {arxiv_id} omitted its publication date"))?;
    let updated = entry.updated();
    let landing = find_link(entry.links(), "text/html")
        .unwrap_or_else(|| format!("https://arxiv.org/abs/{arxiv_id}"));
    let pdf = entry
        .links()
        .iter()
        .find(|link| link.title() == Some("pdf"))
        .map(|link| link.href().to_string())
        .unwrap_or_else(|| format!("https://arxiv.org/pdf/{arxiv_id}"));
    let published_date = published.format("%Y-%m-%d").to_string();
    let updated_date = updated.format("%Y-%m-%d").to_string();

    Ok(build_work(
        &arxiv_id,
        entry.title(),
        entry.summary().map(|summary| summary.as_str()),
        entry.authors().iter().map(|author| author.name.as_str()),
        &published_date,
        &updated_date,
        &categories,
        configured,
        landing,
        pdf,
        atom_extension(entry, "license"),
        atom_extension(entry, "doi"),
    ))
}

fn convert_rss_item(item: &rss::Item, configured: &[String]) -> Result<Work, String> {
    let id_source = item
        .guid()
        .map(|guid| guid.value())
        .or_else(|| item.link())
        .ok_or_else(|| "arXiv RSS item omitted its identifier".to_string())?;
    let arxiv_id = parse_arxiv_id(id_source)
        .ok_or_else(|| format!("Invalid arXiv RSS identifier \"{id_source}\""))?;
    let categories = item
        .categories()
        .iter()
        .map(|category| category.name().to_string())
        .collect::<Vec<_>>();
    let announced = item
        .pub_date()
        .and_then(|date| DateTime::parse_from_rfc2822(date).ok())
        .map(|date| date.format("%Y-%m-%d").to_string())
        .ok_or_else(|| format!("arXiv RSS item {arxiv_id} omitted a valid date"))?;
    let published = arxiv_month_date(&arxiv_id).unwrap_or_else(|| announced.clone());
    let creators = item
        .dublin_core_ext()
        .and_then(|extension| extension.creators.first())
        .map(|creators| creators.split(',').map(str::trim).collect::<Vec<_>>())
        .unwrap_or_default();
    let description = item.description().unwrap_or_default();
    let abstract_text = description
        .split_once("Abstract:")
        .map(|(_, abstract_text)| abstract_text.trim())
        .filter(|abstract_text| !abstract_text.is_empty());
    let landing = item
        .link()
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://arxiv.org/abs/{arxiv_id}"));

    Ok(build_work(
        &arxiv_id,
        item.title().unwrap_or("Untitled"),
        abstract_text,
        creators,
        &published,
        &announced,
        &categories,
        configured,
        landing,
        format!("https://arxiv.org/pdf/{arxiv_id}"),
        item.dublin_core_ext()
            .and_then(|extension| extension.rights.first())
            .map(String::as_str),
        rss_extension(item, "DOI"),
    ))
}

#[allow(clippy::too_many_arguments)]
fn build_work<'a>(
    arxiv_id: &str,
    title: &str,
    abstract_text: Option<&str>,
    authors: impl IntoIterator<Item = &'a str>,
    published: &str,
    updated: &str,
    categories: &[String],
    configured: &[String],
    landing: String,
    pdf: String,
    license: Option<&str>,
    published_doi: Option<&str>,
) -> Work {
    let published_doi = published_doi.map(normalize_doi);
    Work {
        id: Some(format!("arxiv:{arxiv_id}")),
        doi: Some(format!("https://doi.org/10.48550/arXiv.{arxiv_id}")),
        title: Some(collapse_whitespace(title)),
        display_name: None,
        publication_date: Some(published.to_string()),
        latest_version_date: Some(updated.to_string()),
        collection_date: None,
        cited_by_count: None,
        authorships: Some(
            authors
                .into_iter()
                .filter(|name| !name.is_empty())
                .map(|name| Authorship {
                    author: Some(Author {
                        id: None,
                        display_name: Some(name.to_string()),
                    }),
                    raw_author_name: Some(name.to_string()),
                })
                .collect(),
        ),
        primary_location: Some(Location {
            landing_page_url: Some(landing),
            pdf_url: Some(pdf.clone()),
            source: Some(Source {
                display_name: Some("arXiv".to_string()),
            }),
            version: Some("submittedVersion".to_string()),
        }),
        best_oa_location: Some(Location {
            landing_page_url: None,
            pdf_url: Some(pdf),
            source: Some(Source {
                display_name: Some("arXiv".to_string()),
            }),
            version: Some("submittedVersion".to_string()),
        }),
        abstract_inverted_index: None,
        abstract_text_override: abstract_text.map(collapse_whitespace),
        license: license.map(str::to_string),
        full_text_url: None,
        published_doi: published_doi.clone(),
        alternate_links: published_doi
            .map(|doi| vec![format!("https://doi.org/{doi}")])
            .unwrap_or_default(),
        matched_author_names: Vec::new(),
        author_matches: Vec::new(),
        openalex_author_matches: Vec::new(),
        discovery_sources: configured
            .iter()
            .filter(|configured| categories.contains(configured))
            .filter_map(|category| {
                category_label(category)
                    .map(|label| DiscoverySource::arxiv_category(category, label))
            })
            .collect(),
        curated_categories: Vec::new(),
    }
}

fn parse_arxiv_id(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_start_matches("oai:arXiv.org:")
        .trim_start_matches("https://arxiv.org/abs/")
        .trim_start_matches("http://arxiv.org/abs/");
    let value = value
        .rsplit_once('v')
        .filter(|(_, version)| version.chars().all(|character| character.is_ascii_digit()))
        .map(|(id, _)| id)
        .unwrap_or(value);
    (!value.is_empty()).then(|| value.to_string())
}

fn arxiv_month_date(arxiv_id: &str) -> Option<String> {
    let numeric = arxiv_id.rsplit('/').next().unwrap_or(arxiv_id);
    let prefix = numeric.get(..4)?;
    if prefix.len() != 4 || !prefix.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    let year = prefix[..2].parse::<u32>().ok()?;
    let month = prefix[2..].parse::<u32>().ok()?;
    (1..=12).contains(&month).then(|| {
        let century = if year >= 91 { 1900 } else { 2000 };
        format!("{}-{month:02}-01", century + year)
    })
}

fn find_link(links: &[Link], mime_type: &str) -> Option<String> {
    links
        .iter()
        .find(|link| link.mime_type() == Some(mime_type))
        .map(|link| link.href().to_string())
}

fn atom_extension<'a>(entry: &'a Entry, name: &str) -> Option<&'a str> {
    entry
        .extensions()
        .get("arxiv")
        .and_then(|extensions| extensions.get(name))
        .and_then(|extensions| extensions.first())
        .and_then(|extension| extension.value.as_deref())
}

fn rss_extension<'a>(item: &'a rss::Item, name: &str) -> Option<&'a str> {
    item.extensions()
        .get("arxiv")
        .and_then(|extensions| extensions.get(name))
        .and_then(|extensions| extensions.first())
        .and_then(|extension| extension.value.as_deref())
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_doi(doi: &str) -> String {
    doi.trim()
        .trim_start_matches("https://doi.org/")
        .trim_start_matches("http://doi.org/")
        .trim_start_matches("doi:")
        .to_string()
}

fn category_label(category: &str) -> Option<&'static str> {
    CATEGORIES
        .iter()
        .find_map(|(key, label)| (*key == category).then_some(*label))
}

#[cfg(test)]
mod tests;
