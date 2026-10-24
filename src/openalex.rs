use crate::provenance::DiscoverySource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

pub const API_BASE: &str = "https://api.openalex.org";

pub fn request(client: &reqwest::Client, url: url::Url) -> Result<reqwest::RequestBuilder, String> {
    let key = match std::env::var("GSRF_OPENALEX_API_KEY") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("GSRF_OPENALEX_API_KEY must contain valid text.".to_string());
        }
    };
    request_with_key(client, url, key.as_deref())
}

fn request_with_key(
    client: &reqwest::Client,
    url: url::Url,
    key: Option<&str>,
) -> Result<reqwest::RequestBuilder, String> {
    let official_api = url.scheme() == "https"
        && url.host_str() == Some("api.openalex.org")
        && url.port_or_known_default() == Some(443);
    let mut request = client.get(url);
    if let Some(key) = key.map(str::trim).filter(|key| !key.is_empty() && official_api) {
        let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| "GSRF_OPENALEX_API_KEY is not a valid authorization value.".to_string())?;
        authorization.set_sensitive(true);
        request = request.header(reqwest::header::AUTHORIZATION, authorization);
    }
    Ok(request)
}

pub fn request_error(error: reqwest::Error) -> String {
    match error.status() {
        Some(reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN) =>
            "OpenAlex authorization failed. Configure a verified GSRF_OPENALEX_API_KEY; cursor paging beyond the first page requires a key.".to_string(),
        Some(reqwest::StatusCode::TOO_MANY_REQUESTS) =>
            "OpenAlex rate or usage budget limit reached. Check the API key's usage before retrying.".to_string(),
        _ => format!("OpenAlex request failed: {error}"),
    }
}

struct DecodeFailure {
    message: String,
    retryable: bool,
}

fn decode_works_response(bytes: &[u8]) -> Result<WorksResponse, DecodeFailure> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let response = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        let category = error.inner().classify();
        DecodeFailure {
            message: format!(
                "JSON {category:?} error at {}: {}",
                error.path(),
                error.inner()
            ),
            retryable: error.inner().is_eof(),
        }
    })?;
    deserializer.end().map_err(|error| DecodeFailure {
        message: format!("JSON {:?} error after the response: {error}", error.classify()),
        retryable: error.is_eof(),
    })?;
    Ok(response)
}

fn error_details(error: &(dyn std::error::Error + 'static)) -> String {
    let mut details = error.to_string();
    let mut cause = error.source();
    while let Some(error) = cause {
        details.push_str(": ");
        details.push_str(&error.to_string());
        cause = error.source();
    }
    details
}

pub async fn works_page(
    client: &reqwest::Client,
    url: url::Url,
    page: usize,
    received: usize,
) -> Result<WorksResponse, String> {
    const MAX_RETRIES: u32 = 2;
    let context = format!("OpenAlex works page {page} ({received} records received)");
    for retry in 0..=MAX_RETRIES {
        let response = request(client, url.clone())?
            .send()
            .await
            .map_err(|error| format!("{context}: {}", request_error(error.without_url())))?
            .error_for_status()
            .map_err(|error| format!("{context}: {}", request_error(error.without_url())))?;
        let metadata = format!(
            "HTTP {}, content-type {:?}, content-encoding {:?}",
            response.status(),
            response.headers().get(reqwest::header::CONTENT_TYPE),
            response.headers().get(reqwest::header::CONTENT_ENCODING)
        );
        let failure = match response.bytes().await {
            Ok(bytes) => match decode_works_response(&bytes) {
                Ok(body) => return Ok(body),
                Err(mut failure) => {
                    failure.message = format!(
                        "{context}: {}; {metadata}; {} bytes received",
                        failure.message,
                        bytes.len()
                    );
                    failure
                }
            },
            Err(error) => DecodeFailure {
                message: format!(
                    "{context}: response body read failed; {metadata}; {}",
                    error_details(&error.without_url())
                ),
                retryable: true,
            },
        };
        if !failure.retryable {
            return Err(failure.message);
        }
        if retry == MAX_RETRIES {
            return Err(format!("{}; exhausted {} attempts", failure.message, retry + 1));
        }
        let delay = Duration::from_secs(2 * 2_u64.pow(retry));
        eprintln!(
            "{}; retrying the same page in {}s (retry {}/{MAX_RETRIES})",
            failure.message,
            delay.as_secs(),
            retry + 1
        );
        tokio::time::sleep(delay).await;
    }
    unreachable!("each page either succeeds or returns its final failure")
}

#[cfg(test)]
mod authentication_tests {
    use super::*;

    #[test]
    fn key_is_a_sensitive_header_not_a_query_parameter() {
        let key = "fixture-api-key";
        let request = request_with_key(
            &reqwest::Client::new(), url::Url::parse(&format!("{API_BASE}/works")).unwrap(), Some(key),
        ).unwrap().build().unwrap();
        let header = &request.headers()[reqwest::header::AUTHORIZATION];
        assert_eq!(header.to_str().unwrap(), format!("Bearer {key}"));
        assert!(header.is_sensitive());
        assert!(!request.url().as_str().contains(key));
        assert!(!format!("{request:?}").contains(key));
    }

    #[test]
    fn anonymous_requests_work_and_keys_never_go_to_other_origins() {
        for (url, key) in [
            ("https://api.openalex.org/works", None),
            ("https://api.openalex.org/works", Some("")),
            ("https://example.org/works", Some("fixture-key")),
            ("http://api.openalex.org/works", Some("fixture-key")),
            ("https://api.openalex.org:8443/works", Some("fixture-key")),
        ] {
            let request = request_with_key(&reqwest::Client::new(), url::Url::parse(url).unwrap(), key)
                .unwrap().build().unwrap();
            assert!(!request.headers().contains_key(reqwest::header::AUTHORIZATION));
        }
    }

    #[test]
    fn invalid_authorization_is_rejected_without_echoing_the_value() {
        let error = request_with_key(
            &reqwest::Client::new(), url::Url::parse(API_BASE).unwrap(), Some("fixture\nkey"),
        ).unwrap_err();
        assert!(error.contains("not a valid authorization value"));
        assert!(!error.contains("fixture"));
    }
}

/// A single author record from `/authors`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Author {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AuthorsResponse {
    pub results: Vec<Author>,
}

/// A source (journal, repository, etc.) record from `/sources`.
#[derive(Debug, Deserialize)]
pub struct SourceRecord {
    pub id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SourcesResponse {
    pub results: Vec<SourceRecord>,
}

/// A single work record from `/works`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Work {
    pub id: Option<String>,
    pub doi: Option<String>,
    pub title: Option<String>,
    pub display_name: Option<String>,
    pub publication_date: Option<String>,
    #[serde(default)]
    pub latest_version_date: Option<String>,
    #[serde(default)]
    pub collection_date: Option<CollectionDate>,
    pub cited_by_count: Option<u64>,
    pub authorships: Option<Vec<Authorship>>,
    pub primary_location: Option<Location>,
    pub best_oa_location: Option<Location>,
    pub abstract_inverted_index: Option<HashMap<String, Vec<u32>>>,
    #[serde(default)]
    pub abstract_text_override: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub full_text_url: Option<String>,
    #[serde(default)]
    pub published_doi: Option<String>,
    #[serde(default)]
    pub alternate_links: Vec<String>,
    #[serde(default)]
    pub matched_author_names: Vec<String>,
    #[serde(default)]
    pub author_matches: Vec<AuthorMatch>,
    #[serde(default)]
    pub openalex_author_matches: Vec<OpenAlexAuthorMatch>,
    #[serde(default)]
    pub discovery_sources: Vec<DiscoverySource>,
    #[serde(default)]
    pub curated_categories: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AuthorMatch {
    pub queried_name: String,
    pub matched_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct OpenAlexAuthorMatch {
    pub author_id: String,
    pub matched_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CollectionDate {
    pub date: String,
    pub commit_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Authorship {
    pub author: Option<Author>,
    pub raw_author_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Location {
    pub landing_page_url: Option<String>,
    pub pdf_url: Option<String>,
    pub source: Option<Source>,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Source {
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorksResponse {
    pub results: Vec<Work>,
    #[serde(default)]
    pub meta: Option<WorksMeta>,
}

#[derive(Debug, Deserialize)]
pub struct WorksMeta {
    pub next_cursor: Option<String>,
}

impl Work {
    pub fn add_discovery_source(&mut self, source: DiscoverySource) {
        if !self.discovery_sources.contains(&source) {
            self.discovery_sources.push(source);
            self.discovery_sources.sort();
        }
    }

    pub fn curated_discovery_sources(&self) -> impl Iterator<Item = &DiscoverySource> {
        self.discovery_sources
            .iter()
            .filter(|source| source.is_curated_collection())
    }

    pub fn retention_date(&self) -> Option<&str> {
        [
            self.latest_version_date.as_deref(),
            self.publication_date.as_deref(),
        ]
        .into_iter()
        .flatten()
        .max()
        .or_else(|| self.collection_date.as_ref().map(|date| date.date.as_str()))
    }

    /// Best available human-readable title.
    pub fn best_title(&self) -> String {
        self.title
            .clone()
            .or_else(|| self.display_name.clone())
            .unwrap_or_else(|| String::from("Untitled"))
    }

    /// Best available link for the work: DOI, then landing page, then OpenAlex id.
    pub fn best_link(&self) -> Option<String> {
        if let Some(doi) = &self.doi {
            return Some(doi.clone());
        }
        if let Some(loc) = &self.primary_location {
            if let Some(url) = &loc.landing_page_url {
                return Some(url.clone());
            }
        }
        self.id.clone()
    }

    /// Author display names.
    pub fn author_names(&self) -> Vec<String> {
        self.authorships
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|authorship| {
                authorship
                    .author
                    .as_ref()
                    .and_then(|author| author.display_name.clone())
                    .or_else(|| authorship.raw_author_name.clone())
            })
            .collect()
    }

    /// Venue (journal / repository) name.
    pub fn venue(&self) -> Option<String> {
        self.primary_location
            .as_ref()
            .and_then(|loc| loc.source.as_ref())
            .and_then(|s| s.display_name.clone())
    }

    /// Open-access PDF url, if any.
    pub fn oa_pdf_url(&self) -> Option<String> {
        self.best_oa_location
            .as_ref()
            .and_then(|loc| loc.pdf_url.clone())
            .or_else(|| {
                self.primary_location
                    .as_ref()
                    .and_then(|loc| loc.pdf_url.clone())
            })
    }

    /// Return the abstract text with redundant source section labels removed.
    pub fn abstract_text(&self) -> Option<String> {
        if let Some(text) = &self.abstract_text_override {
            return normalize_abstract_text(text, false);
        }
        let index = self.abstract_inverted_index.as_ref()?;
        if index.is_empty() {
            return None;
        }
        let mut positions: Vec<(u32, &str)> = Vec::new();
        for (word, locs) in index {
            for &pos in locs {
                positions.push((pos, word.as_str()));
            }
        }
        positions.sort_by_key(|(pos, _)| *pos);
        let text = positions
            .into_iter()
            .map(|(_, word)| word)
            .collect::<Vec<_>>()
            .join(" ");
        normalize_abstract_text(&text, true)
    }
}

fn normalize_abstract_text(text: &str, from_inverted_index: bool) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let Some(label) = text.get(..8).filter(|label| label.eq_ignore_ascii_case("abstract"))
    else {
        return Some(text.to_string());
    };
    let remainder = &text[label.len()..];
    let body = remainder.trim_start();
    let separate_line = remainder
        .chars()
        .take_while(|character| character.is_whitespace())
        .any(|character| matches!(character, '\r' | '\n'));
    let delimited_body = body.chars().next().and_then(|separator| {
        let rest = &body[separator.len_utf8()..];
        match separator {
            ':' | '\u{2013}' | '\u{2014}' => Some(rest.trim_start()),
            '.' | '-' if rest.starts_with(char::is_whitespace) => Some(rest.trim_start()),
            _ => None,
        }
    });

    // OpenAlex flattens headings into word positions. Only infer a bare label
    // before clear sentence openings, not arbitrary capitals such as "Abstract DNA".
    let flattened_label = from_inverted_index
        && remainder.starts_with(char::is_whitespace)
        && matches!(
            body.split_whitespace().next(),
            Some("The" | "This" | "We" | "Our" | "Here")
        );
    let cleaned = if let Some(body) = delimited_body {
        body
    } else if separate_line || flattened_label {
        body
    } else {
        text
    };

    // Preserve ambiguous label-only records rather than deleting their entire content.
    Some(if cleaned.is_empty() { text } else { cleaned }.to_string())
}

/// Normalize any OpenAlex id or url (author `A…`, source `S…`, topic `T…`) to its bare
/// id form by stripping the `https://openalex.org/` prefix.
pub fn normalize_id(raw: &str) -> String {
    raw.trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(raw)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::{DiscoverySource, DiscoverySourceKind};

    #[test]
    fn abstract_text_removes_explicit_labels_from_both_sources() {
        for text in [
            "Abstract: The study.",
            " ABSTRACT : The study. ",
            "abstract:The study.",
            "Abstract. The study.",
            "Abstract - The study.",
            "Abstract\u{2014}The study.",
            "Abstract \u{2013} The study.",
            "Abstract\nThe study.",
            "Abstract\r\nThe study.",
        ] {
            for from_inverted_index in [false, true] {
                assert_eq!(
                    normalize_abstract_text(text, from_inverted_index).as_deref(),
                    Some("The study."),
                    "{text:?}, from_inverted_index={from_inverted_index}"
                );
            }
        }
    }

    #[test]
    fn abstract_text_removes_only_clear_flattened_openalex_labels() {
        for opening in ["The", "This", "We", "Our", "Here"] {
            let mut work: Work = serde_json::from_value(serde_json::json!({
                "abstract_inverted_index": {
                    "Abstract": [0],
                    opening: [1],
                    "study.": [2]
                }
            }))
            .unwrap();
            assert_eq!(work.abstract_text(), Some(format!("{opening} study.")));
            work.abstract_text_override = Some(format!("Abstract {opening} study."));
            assert_eq!(
                work.abstract_text(),
                Some(format!("Abstract {opening} study."))
            );
        }
        assert_eq!(
            normalize_abstract_text("ABSTRACT This study.", true).as_deref(),
            Some("This study.")
        );
    }

    #[test]
    fn abstract_text_preserves_ambiguous_openings_and_body_content() {
        for text in [
            "Abstract representations are useful.",
            "Abstract DNA models are useful.",
            "Abstract Sperm subpopulations are distinct.",
            "Abstract-based models are useful.",
            "Abstract.js is a library.",
            "Abstractness is measured.",
            "Abstract",
            "Abstract:",
            "Background: The study. Abstract appears later.",
            "The study discusses Abstract representations.",
            "\u{7814}\u{7a76} describes a study.",
        ] {
            for from_inverted_index in [false, true] {
                assert_eq!(
                    normalize_abstract_text(text, from_inverted_index).as_deref(),
                    Some(text),
                    "{text:?}, from_inverted_index={from_inverted_index}"
                );
            }
        }
        assert_eq!(
            normalize_abstract_text("Abstract: Background: A study.\nMethods: Details.", false)
                .as_deref(),
            Some("Background: A study.\nMethods: Details.")
        );
    }

    #[test]
    fn abstract_text_handles_missing_empty_and_override_values() {
        let mut work: Work = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(work.abstract_text(), None);
        work.abstract_inverted_index = Some(HashMap::new());
        assert_eq!(work.abstract_text(), None);
        work.abstract_text_override = Some(" \n ".to_string());
        assert_eq!(work.abstract_text(), None);
        work.abstract_text_override = Some(" Abstract: A study. ".to_string());
        assert_eq!(work.abstract_text().as_deref(), Some("A study."));
        assert_eq!(
            work.abstract_text_override.as_deref(),
            Some(" Abstract: A study. ")
        );
    }

    #[test]
    fn serializes_structured_provenance() {
        let work: Work = serde_json::from_value(serde_json::json!({
            "id": "curated:1",
            "discovery_sources": [{
                "kind": "curated_collection",
                "key": "collection",
                "label": "Collection",
                "url": "https://example.com/collection"
            }]
        }))
        .unwrap();

        assert_eq!(
            work.discovery_sources,
            vec![DiscoverySource::curated_collection(
                "collection".to_string(),
                "Collection".to_string(),
                "https://example.com/collection".to_string()
            )]
        );
        assert!(work
            .discovery_sources
            .iter()
            .any(|source| source.kind == DiscoverySourceKind::CuratedCollection));

        let serialized = serde_json::to_value(work).unwrap();
        assert!(serialized.get("discovery_sources").is_some());
    }

    #[test]
    fn adding_a_discovery_source_is_idempotent() {
        let mut work: Work = serde_json::from_value(serde_json::json!({"id": "W1"})).unwrap();

        work.add_discovery_source(DiscoverySource::openalex());
        work.add_discovery_source(DiscoverySource::openalex());

        assert_eq!(work.discovery_sources, vec![DiscoverySource::openalex()]);
    }
}
