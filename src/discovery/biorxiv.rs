use crate::discovery::snapshot::{
    resolve_refresh, DiscoverySnapshot, RefreshBatch, RefreshOutcome, RefreshPolicy,
};
use crate::openalex::{Author, Authorship, Location, Source, Work};
use crate::provenance::DiscoverySource;
use crate::works::merge_works;
use chrono::Utc;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.biorxiv.org/details/biorxiv";
const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const OVERLAP_DAYS: u32 = 2;
const PAGE_SIZE: usize = 30;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const CATEGORIES: &[(&str, &str)] = &[
    ("biochemistry", "Biochemistry"),
    ("bioengineering", "Bioengineering"),
    ("bioinformatics", "Bioinformatics"),
    ("genomics", "Genomics"),
    ("molecular_biology", "Molecular Biology"),
    ("synthetic_biology", "Synthetic Biology"),
    ("systems_biology", "Systems Biology"),
];

lazy_static! {
    static ref REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct SnapshotMetadata {
    category: String,
    entries_fetched: usize,
    covered_from: String,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    messages: Vec<ApiMessage>,
    collection: Vec<ApiWork>,
}

#[derive(Debug, Deserialize)]
struct ApiMessage {
    total: String,
}

#[derive(Debug, Deserialize)]
struct ApiWork {
    doi: String,
    title: String,
    authors: String,
    date: String,
    version: String,
    license: String,
    category: String,
    jatsxml: String,
    #[serde(rename = "abstract")]
    abstract_text: String,
    published: String,
}

pub fn validate_categories(categories: &[String]) -> Result<(), String> {
    for category in categories {
        if category_label(category).is_none() {
            return Err(format!("Unknown bioRxiv category \"{category}\"."));
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
    let mut works = Vec::new();
    for category in categories {
        works = merge_works(
            works,
            fetch_category(client, category, retention_from, policy).await?,
        );
    }
    Ok(works)
}

async fn fetch_category(
    client: &reqwest::Client,
    category: &str,
    retention_from: &str,
    policy: RefreshPolicy,
) -> Result<Vec<Work>, String> {
    let _refresh_guard = REFRESH_LOCK.lock().await;
    let source_key = format!("biorxiv:{category}");
    let filename = format!("biorxiv-{category}-snapshot-v1.json");
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
    let refresh = fetch_pages(client, category, &refresh_from, &covered_through).await;
    let outcome = resolve_refresh(
        &source_key,
        SNAPSHOT_SCHEMA_VERSION,
        previous,
        refresh.map(|works| RefreshBatch {
            covered_through,
            metadata: SnapshotMetadata {
                category: category.to_string(),
                entries_fetched: works.len(),
                covered_from: snapshot_retention_from.clone(),
            },
            works,
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
            eprintln!("{error}; using the last successful {category} bioRxiv snapshot");
            snapshot.into_works_since(retention_from)
        }
    }
}

async fn fetch_pages(
    client: &reqwest::Client,
    category: &str,
    from: &str,
    through: &str,
) -> Result<Vec<Work>, String> {
    let mut cursor = 0;
    let mut works = Vec::new();

    loop {
        let response = fetch_page(client, category, from, through, cursor).await?;
        let total = response
            .messages
            .first()
            .ok_or_else(|| "bioRxiv response omitted pagination metadata".to_string())?
            .total
            .parse::<usize>()
            .map_err(|error| format!("Invalid bioRxiv result count: {error}"))?;
        let count = response.collection.len();
        works.extend(
            response
                .collection
                .into_iter()
                .map(|work| convert_work(category, work))
                .collect::<Result<Vec<_>, _>>()?,
        );
        cursor += count;
        if count == 0 || cursor >= total {
            break;
        }
        if count != PAGE_SIZE {
            return Err(format!(
                "bioRxiv pagination stopped unexpectedly at {cursor} of {total} results"
            ));
        }
    }

    Ok(works)
}

async fn fetch_page(
    client: &reqwest::Client,
    category: &str,
    from: &str,
    through: &str,
    cursor: usize,
) -> Result<ApiResponse, String> {
    let url = format!("{API_BASE}/{from}/{through}/{cursor}/json");
    let response = client
        .get(url)
        .query(&[("category", category)])
        .send()
        .await
        .map_err(|error| format!("bioRxiv {category} request failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("bioRxiv {category} request failed: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "bioRxiv {category} response exceeded the {MAX_RESPONSE_BYTES}-byte limit"
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("Failed reading bioRxiv {category} response: {error}"))?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(format!(
            "bioRxiv {category} response exceeded the {MAX_RESPONSE_BYTES}-byte limit"
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("Failed parsing bioRxiv {category} response: {error}"))
}

fn convert_work(configured_category: &str, work: ApiWork) -> Result<Work, String> {
    let category_label = category_label(configured_category).expect("category was validated");
    let reported_category = work.category.trim().to_ascii_lowercase().replace(' ', "_");
    if reported_category != configured_category {
        return Err(format!(
            "bioRxiv returned category \"{}\" for requested category \"{configured_category}\"",
            work.category
        ));
    }
    let version = work.version.trim();
    let landing_url = format!("https://www.biorxiv.org/content/{}v{version}", work.doi);
    let pdf_url = format!("{landing_url}.full.pdf");
    let published_doi = normalize_doi(&work.published);
    let mut alternate_links = vec![work.jatsxml.clone()];
    if let Some(doi) = &published_doi {
        alternate_links.push(format!("https://doi.org/{doi}"));
    }

    Ok(Work {
        id: Some(format!("biorxiv:{}v{version}", work.doi)),
        doi: Some(format!("https://doi.org/{}", work.doi)),
        title: Some(work.title),
        display_name: None,
        publication_date: Some(work.date.clone()),
        latest_version_date: Some(work.date),
        collection_date: None,
        cited_by_count: None,
        authorships: Some(parse_authors(&work.authors)),
        primary_location: Some(Location {
            landing_page_url: Some(landing_url),
            pdf_url: Some(pdf_url.clone()),
            source: Some(Source {
                display_name: Some("bioRxiv".to_string()),
            }),
            version: Some("submittedVersion".to_string()),
        }),
        best_oa_location: Some(Location {
            landing_page_url: None,
            pdf_url: Some(pdf_url),
            source: Some(Source {
                display_name: Some("bioRxiv".to_string()),
            }),
            version: Some("submittedVersion".to_string()),
        }),
        abstract_inverted_index: None,
        abstract_text_override: normalize_optional(&work.abstract_text),
        license: normalize_optional(&work.license),
        full_text_url: normalize_optional(&work.jatsxml),
        published_doi,
        alternate_links,
        matched_author_names: Vec::new(),
        author_matches: Vec::new(),
        openalex_author_matches: Vec::new(),
        discovery_sources: vec![DiscoverySource::biorxiv_category(
            configured_category,
            category_label,
        )],
        curated_categories: Vec::new(),
    })
}

fn parse_authors(authors: &str) -> Vec<Authorship> {
    authors
        .split(';')
        .map(str::trim)
        .map(|name| name.trim_end_matches('.').trim())
        .filter(|name| !name.is_empty())
        .map(|name| Authorship {
            author: Some(Author {
                id: None,
                display_name: Some(name.to_string()),
            }),
            raw_author_name: Some(name.to_string()),
        })
        .collect()
}

fn normalize_optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.eq_ignore_ascii_case("na")).then(|| value.to_string())
}

fn normalize_doi(value: &str) -> Option<String> {
    normalize_optional(value).map(|doi| {
        doi.trim_start_matches("https://doi.org/")
            .trim_start_matches("http://doi.org/")
            .trim_start_matches("doi:")
            .to_string()
    })
}

fn category_label(category: &str) -> Option<&'static str> {
    CATEGORIES
        .iter()
        .find_map(|(key, label)| (*key == category).then_some(*label))
}

#[cfg(test)]
mod tests;
