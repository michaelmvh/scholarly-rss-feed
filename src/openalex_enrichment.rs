use crate::openalex::{Work, API_BASE};
use crate::works::{doi_identifiers, mark_feed_authors, merge_works, normalize_doi};
use lazy_static::lazy_static;
use parking_lot::RwLock;
use serde::Deserialize;
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

#[derive(Deserialize)]
struct EnrichmentResponse {
    results: Vec<EnrichedWork>,
}

#[derive(Deserialize)]
struct EnrichedWork {
    #[serde(flatten)]
    work: Work,
    #[serde(default)]
    locations: Vec<LocationDoi>,
}

#[derive(Deserialize)]
struct LocationDoi {
    doi: Option<String>,
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
    let identifiers = stable_identifiers(&curated);
    enriched.retain(|work| {
        let matched = doi_identifiers(work)
            .iter()
            .any(|doi| identifiers.contains(doi));
        if !matched {
            eprintln!(
                "Ignoring unmatched OpenAlex enrichment record {:?}; preserving original curated records",
                work.id
            );
        }
        matched
    });
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
            doi_identifiers(work)
                .iter()
                .any(|doi| identifiers.contains(doi))
        })
        .cloned()
        .collect()
}

fn stable_identifiers(works: &[Work]) -> HashSet<String> {
    works.iter().flat_map(doi_identifiers).collect()
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
    parse_enrichment_response(&body)
}

fn parse_enrichment_response(body: &[u8]) -> Result<Vec<Work>, String> {
    let response: EnrichmentResponse = serde_json::from_slice(body)
        .map_err(|error| format!("Failed to parse OpenAlex enrichment response: {error}"))?;
    response
        .results
        .into_iter()
        .map(|mut record| {
            for doi in record.locations.into_iter().filter_map(|location| location.doi) {
                let doi = normalize_doi(&doi);
                if !doi.starts_with("10.") || !doi.contains('/') || doi.contains(char::is_whitespace) {
                    return Err(format!(
                        "Invalid OpenAlex location DOI for record {:?}",
                        record.work.id
                    ));
                }
                record.work.alternate_links.push(format!("https://doi.org/{doi}"));
            }
            record.work.alternate_links.sort();
            record.work.alternate_links.dedup();
            Ok(record.work)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::DiscoverySource;

    #[test]
    fn location_doi_aliases_preserve_curated_provenance_across_published_versions() {
        let body = serde_json::to_vec(&serde_json::json!({
            "results": [{
                "id": "https://openalex.org/W4416808855",
                "doi": "https://doi.org/10.1038/s42003-026-10497-1",
                "title": "Ovo, an open-source ecosystem for de novo protein design",
                "authorships": [{"author": {"display_name": "Published Author"}}],
                "best_oa_location": {"pdf_url": "https://example.org/published.pdf"},
                "locations": [
                    {"doi": "https://doi.org/10.1038/s42003-026-10497-1"},
                    {"doi": "https://doi.org/10.1101/2025.11.27.691041"},
                    {"doi": "https://doi.org/10.1101/2025.11.27.691041"},
                    {"doi": null}
                ]
            }]
        }))
        .unwrap();
        let enriched = parse_enrichment_response(&body).unwrap();
        assert_eq!(enriched[0].alternate_links.len(), 2);
        let mut curated: Work = serde_json::from_value(serde_json::json!({
            "id": "doi:10.1101/2025.11.27.691041",
            "doi": "https://doi.org/10.1101/2025.11.27.691041",
            "title": "A different preprint title",
            "authorships": [{"raw_author_name": "Preprint Author"}],
            "curated_categories": ["Protein design"]
        }))
        .unwrap();
        let source = DiscoverySource::curated_collection(
            "example".to_string(), "Example".to_string(), "https://example.org/".to_string(),
        );
        curated.add_discovery_source(source.clone());
        let identifiers = stable_identifiers(&[curated.clone()]);
        let cached = works_for_identifiers(&enriched, &identifiers);
        assert_eq!(cached.len(), 1);
        for candidate in [enriched, cached] {
            let merged = merge_enriched(candidate, vec![curated.clone()], &[]);
            assert_eq!(merged.len(), 1);
            assert_eq!(merged[0].id.as_deref(), Some("https://openalex.org/W4416808855"));
            assert_eq!(merged[0].discovery_sources, vec![source.clone()]);
            assert_eq!(merged[0].curated_categories, vec!["Protein design"]);
            assert!(doi_identifiers(&merged[0]).contains(&"10.1101/2025.11.27.691041".to_string()));
        }
    }

    #[test]
    fn unmatched_enrichment_cannot_add_an_unattributed_paper() {
        let curated: Work = serde_json::from_value(serde_json::json!({
            "id": "curated:original",
            "doi": "https://doi.org/10.1000/original",
            "title": "Original"
        }))
        .unwrap();
        let candidate: Work = serde_json::from_value(serde_json::json!({
            "id": "W-unrelated",
            "doi": "https://doi.org/10.1000/unrelated",
            "title": "Unrelated enrichment result"
        }))
        .unwrap();
        let merged = merge_enriched(vec![candidate], vec![curated], &[]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id.as_deref(), Some("curated:original"));
    }

    #[test]
    fn invalid_location_doi_is_reported_with_record_context() {
        let error = parse_enrichment_response(br#"{
            "results": [{"id": "W1", "locations": [{"doi": "not a doi"}]}]
        }"#).unwrap_err();
        assert!(error.contains("Invalid OpenAlex location DOI"));
        assert!(error.contains("W1"));
    }

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
