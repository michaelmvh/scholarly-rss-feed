use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Top-level feeds configuration, parsed from a TOML file.
#[derive(Debug, Default, Deserialize)]
pub struct Config {
    /// Name of the feed served at bare `/`.
    pub default_feed: Option<String>,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub feeds: HashMap<String, FeedConfig>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Settings {
    /// Default recency window in days when a feed omits `from`.
    pub from_days: Option<u32>,
}

/// A single named feed definition.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct FeedConfig {
    /// Optional RSS channel title override.
    pub title: Option<String>,
    /// Canonical people with provider-specific identifiers.
    #[serde(default)]
    pub people: Vec<PersonConfig>,
    /// Curated paper collections merged into this feed.
    #[serde(default)]
    pub curated_sources: Vec<String>,
    /// bioRxiv subject categories discovered through the native API.
    #[serde(default)]
    pub biorxiv_categories: Vec<String>,
    /// arXiv subject categories discovered through native feeds and API backfill.
    #[serde(default)]
    pub arxiv_categories: Vec<String>,
    /// Legacy OpenAlex author IDs.
    #[serde(default)]
    pub author_ids: Vec<String>,
    /// Legacy ORCIDs resolved to OpenAlex author IDs.
    #[serde(default)]
    pub orcids: Vec<String>,
    /// Legacy author names resolved through OpenAlex search.
    #[serde(default)]
    pub authors: Vec<String>,
    /// Legacy author names used by the archived Google Scholar provider.
    #[serde(default)]
    pub google_scholar_authors: Vec<String>,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub issns: Vec<String>,
    #[serde(default)]
    pub journals: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    /// Field-wide OpenAlex title/abstract searches, unioned with authors and journals.
    #[serde(default)]
    pub search_queries: Vec<String>,
    /// Relevance gate applied to every discovered candidate in this feed.
    pub relevance_filter: Option<RelevanceFilter>,
    /// Feed-specific rolling recency window in days.
    pub from_days: Option<u32>,
    /// Explicit earliest publication date (YYYY-MM-DD); overrides `from_days`.
    pub from: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Hash, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RelevanceFilter {
    ProteinNanopore,
}

#[derive(Debug, Clone, Deserialize, Hash, Eq, PartialEq)]
pub struct PersonConfig {
    /// Canonical human-readable name.
    pub name: String,
    /// Precise OpenAlex author ID.
    pub openalex_id: Option<String>,
    /// Optional Google Scholar query spelling; defaults to `name`.
    pub google_scholar_name: Option<String>,
    /// Include in RSS, but leave unselected in the reader by default.
    #[serde(default)]
    pub optional: bool,
}

impl Config {
    /// Load config from `path`. A missing file yields an empty config so the
    /// server still works in pure ad-hoc-param mode.
    pub fn load(path: &Path) -> Result<Config, String> {
        match std::fs::read_to_string(path) {
            Ok(contents) => toml::from_str::<Config>(&contents)
                .map_err(|error| format!("Failed to parse config {}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(error) => Err(format!("Failed to read config {}: {error}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_path(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "scholarly-rss-feed-{label}-{}-{nonce}.toml",
            std::process::id()
        ))
    }

    #[test]
    fn missing_config_supports_ad_hoc_mode() {
        let config = Config::load(&temporary_path("missing")).unwrap();

        assert!(config.default_feed.is_none());
        assert!(config.feeds.is_empty());
    }

    #[test]
    fn malformed_config_is_an_error() {
        let path = temporary_path("malformed");
        std::fs::write(&path, "[feeds.invalid\n").unwrap();

        let error = Config::load(&path).unwrap_err();
        std::fs::remove_file(path).unwrap();

        assert!(error.contains("Failed to parse config"));
    }

    #[test]
    fn people_are_selected_by_default_unless_optional() {
        let config: Config = toml::from_str(
            r#"
[[feeds.example.people]]
name = "Ada Lovelace"
[[feeds.example.people]]
name = "Grace Hopper"
optional = true
[[feeds.example.people]]
name = "Alan Turing"
optional = false
"#,
        )
        .unwrap();

        let people = &config.feeds["example"].people;
        assert!(!people[0].optional);
        assert!(people[1].optional);
        assert!(!people[2].optional);
        assert!(toml::from_str::<Config>(
            "[[feeds.example.people]]\nname = 'Ada'\noptional = 'yes'"
        )
        .is_err());
    }
}
