use crate::openalex::Work;
use crate::works::merge_works;
use chrono::{Duration, NaiveDate, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshPolicy {
    AllowStale,
    RequireFresh,
}

impl RefreshPolicy {
    pub fn check_failure(self, error: &str) -> Result<(), String> {
        match self {
            Self::AllowStale => Ok(()),
            Self::RequireFresh => Err(format!("Required discovery refresh failed: {error}")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DiscoverySnapshot<M> {
    source_key: String,
    schema_version: u32,
    refreshed_at: String,
    covered_through: Option<String>,
    works: Vec<Work>,
    metadata: M,
}

impl<M> DiscoverySnapshot<M> {
    pub fn new(
        source_key: impl Into<String>,
        schema_version: u32,
        covered_through: Option<String>,
        works: Vec<Work>,
        metadata: M,
    ) -> Self {
        Self {
            source_key: source_key.into(),
            schema_version,
            refreshed_at: Utc::now().to_rfc3339(),
            covered_through,
            works,
            metadata,
        }
    }

    pub fn into_parts(self) -> (Vec<Work>, M) {
        (self.works, self.metadata)
    }

    pub fn metadata(&self) -> &M {
        &self.metadata
    }

    pub fn into_works_since(self, retention_from: &str) -> Result<Vec<Work>, String> {
        let retention_from = parse_date(retention_from, "retention date")?;
        validate_work_dates(&self.works)?;
        Ok(retain_works(self.works, retention_from))
    }

    pub fn refresh_from(&self, fallback_from: &str, overlap_days: u32) -> Result<String, String> {
        let fallback = parse_date(fallback_from, "fallback refresh date")?;
        let Some(covered_through) = self.covered_through.as_deref() else {
            return Ok(fallback_from.to_string());
        };
        let covered_through = parse_date(covered_through, "snapshot coverage date")?;
        Ok(std::cmp::max(
            fallback,
            covered_through - Duration::days(overlap_days as i64),
        )
        .format("%Y-%m-%d")
        .to_string())
    }
}

impl<M: DeserializeOwned> DiscoverySnapshot<M> {
    pub fn load(
        filename: &str,
        expected_source_key: &str,
        expected_schema_version: u32,
    ) -> Result<Option<Self>, String> {
        let Some(snapshot) = crate::snapshot_store::load::<Self>(filename)? else {
            return Ok(None);
        };
        if snapshot.source_key != expected_source_key {
            return Err(format!(
                "Snapshot {filename} belongs to source \"{}\", expected \"{expected_source_key}\"",
                snapshot.source_key
            ));
        }
        if snapshot.schema_version != expected_schema_version {
            return Err(format!(
                "Snapshot {filename} uses schema version {}, expected {expected_schema_version}",
                snapshot.schema_version
            ));
        }
        Ok(Some(snapshot))
    }
}

impl<M: Serialize> DiscoverySnapshot<M> {
    pub fn save(&self, filename: &str) -> Result<(), String> {
        crate::snapshot_store::save(filename, self)
    }
}

#[derive(Debug)]
pub struct RefreshBatch<M> {
    pub covered_through: String,
    pub works: Vec<Work>,
    pub metadata: M,
}

#[derive(Debug)]
pub enum RefreshOutcome<M> {
    Updated(DiscoverySnapshot<M>),
    Stale {
        snapshot: DiscoverySnapshot<M>,
        error: String,
    },
}

impl<M> RefreshOutcome<M> {
    pub fn into_snapshot(self) -> DiscoverySnapshot<M> {
        match self {
            Self::Updated(snapshot) | Self::Stale { snapshot, .. } => snapshot,
        }
    }
}

pub fn resolve_refresh<M>(
    source_key: &str,
    schema_version: u32,
    previous: Option<DiscoverySnapshot<M>>,
    refresh: Result<RefreshBatch<M>, String>,
    retention_from: &str,
) -> Result<RefreshOutcome<M>, String> {
    match refresh {
        Ok(batch) => {
            let validation = (|| {
                let retention_from = parse_date(retention_from, "retention date")?;
                parse_date(&batch.covered_through, "refresh coverage date")?;
                validate_work_dates(&batch.works)?;
                if let Some(snapshot) = &previous {
                    validate_work_dates(&snapshot.works)?;
                }
                Ok::<_, String>(retention_from)
            })();
            let retention_from = match validation {
                Ok(date) => date,
                Err(error) => return stale_or_error(previous, error, retention_from),
            };
            let previous_works = retain_works(
                previous.map(|snapshot| snapshot.works).unwrap_or_default(),
                retention_from,
            );
            let refreshed_works = retain_works(batch.works, retention_from);
            let works = merge_works(refreshed_works, previous_works);
            Ok(RefreshOutcome::Updated(DiscoverySnapshot::new(
                source_key,
                schema_version,
                Some(batch.covered_through),
                works,
                batch.metadata,
            )))
        }
        Err(error) => stale_or_error(previous, error, retention_from),
    }
}

fn stale_or_error<M>(
    previous: Option<DiscoverySnapshot<M>>,
    error: String,
    retention_from: &str,
) -> Result<RefreshOutcome<M>, String> {
    match previous {
        Some(mut snapshot) => {
            let retention_from = parse_date(retention_from, "retention date")?;
            validate_work_dates(&snapshot.works)?;
            snapshot.works = retain_works(snapshot.works, retention_from);
            Ok(RefreshOutcome::Stale { snapshot, error })
        }
        None => Err(error),
    }
}

fn validate_work_dates(works: &[Work]) -> Result<(), String> {
    for work in works {
        for date in [
            work.latest_version_date.as_deref(),
            work.publication_date.as_deref(),
            work.collection_date.as_ref().map(|date| date.date.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            parse_date(date, "work date")?;
        }
    }
    Ok(())
}

fn retain_works(works: Vec<Work>, retention_from: NaiveDate) -> Vec<Work> {
    works
        .into_iter()
        .filter(|work| {
            work.retention_date().is_none_or(|date| {
                NaiveDate::parse_from_str(date, "%Y-%m-%d")
                    .expect("work dates are validated before retention")
                    >= retention_from
            })
        })
        .collect()
}

fn parse_date(value: &str, label: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|error| format!("Invalid {label} \"{value}\": {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::DiscoverySource;
    use serde_json::json;

    #[test]
    fn static_refresh_policy_rejects_fallback_without_changing_http_policy() {
        let previous = DiscoverySnapshot::new("source", 1, None, Vec::new(), ());
        let outcome = resolve_refresh(
            "source", 1, Some(previous), Err("upstream unavailable".to_string()), "2026-01-01",
        ).unwrap();
        match outcome {
            RefreshOutcome::Stale { error, .. } => {
                assert!(RefreshPolicy::AllowStale.check_failure(&error).is_ok());
                assert!(RefreshPolicy::RequireFresh.check_failure(&error).unwrap_err().contains("upstream unavailable"));
            }
            RefreshOutcome::Updated(_) => panic!("failed refresh must not be reported as updated"),
        }
    }

    fn work(id: &str, doi: &str, date: &str, source: DiscoverySource) -> Work {
        let mut work: Work = serde_json::from_value(json!({
            "id": id,
            "doi": doi,
            "publication_date": date
        }))
        .unwrap();
        work.add_discovery_source(source);
        work
    }

    #[test]
    fn computes_overlapping_incremental_window_without_crossing_retention_start() {
        let snapshot =
            DiscoverySnapshot::new("source", 1, Some("2026-09-01".to_string()), Vec::new(), ());

        assert_eq!(
            snapshot.refresh_from("2026-01-01", 2).unwrap(),
            "2026-08-30"
        );
        assert_eq!(
            snapshot.refresh_from("2026-08-31", 2).unwrap(),
            "2026-08-31"
        );
    }

    #[test]
    fn refresh_merges_duplicates_and_enforces_retention() {
        let previous = DiscoverySnapshot::new(
            "source",
            1,
            Some("2026-08-31".to_string()),
            vec![
                work(
                    "old",
                    "https://doi.org/10.1000/old",
                    "2025-01-01",
                    DiscoverySource::openalex(),
                ),
                work(
                    "shared-old",
                    "https://doi.org/10.1000/shared",
                    "2026-08-30",
                    DiscoverySource::openalex(),
                ),
            ],
            "old metadata",
        );
        let batch = RefreshBatch {
            covered_through: "2026-09-01".to_string(),
            works: vec![work(
                "shared-new",
                "https://doi.org/10.1000/shared",
                "2026-09-01",
                DiscoverySource::curated_collection(
                    "collection".to_string(),
                    "Collection".to_string(),
                    "https://example.com/collection".to_string(),
                ),
            )],
            metadata: "new metadata",
        };

        let outcome =
            resolve_refresh("source", 1, Some(previous), Ok(batch), "2026-01-01").unwrap();
        let snapshot = outcome.into_snapshot();

        assert_eq!(snapshot.works.len(), 1);
        assert_eq!(snapshot.metadata, "new metadata");
        assert_eq!(snapshot.works[0].discovery_sources.len(), 2);
    }

    #[test]
    fn retention_keeps_new_version_when_old_richer_version_is_expired() {
        let mut old_rich = work(
            "old-rich",
            "https://doi.org/10.1000/shared",
            "2025-12-31",
            DiscoverySource::openalex(),
        );
        old_rich.abstract_inverted_index = Some(std::collections::HashMap::from([(
            "Abstract".to_string(),
            vec![0],
        )]));
        let previous = DiscoverySnapshot::new(
            "source",
            1,
            Some("2025-12-31".to_string()),
            vec![old_rich],
            (),
        );
        let batch = RefreshBatch {
            covered_through: "2026-01-02".to_string(),
            works: vec![work(
                "new-sparse",
                "https://doi.org/10.1000/shared",
                "2026-01-02",
                DiscoverySource::openalex(),
            )],
            metadata: (),
        };

        let snapshot = resolve_refresh("source", 1, Some(previous), Ok(batch), "2026-01-01")
            .unwrap()
            .into_snapshot();

        assert_eq!(snapshot.works.len(), 1);
        assert_eq!(
            snapshot.works[0].publication_date.as_deref(),
            Some("2026-01-02")
        );
    }

    #[test]
    fn newest_version_date_survives_for_later_retention() {
        let mut old_rich = work(
            "old-rich",
            "https://doi.org/10.1000/shared",
            "2026-01-01",
            DiscoverySource::openalex(),
        );
        old_rich.abstract_inverted_index = Some(std::collections::HashMap::from([(
            "Abstract".to_string(),
            vec![0],
        )]));
        let initial = DiscoverySnapshot::new(
            "source",
            1,
            Some("2026-01-01".to_string()),
            vec![old_rich],
            (),
        );
        let first_batch = RefreshBatch {
            covered_through: "2026-08-01".to_string(),
            works: vec![work(
                "new-sparse",
                "https://doi.org/10.1000/shared",
                "2026-08-01",
                DiscoverySource::openalex(),
            )],
            metadata: (),
        };
        let first_snapshot =
            resolve_refresh("source", 1, Some(initial), Ok(first_batch), "2026-01-01")
                .unwrap()
                .into_snapshot();
        assert_eq!(
            first_snapshot.works[0].latest_version_date.as_deref(),
            Some("2026-08-01")
        );

        let second_batch = RefreshBatch {
            covered_through: "2026-09-01".to_string(),
            works: Vec::new(),
            metadata: (),
        };
        let second_snapshot = resolve_refresh(
            "source",
            1,
            Some(first_snapshot),
            Ok(second_batch),
            "2026-06-01",
        )
        .unwrap()
        .into_snapshot();

        assert_eq!(second_snapshot.works.len(), 1);
        assert_eq!(
            second_snapshot.works[0].publication_date.as_deref(),
            Some("2026-01-01")
        );
        assert_eq!(
            second_snapshot.works[0].latest_version_date.as_deref(),
            Some("2026-08-01")
        );
    }

    #[test]
    fn refresh_failure_returns_explicit_stale_outcome() {
        let previous = DiscoverySnapshot::new(
            "source",
            1,
            None,
            vec![work(
                "old",
                "https://doi.org/10.1000/old",
                "2025-01-01",
                DiscoverySource::openalex(),
            )],
            (),
        );

        let outcome = resolve_refresh(
            "source",
            1,
            Some(previous),
            Err("unavailable".to_string()),
            "2026-01-01",
        )
        .unwrap();

        match outcome {
            RefreshOutcome::Stale {
                snapshot, error, ..
            } => {
                assert_eq!(error, "unavailable");
                assert!(snapshot.works.is_empty());
            }
            RefreshOutcome::Updated(_) => panic!("expected stale snapshot"),
        }
    }

    #[test]
    fn refresh_failure_without_snapshot_is_an_error() {
        let result = resolve_refresh::<()>(
            "source",
            1,
            None,
            Err("unavailable".to_string()),
            "2026-01-01",
        );

        assert!(result.is_err_and(|error| error == "unavailable"));
    }

    #[test]
    fn rejects_invalid_batch_dates() {
        let batch = RefreshBatch {
            covered_through: "not-a-date".to_string(),
            works: Vec::new(),
            metadata: (),
        };

        let result = resolve_refresh("source", 1, None, Ok(batch), "2026-01-01");

        assert!(result.is_err_and(|error| error.contains("refresh coverage date")));
    }

    #[test]
    fn invalid_batch_returns_previous_snapshot_as_stale() {
        let previous = DiscoverySnapshot::new("source", 1, None, Vec::new(), ());
        let batch = RefreshBatch {
            covered_through: "not-a-date".to_string(),
            works: Vec::new(),
            metadata: (),
        };

        let outcome =
            resolve_refresh("source", 1, Some(previous), Ok(batch), "2026-01-01").unwrap();

        match outcome {
            RefreshOutcome::Stale { snapshot, error } => {
                assert_eq!(snapshot.source_key, "source");
                assert!(error.contains("refresh coverage date"));
            }
            RefreshOutcome::Updated(_) => panic!("expected stale snapshot"),
        }
    }

    #[test]
    fn snapshot_round_trips_source_metadata() {
        let snapshot = DiscoverySnapshot::new(
            "biorxiv:synthetic-biology",
            2,
            Some("2026-09-01".to_string()),
            Vec::new(),
            json!({"cursor": 30}),
        );

        let restored: DiscoverySnapshot<serde_json::Value> =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();

        assert_eq!(restored.source_key, "biorxiv:synthetic-biology");
        assert_eq!(restored.schema_version, 2);
        assert_eq!(restored.covered_through.as_deref(), Some("2026-09-01"));
        assert_eq!(restored.metadata, json!({"cursor": 30}));
    }
}
