use crate::openalex::{Work, WorksResponse, API_BASE};
use crate::works::{mark_feed_authors, merge_works};
use lazy_static::lazy_static;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BATCH_SIZE: usize = 40;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

lazy_static! {
    static ref CACHE: Arc<RwLock<Option<EnrichmentSnapshot>>> = Arc::new(RwLock::new(None));
    static ref REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
}

#[derive(Clone)]
struct EnrichmentSnapshot {
    checked_at: Instant,
    identifiers: HashSet<String>,
    works: Vec<Work>,
}

pub async fn enrich(
    client: &reqwest::Client,
    curated_works: Vec<Work>,
    mailto: Option<&str>,
    tracked_author_ids: &[String],
) -> Result<Vec<Work>, String> {
    let identifiers = stable_identifiers(&curated_works);
    if identifiers.is_empty() {
        return Ok(curated_works);
    }
    if let Some(works) = cached_works(&identifiers) {
        return Ok(merge_enriched(works, curated_works, tracked_author_ids));
    }

    let _refresh_guard = REFRESH.lock().await;
    if let Some(works) = cached_works(&identifiers) {
        return Ok(merge_enriched(works, curated_works, tracked_author_ids));
    }

    let mut identifiers_sorted = identifiers.iter().cloned().collect::<Vec<_>>();
    identifiers_sorted.sort();
    let mut enriched = Vec::new();
    for batch in identifiers_sorted.chunks(BATCH_SIZE) {
        enriched.extend(fetch_batch(client, batch, mailto).await?);
    }

    *CACHE.write() = Some(EnrichmentSnapshot {
        checked_at: Instant::now(),
        identifiers,
        works: enriched.clone(),
    });
    Ok(merge_enriched(enriched, curated_works, tracked_author_ids))
}

fn merge_enriched(
    mut enriched: Vec<Work>,
    mut curated: Vec<Work>,
    tracked_author_ids: &[String],
) -> Vec<Work> {
    mark_feed_authors(&mut enriched, tracked_author_ids);
    mark_feed_authors(&mut curated, tracked_author_ids);
    merge_works(enriched, curated)
}

fn cached_works(identifiers: &HashSet<String>) -> Option<Vec<Work>> {
    CACHE
        .read()
        .as_ref()
        .filter(|snapshot| {
            snapshot.checked_at.elapsed() < REFRESH_INTERVAL
                && identifiers.is_subset(&snapshot.identifiers)
        })
        .map(|snapshot| works_for_identifiers(&snapshot.works, identifiers))
}

fn works_for_identifiers(works: &[Work], identifiers: &HashSet<String>) -> Vec<Work> {
    works
        .iter()
        .filter(|work| {
            work.doi
                .as_deref()
                .map(normalize_doi)
                .is_some_and(|doi| identifiers.contains(&doi))
        })
        .cloned()
        .collect()
}

fn stable_identifiers(works: &[Work]) -> HashSet<String> {
    works
        .iter()
        .filter_map(|work| {
            work.doi
                .as_deref()
                .map(normalize_doi)
                .or_else(|| {
                    work.id
                        .as_deref()
                        .and_then(|id| id.strip_prefix("arxiv:"))
                        .map(|id| format!("10.48550/arxiv.{id}"))
                })
                .filter(|identifier| !identifier.is_empty())
        })
        .collect()
}

fn normalize_doi(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("doi.org/")
        .trim_start_matches("doi:")
        .to_string()
}

async fn fetch_batch(
    client: &reqwest::Client,
    identifiers: &[String],
    mailto: Option<&str>,
) -> Result<Vec<Work>, String> {
    let mut parameters = vec![
        (
            "filter".to_string(),
            format!("doi:{}", identifiers.join("|")),
        ),
        ("per_page".to_string(), BATCH_SIZE.to_string()),
    ];
    if let Some(mailto) = mailto {
        parameters.push(("mailto".to_string(), mailto.to_string()));
    }
    let url = url::Url::parse_with_params(&format!("{API_BASE}/works"), parameters)
        .map_err(|error| format!("Failed to build OpenAlex enrichment URL: {error}"))?;
    let mut response = crate::openalex::request(client, url)?
        .send()
        .await
        .map_err(crate::openalex::request_error)?
        .error_for_status()
        .map_err(crate::openalex::request_error)?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "OpenAlex enrichment response exceeded the {MAX_RESPONSE_BYTES}-byte limit"
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Failed reading OpenAlex enrichment response: {error}"))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(format!(
                "OpenAlex enrichment response exceeded the {MAX_RESPONSE_BYTES}-byte limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<WorksResponse>(&body)
        .map(|response| response.results)
        .map_err(|error| format!("Failed to parse OpenAlex enrichment response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_openalex_doi_filters_without_title_matching() {
        let doi_work: Work = serde_json::from_value(serde_json::json!({
            "id": "curated:doi",
            "doi": "https://doi.org/10.1000/EXAMPLE"
        }))
        .unwrap();
        let arxiv_work: Work = serde_json::from_value(serde_json::json!({
            "id": "arxiv:2605.26690"
        }))
        .unwrap();
        let no_id_work: Work = serde_json::from_value(serde_json::json!({
            "id": "curated:no-stable-id"
        }))
        .unwrap();

        assert_eq!(
            stable_identifiers(&[doi_work, arxiv_work, no_id_work]),
            HashSet::from([
                "10.1000/example".to_string(),
                "10.48550/arxiv.2605.26690".to_string()
            ])
        );
    }

    #[test]
    fn enrichment_retains_identity_before_replacing_authorships_without_mutating_cached_records() {
        let enriched: Work = serde_json::from_value(serde_json::json!({
            "id": "W1",
            "doi": "https://doi.org/10.1000/identity",
            "authorships": [{"author": {"id": "https://openalex.org/A1", "display_name": "Sophia Tang"}}]
        }))
        .unwrap();
        let curated: Work = serde_json::from_value(serde_json::json!({
            "id": "curated:richer",
            "doi": "https://doi.org/10.1000/identity",
            "authorships": [{"author": {"display_name": "Tang, Sophia"}}],
            "best_oa_location": {"pdf_url": "https://example.com/richer.pdf"}
        }))
        .unwrap();
        for tracked_ids in [vec!["A1".to_string()], Vec::new()] {
            let merged =
                merge_enriched(vec![enriched.clone()], vec![curated.clone()], &tracked_ids);
            assert_eq!(merged.len(), 1);
            assert_eq!(merged[0].id.as_deref(), Some("curated:richer"));
            assert_eq!(merged[0].openalex_author_matches.len(), tracked_ids.len());
            let people = [crate::config::PersonConfig {
                name: "Sophia Tang".to_string(),
                openalex_id: Some("A1".to_string()),
                google_scholar_name: None,
                optional: true,
            }];
            let publication = crate::work_to_publication(&merged[0], &people);
            assert_eq!(publication.authors[0].matched_feed, !tracked_ids.is_empty());
            assert_eq!(publication.authors[0].optional, !tracked_ids.is_empty());
        }
        assert!(enriched.openalex_author_matches.is_empty());
        assert!(curated.openalex_author_matches.is_empty());
    }

    #[test]
    fn cached_superset_does_not_leak_works_into_narrower_feed() {
        let first: Work = serde_json::from_value(serde_json::json!({
            "id": "W1",
            "doi": "https://doi.org/10.1000/first"
        }))
        .unwrap();
        let second: Work = serde_json::from_value(serde_json::json!({
            "id": "W2",
            "doi": "https://doi.org/10.1000/second"
        }))
        .unwrap();
        let requested = HashSet::from(["10.1000/second".to_string()]);

        let filtered = works_for_identifiers(&[first, second], &requested);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id.as_deref(), Some("W2"));
    }
}
