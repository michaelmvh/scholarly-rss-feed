use crate::discovery::snapshot::{DiscoverySnapshot, RefreshPolicy};
use crate::openalex::{Author, Authorship, Location, Source, Work};
use crate::provenance::DiscoverySource;
use chrono::NaiveDate;
use hyper::header::{ETAG, IF_NONE_MATCH};
use hyper::StatusCode;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const PELDOM_PROTEIN_DESIGN: &str = "peldom-protein-design";
pub const NANOPORE_FOUNDATIONS: &str = "nanopore-foundations";

const NANOPORE_FOUNDATIONS_NAME: &str = "Nanopore Foundations & Signal Analysis";
const NANOPORE_FOUNDATIONS_URL: &str = "https://github.com/Michaelmvh/scholarly-rss-feed/blob/main/src/curated/nanopore-foundations.json";
const PELDOM_README_URL: &str =
    "https://raw.githubusercontent.com/Peldom/papers_for_protein_design_using_DL/main/README.md";
const PELDOM_REPOSITORY_URL: &str = "https://github.com/Peldom/papers_for_protein_design_using_DL";
const PELDOM_SOURCE_NAME: &str = "Papers for Protein Design Using Deep Learning";
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MIN_EXPECTED_PAPERS: usize = 100;
const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const KNOWN_IRRELEVANT_IDS: &[&str] = &["arxiv:2602.23956"];
const SNAPSHOT_FILE: &str = "peldom-snapshot-v3.json";
const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

lazy_static! {
    static ref PELDOM_SNAPSHOT: Arc<RwLock<Option<Snapshot>>> = Arc::new(RwLock::new(None));
    static ref PELDOM_REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
}

#[derive(Clone)]
struct Snapshot {
    etag: Option<String>,
    checked_at: Instant,
    works: Vec<Work>,
    diagnostics: SourceDiagnostics,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SourceDiagnostics {
    pub source_name: String,
    pub entries_seen: usize,
    pub accepted: usize,
    pub excluded_section: usize,
    pub irrelevant: usize,
    pub unavailable: usize,
    pub missing_stable_id: usize,
}

#[derive(Deserialize, Serialize)]
struct PersistedMetadata {
    etag: Option<String>,
    diagnostics: SourceDiagnostics,
}

#[derive(Clone, Debug)]
pub struct SourceEvaluation {
    pub works: Vec<Work>,
    pub diagnostics: Vec<SourceDiagnostics>,
}

pub fn validate_sources(source_names: &[String]) -> Result<(), String> {
    for source_name in source_names {
        if source_details(source_name).is_none() {
            return Err(format!("Unknown curated source \"{source_name}\"."));
        }
    }
    Ok(())
}

pub fn source_details(source_name: &str) -> Option<(&'static str, &'static str)> {
    match source_name {
        PELDOM_PROTEIN_DESIGN => Some((PELDOM_SOURCE_NAME, PELDOM_REPOSITORY_URL)),
        NANOPORE_FOUNDATIONS => Some((NANOPORE_FOUNDATIONS_NAME, NANOPORE_FOUNDATIONS_URL)),
        _ => None,
    }
}

pub async fn fetch_sources(
    client: &reqwest::Client,
    source_names: &[String],
    from: &str,
    policy: RefreshPolicy,
) -> Result<Vec<Work>, String> {
    Ok(fetch_sources_with_policy(client, source_names, from, policy)
        .await?
        .works)
}

pub async fn fetch_sources_for_evaluation(
    client: &reqwest::Client,
    source_names: &[String],
    from: &str,
) -> Result<SourceEvaluation, String> {
    fetch_sources_with_policy(client, source_names, from, RefreshPolicy::AllowStale).await
}

async fn fetch_sources_with_policy(
    client: &reqwest::Client,
    source_names: &[String],
    from: &str,
    policy: RefreshPolicy,
) -> Result<SourceEvaluation, String> {
    validate_sources(source_names)?;
    let mut works = Vec::new();
    let mut diagnostics = Vec::new();

    for source_name in source_names {
        match source_name.as_str() {
            PELDOM_PROTEIN_DESIGN => {
                let snapshot = fetch_peldom(client, policy).await?;
                works.extend(snapshot.works);
                diagnostics.push(snapshot.diagnostics);
            }
            NANOPORE_FOUNDATIONS => {
                let papers = nanopore_foundations()?;
                diagnostics.push(SourceDiagnostics {
                    source_name: NANOPORE_FOUNDATIONS.to_string(),
                    entries_seen: papers.len(),
                    accepted: papers.len(),
                    ..SourceDiagnostics::default()
                });
                works.extend(papers);
            }
            _ => unreachable!("curated sources were validated"),
        }
    }

    filter_from(&mut works, from)?;

    Ok(SourceEvaluation { works, diagnostics })
}

fn nanopore_foundations() -> Result<Vec<Work>, String> {
    let mut works: Vec<Work> =
        serde_json::from_str(include_str!("curated/nanopore-foundations.json"))
            .map_err(|error| format!("Invalid nanopore foundations collection: {error}"))?;
    for work in &mut works {
        let doi = work.doi.as_deref().ok_or_else(|| {
            format!("Nanopore foundations paper lacks a DOI: {}", work.best_title())
        })?;
        work.id = Some(format!("doi:{}", crate::works::normalize_doi(doi)));
        work.add_discovery_source(DiscoverySource::curated_collection(
            NANOPORE_FOUNDATIONS.to_string(),
            NANOPORE_FOUNDATIONS_NAME.to_string(),
            NANOPORE_FOUNDATIONS_URL.to_string(),
        ));
    }
    Ok(works)
}

fn filter_from(works: &mut Vec<Work>, from: &str) -> Result<(), String> {
    let from_date = NaiveDate::parse_from_str(from, "%Y-%m-%d")
        .map_err(|error| format!("Invalid curated-source cutoff \"{from}\": {error}"))?;
    let include_undated = from_date == NaiveDate::from_ymd_opt(1900, 1, 1).unwrap();
    works.retain(|work| {
        work.publication_date
            .as_deref()
            .and_then(parse_date)
            .or_else(|| {
                work.collection_date
                    .as_ref()
                    .and_then(|collection_date| parse_date(&collection_date.date))
            })
            .is_some_and(|date| date >= from_date)
            || (include_undated
                && work.publication_date.is_none()
                && work.collection_date.is_none())
    });
    Ok(())
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

async fn fetch_peldom(client: &reqwest::Client, policy: RefreshPolicy) -> Result<Snapshot, String> {
    if policy == RefreshPolicy::AllowStale {
        if let Some(snapshot) = fresh_snapshot() {
            return Ok(snapshot);
        }
    }

    let _refresh_guard = PELDOM_REFRESH.lock().await;
    if policy == RefreshPolicy::AllowStale {
        if let Some(snapshot) = fresh_snapshot() {
            return Ok(snapshot);
        }
    }

    let cached = PELDOM_SNAPSHOT
        .read()
        .clone()
        .or_else(load_persisted_snapshot);
    match refresh_peldom(client, cached.as_ref()).await {
        Ok(snapshot) => {
            if let Err(error) = persist_snapshot(&snapshot) {
                policy.check_failure(&error)?;
                eprintln!("{error}");
            }
            *PELDOM_SNAPSHOT.write() = Some(snapshot.clone());
            Ok(snapshot)
        }
        Err(error) => match cached {
            Some(mut snapshot) => {
                policy.check_failure(&error)?;
                eprintln!("{error}; using the last successfully parsed Peldom snapshot");
                snapshot.checked_at = Instant::now();
                *PELDOM_SNAPSHOT.write() = Some(snapshot.clone());
                Ok(snapshot)
            }
            None => Err(error),
        },
    }
}

fn load_persisted_snapshot() -> Option<Snapshot> {
    match DiscoverySnapshot::<PersistedMetadata>::load(
        SNAPSHOT_FILE,
        PELDOM_PROTEIN_DESIGN,
        SNAPSHOT_SCHEMA_VERSION,
    ) {
        Ok(Some(snapshot)) => {
            let (works, metadata) = snapshot.into_parts();
            if works.len() < MIN_EXPECTED_PAPERS {
                eprintln!(
                    "Ignoring persisted Peldom snapshot with only {} papers",
                    works.len()
                );
                return None;
            }
            Some(Snapshot {
                etag: metadata.etag,
                checked_at: Instant::now()
                    .checked_sub(REFRESH_INTERVAL)
                    .unwrap_or_else(Instant::now),
                works,
                diagnostics: metadata.diagnostics,
            })
        }
        Ok(None) => None,
        Err(error) => {
            eprintln!("{error}");
            None
        }
    }
}

fn persist_snapshot(snapshot: &Snapshot) -> Result<(), String> {
    DiscoverySnapshot::new(
        PELDOM_PROTEIN_DESIGN,
        SNAPSHOT_SCHEMA_VERSION,
        None,
        snapshot.works.clone(),
        PersistedMetadata {
            etag: snapshot.etag.clone(),
            diagnostics: snapshot.diagnostics.clone(),
        },
    )
    .save(SNAPSHOT_FILE)
}

fn fresh_snapshot() -> Option<Snapshot> {
    PELDOM_SNAPSHOT
        .read()
        .as_ref()
        .filter(|snapshot| snapshot.checked_at.elapsed() < REFRESH_INTERVAL)
        .cloned()
}

async fn refresh_peldom(
    client: &reqwest::Client,
    cached: Option<&Snapshot>,
) -> Result<Snapshot, String> {
    let mut request = client.get(PELDOM_README_URL);
    if let Some(etag) = cached.and_then(|snapshot| snapshot.etag.as_deref()) {
        request = request.header(IF_NONE_MATCH, etag);
    }
    let mut response = request
        .send()
        .await
        .map_err(|error| format!("Peldom source request failed: {error}"))?;

    if response.status() == StatusCode::NOT_MODIFIED {
        let mut snapshot = cached
            .cloned()
            .ok_or_else(|| "Peldom returned 304 without a cached snapshot".to_string())?;
        snapshot.checked_at = Instant::now();
        return Ok(snapshot);
    }
    response = response
        .error_for_status()
        .map_err(|error| format!("Peldom source request failed: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "Peldom source exceeded the {MAX_RESPONSE_BYTES}-byte limit"
        ));
    }

    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("Failed reading Peldom source: {error}"))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(format!(
                "Peldom source exceeded the {MAX_RESPONSE_BYTES}-byte limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let markdown = String::from_utf8(body)
        .map_err(|error| format!("Peldom source was not valid UTF-8: {error}"))?;
    let (mut works, diagnostics) = parse_peldom_with_diagnostics(&markdown)?;
    if works.len() < MIN_EXPECTED_PAPERS {
        return Err(format!(
            "Peldom parser found only {} stable-ID papers; expected at least {MIN_EXPECTED_PAPERS}",
            works.len()
        ));
    }
    let undated_titles = works
        .iter()
        .filter(|work| work.publication_date.is_none())
        .map(Work::best_title)
        .collect::<HashSet<_>>();
    match crate::github_history::fetch_collection_dates(client, &undated_titles).await {
        Ok(collection_dates) => {
            for work in &mut works {
                if work.publication_date.is_none() {
                    work.collection_date = collection_dates.get(&work.best_title()).cloned();
                }
            }
            let unresolved = works
                .iter()
                .filter(|work| work.publication_date.is_none() && work.collection_date.is_none())
                .count();
            if unresolved > 0 {
                eprintln!(
                    "GitHub blame did not resolve collection dates for {unresolved} undated papers"
                );
            }
        }
        Err(error) => {
            eprintln!("Could not enrich undated papers with collection dates: {error}");
        }
    }

    Ok(Snapshot {
        etag,
        checked_at: Instant::now(),
        works,
        diagnostics,
    })
}

fn parse_peldom_with_diagnostics(markdown: &str) -> Result<(Vec<Work>, SourceDiagnostics), String> {
    let mut section: Option<String> = None;
    let mut subsection: Option<String> = None;
    let mut pending: Option<PendingPaper> = None;
    let mut works = Vec::new();
    let mut diagnostics = SourceDiagnostics {
        source_name: PELDOM_PROTEIN_DESIGN.to_string(),
        ..SourceDiagnostics::default()
    };

    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            finish_pending(&mut pending, &mut works, &mut diagnostics);
            section = Some(heading.trim().to_string());
            subsection = None;
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("### ") {
            finish_pending(&mut pending, &mut works, &mut diagnostics);
            subsection = Some(heading.trim().to_string());
            continue;
        }
        if let Some(title) = paper_title(trimmed) {
            finish_pending(&mut pending, &mut works, &mut diagnostics);
            diagnostics.entries_seen += 1;
            if included_section(section.as_deref(), subsection.as_deref()) {
                pending = Some(PendingPaper {
                    title,
                    section: section.clone().unwrap_or_default(),
                    subsection: subsection.clone(),
                    lines: Vec::new(),
                });
            } else {
                diagnostics.excluded_section += 1;
            }
            continue;
        }
        if let Some(paper) = &mut pending {
            paper.lines.push(trimmed.to_string());
        }
    }
    finish_pending(&mut pending, &mut works, &mut diagnostics);

    if works.is_empty() {
        return Err("Peldom document contained no recognizable stable-ID papers".to_string());
    }
    diagnostics.accepted = works.len();
    Ok((works, diagnostics))
}

struct PendingPaper {
    title: String,
    section: String,
    subsection: Option<String>,
    lines: Vec<String>,
}

fn finish_pending(
    pending: &mut Option<PendingPaper>,
    works: &mut Vec<Work>,
    diagnostics: &mut SourceDiagnostics,
) {
    let Some(paper) = pending.take() else {
        return;
    };
    if paper
        .lines
        .iter()
        .any(|line| line.to_ascii_lowercase().contains("paper unavailable"))
    {
        diagnostics.unavailable += 1;
        return;
    }
    match paper.into_work() {
        Some(work)
            if work
                .id
                .as_deref()
                .is_some_and(|id| KNOWN_IRRELEVANT_IDS.contains(&id)) =>
        {
            diagnostics.irrelevant += 1;
        }
        Some(work) => works.push(work),
        None => diagnostics.missing_stable_id += 1,
    }
}

impl PendingPaper {
    fn into_work(self) -> Option<Work> {
        let links = self
            .lines
            .iter()
            .flat_map(|line| markdown_links(line))
            .collect::<Vec<_>>();
        let (identifier, paper_link, venue) = stable_identifier(&links)?;
        let authors = self
            .lines
            .iter()
            .find(|line| {
                !line.is_empty()
                    && !line.starts_with('[')
                    && !line.starts_with('+')
                    && !line.starts_with('<')
            })
            .map(|line| split_authors(line))
            .unwrap_or_default();
        let publication_date = self
            .lines
            .iter()
            .find_map(|line| find_publication_date(line));
        let source = venue.clone().map(|display_name| Source {
            display_name: Some(display_name),
        });
        let categories = [Some(self.section), self.subsection]
            .into_iter()
            .flatten()
            .collect();

        Some(Work {
            id: Some(identifier.clone()),
            doi: identifier
                .strip_prefix("doi:")
                .map(|doi| format!("https://doi.org/{doi}")),
            title: Some(self.title),
            display_name: None,
            publication_date,
            latest_version_date: None,
            collection_date: None,
            cited_by_count: None,
            authorships: Some(
                authors
                    .into_iter()
                    .map(|name| Authorship {
                        author: Some(Author {
                            id: None,
                            display_name: Some(name.clone()),
                        }),
                        raw_author_name: Some(name),
                    })
                    .collect(),
            ),
            primary_location: Some(Location {
                landing_page_url: Some(paper_link),
                pdf_url: None,
                source,
                version: None,
            }),
            best_oa_location: None,
            abstract_inverted_index: None,
            abstract_text_override: None,
            license: None,
            full_text_url: None,
            published_doi: None,
            alternate_links: Vec::new(),
            matched_author_names: Vec::new(),
            author_matches: Vec::new(),
            openalex_author_matches: Vec::new(),
            discovery_sources: vec![DiscoverySource::curated_collection(
                PELDOM_PROTEIN_DESIGN.to_string(),
                PELDOM_SOURCE_NAME.to_string(),
                PELDOM_REPOSITORY_URL.to_string(),
            )],
            curated_categories: categories,
        })
    }
}

fn paper_title(line: &str) -> Option<String> {
    let content = line.strip_prefix("**")?;
    let end = content.find("**")?;
    let title = content[..end].trim();
    (!title.is_empty()).then(|| title.to_string())
}

fn included_section(section: Option<&str>, subsection: Option<&str>) -> bool {
    let section_number = section
        .and_then(|heading| heading.split_once('.'))
        .and_then(|(number, _)| number.trim().parse::<u8>().ok());
    if !matches!(section_number, Some(1..=7)) {
        return false;
    }
    !subsection.is_some_and(|heading| heading.starts_with("7.3") || heading.starts_with("7.5"))
}

fn markdown_links(line: &str) -> Vec<(String, String)> {
    let mut links = Vec::new();
    let mut remaining = line;
    while let Some(label_start) = remaining.find('[') {
        remaining = &remaining[label_start + 1..];
        let Some(label_end) = remaining.find("](") else {
            continue;
        };
        let label = remaining[..label_end].trim_matches(['[', ']']).trim();
        remaining = &remaining[label_end + 2..];
        let Some(url_end) = remaining.find(')') else {
            break;
        };
        let url = remaining[..url_end].trim();
        if url.starts_with("http://") || url.starts_with("https://") {
            links.push((label.to_string(), url.to_string()));
        }
        remaining = &remaining[url_end + 1..];
    }
    links
}

fn stable_identifier(links: &[(String, String)]) -> Option<(String, String, Option<String>)> {
    for (label, url) in links {
        if is_artifact_link(label, url) {
            continue;
        }
        if let Some(doi) = extract_doi(url) {
            return Some((
                format!("doi:{doi}"),
                format!("https://doi.org/{doi}"),
                Some(label.clone()),
            ));
        }
    }
    for (label, url) in links {
        if is_artifact_link(label, url) {
            continue;
        }
        if let Some(arxiv_id) = extract_arxiv_id(url) {
            return Some((
                format!("arxiv:{arxiv_id}"),
                url.clone(),
                Some(label.clone()),
            ));
        }
    }
    None
}

fn is_artifact_link(label: &str, url: &str) -> bool {
    let label = label.to_ascii_lowercase();
    let url = url.to_ascii_lowercase();
    [
        "code",
        "github",
        "model",
        "dataset",
        "supplement",
        "website",
        "project",
        "checkpoint",
        "lecture",
        "video",
    ]
    .iter()
    .any(|marker| label.contains(marker))
        || url.contains("doi.org/10.5281/zenodo.")
}

fn extract_doi(url: &str) -> Option<String> {
    let lowercase = url.to_ascii_lowercase();
    let supported_context = lowercase.contains("doi.org/10.")
        || lowercase.contains("/doi/10.")
        || lowercase.contains("biorxiv.org/content/10.");
    if !supported_context {
        return None;
    }
    let start = lowercase.find("10.")?;
    let mut doi = lowercase[start..]
        .split(['?', '#', ')', ']', ' '])
        .next()?
        .trim_end_matches('/')
        .to_string();
    if doi.starts_with("10.1101/") || doi.starts_with("10.64898/") {
        doi = strip_preprint_version(&doi);
    } else if !lowercase.contains("doi.org/") {
        doi = strip_publisher_route_suffix(&doi);
    }
    let (prefix, suffix) = doi.split_once('/')?;
    let valid_prefix = prefix
        .strip_prefix("10.")
        .is_some_and(|value| !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()));
    (valid_prefix && !suffix.is_empty()).then_some(doi)
}

fn strip_publisher_route_suffix(doi: &str) -> String {
    let Some((prefix, suffix)) = doi.rsplit_once('/') else {
        return doi.to_string();
    };
    if suffix.chars().all(|character| character.is_ascii_digit())
        && prefix.matches('/').count() >= 2
    {
        prefix.to_string()
    } else {
        doi.to_string()
    }
}

fn strip_preprint_version(doi: &str) -> String {
    let Some(version_start) = doi.rfind('v') else {
        return doi.to_string();
    };
    let suffix = &doi[version_start + 1..];
    let digit_count = suffix
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();
    if digit_count > 0 && (digit_count == suffix.len() || suffix[digit_count..].starts_with('.')) {
        doi[..version_start].to_string()
    } else {
        doi.to_string()
    }
}

fn extract_arxiv_id(url: &str) -> Option<String> {
    let lowercase = url.to_ascii_lowercase();
    let start = lowercase.find("arxiv.org/abs/")? + "arxiv.org/abs/".len();
    let id = lowercase[start..]
        .split(['?', '#', '/', ')', ']', ' '])
        .next()?;
    let id = strip_preprint_version(id);
    (!id.is_empty()
        && id
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.'))
    .then_some(id)
}

fn split_authors(line: &str) -> Vec<String> {
    let citation_style = line.contains(", and ");
    let mut authors = line
        .replace(", and ", ", ")
        .replace(" and ", ", ")
        .replace(" & ", ", ")
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let repeated_citation_pairs = authors.len() >= 4
        && authors.len().is_multiple_of(2)
        && authors
            .chunks_exact(2)
            .all(|pair| !pair[0].contains(' ') && looks_like_initials(&pair[1]));
    if repeated_citation_pairs {
        return authors
            .chunks_exact(2)
            .map(|pair| format!("{} {}", pair[1], pair[0]))
            .collect();
    }
    if citation_style
        && authors.len() >= 3
        && authors[0].split_whitespace().count() == 1
        && authors[1].split_whitespace().count() == 1
    {
        let given_name = authors.remove(1);
        authors[0] = format!("{given_name} {}", authors[0]);
    }
    authors
}

fn looks_like_initials(name: &str) -> bool {
    let letter_count = name
        .chars()
        .filter(|character| character.is_alphabetic())
        .count();
    (1..=3).contains(&letter_count)
        && name
            .chars()
            .all(|character| character.is_alphabetic() || ".- ".contains(character))
}

fn find_publication_date(line: &str) -> Option<String> {
    if let Some(date) = find_calendar_date(line) {
        return Some(date);
    }
    if let Some(date) = find_arxiv_month(line) {
        return Some(date);
    }
    find_explicit_year(line).map(|year| format!("{year}-01-01"))
}

fn find_calendar_date(line: &str) -> Option<String> {
    let numeric_parts = line
        .split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    numeric_parts.windows(3).find_map(|parts| {
        if parts[0].len() != 4 {
            return None;
        }
        let year = parts[0].parse::<i32>().ok()?;
        let month = parts[1].parse::<u32>().ok()?;
        let day = parts[2].parse::<u32>().ok()?;
        let date = chrono::NaiveDate::from_ymd_opt(year, month, day)?;
        (1900..=2099)
            .contains(&year)
            .then(|| date.format("%Y-%m-%d").to_string())
    })
}

fn find_arxiv_month(line: &str) -> Option<String> {
    let lowercase = line.to_ascii_lowercase();
    let (marker, marker_start) = ["arxiv:", "arxiv.org/abs/"]
        .into_iter()
        .find_map(|marker| lowercase.find(marker).map(|start| (marker, start)))?;
    let start = marker_start + marker.len();
    let year = 2000 + lowercase.get(start..start + 2)?.parse::<i32>().ok()?;
    let month = lowercase.get(start + 2..start + 4)?.parse::<u32>().ok()?;
    chrono::NaiveDate::from_ymd_opt(year, month, 1).map(|date| date.format("%Y-%m-%d").to_string())
}

fn find_explicit_year(line: &str) -> Option<u16> {
    line.split(|character: char| !character.is_ascii_digit())
        .find_map(|part| {
            (part.len() == 4)
                .then(|| part.parse::<u16>().ok())
                .flatten()
                .filter(|year| (1900..=2099).contains(year))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
# List

## 0. Benchmarks and datasets

### 0.1 Sequence

**Excluded benchmark**
Ada Lovelace
[bioRxiv 2025](https://www.biorxiv.org/content/10.1101/2025.01.01.123456v1)

## 3. Function to Scaffold

### 3.4 Diffusion-based

**A DOI paper**
Ada Lovelace, Grace Hopper
[bioRxiv 2026.05.06.723381](https://www.biorxiv.org/content/10.64898/2026.05.06.723381v2) • [code](https://github.com/example/code)

**An arXiv paper**
Alan Turing and Joan Clarke
[arXiv:2602.04916](https://arxiv.org/abs/2602.04916v3)

**Known irrelevant paper**
Example Author
[arXiv:2602.23956](https://arxiv.org/abs/2602.23956)

**An undated DOI paper**
Syrlybaeva, Raulia, and Eva-Maria Strauch
[Science](https://www.science.org/doi/10.1126/science.aee1792)

**An artifact-only record**
Example Author
[code](https://doi.org/10.5281/zenodo.17089342)

**Unavailable paper**
Example Author
Paper unavailable at [Workshop 2026](https://example.com/workshop)

## 7. Other

### 7.3 Molecular Design Models

**Excluded molecule paper**
Example Author
[arXiv](https://arxiv.org/abs/2601.00001)
"#;

    #[test]
    fn parses_only_stable_ids_from_included_sections() {
        let (works, diagnostics) = parse_peldom_with_diagnostics(FIXTURE).unwrap();

        assert_eq!(works.len(), 3);
        assert_eq!(
            diagnostics,
            SourceDiagnostics {
                source_name: PELDOM_PROTEIN_DESIGN.to_string(),
                entries_seen: 8,
                accepted: 3,
                excluded_section: 2,
                irrelevant: 1,
                unavailable: 1,
                missing_stable_id: 1,
            }
        );
        assert_eq!(
            works[0].id.as_deref(),
            Some("doi:10.64898/2026.05.06.723381")
        );
        assert_eq!(
            works[0].author_names(),
            vec!["Ada Lovelace", "Grace Hopper"]
        );
        assert_eq!(
            works[0].curated_categories,
            vec!["3. Function to Scaffold", "3.4 Diffusion-based"]
        );
        assert_eq!(works[1].id.as_deref(), Some("arxiv:2602.04916"));
        assert_eq!(works[1].author_names(), vec!["Alan Turing", "Joan Clarke"]);
        assert_eq!(works[2].id.as_deref(), Some("doi:10.1126/science.aee1792"));
        assert_eq!(
            works[2].author_names(),
            vec!["Raulia Syrlybaeva", "Eva-Maria Strauch"]
        );
        assert!(works[2].publication_date.is_none());
    }

    #[test]
    fn extracts_dois_embedded_in_publisher_paths() {
        assert_eq!(
            extract_doi("https://academic.oup.com/article/doi/10.1093/bioinformatics/btad027/1")
                .as_deref(),
            Some("10.1093/bioinformatics/btad027")
        );
        assert!(extract_doi("https://nature.com/articles/s41592-019-0496-6").is_none());
        assert_eq!(
            extract_doi("https://www.biorxiv.org/content/10.64898/2026.05.09.722041v1.full.pdf")
                .as_deref(),
            Some("10.64898/2026.05.09.722041")
        );
    }

    #[test]
    fn preserves_available_date_precision() {
        assert_eq!(
            find_publication_date(
                "[bioRxiv 2026.05.06.723381](https://www.biorxiv.org/content/10.64898/2026.05.06.723381v2)"
            )
            .as_deref(),
            Some("2026-05-06")
        );
        assert_eq!(
            find_publication_date("[arXiv:2208.13616v2](https://arxiv.org/abs/2208.13616v2)")
                .as_deref(),
            Some("2022-08-01")
        );
        assert_eq!(
            find_publication_date("[arXiv:2605.26690](https://arxiv.org/abs/2605.26690)")
                .as_deref(),
            Some("2026-05-01")
        );
        assert_eq!(
            find_publication_date("[Journal 2026](https://example.com/article)").as_deref(),
            Some("2026-01-01")
        );
    }

    #[test]
    fn rejects_documents_without_recognizable_papers() {
        assert!(parse_peldom_with_diagnostics("# unexpected content").is_err());
    }

    #[test]
    fn validates_registered_source_names() {
        assert!(validate_sources(&[
            PELDOM_PROTEIN_DESIGN.to_string(),
            NANOPORE_FOUNDATIONS.to_string(),
        ])
        .is_ok());
        assert!(validate_sources(&["unknown".to_string()]).is_err());
    }

    #[tokio::test]
    async fn foundations_preserve_methods_and_landmarks_without_network_discovery() {
        let works = fetch_sources(
            &reqwest::Client::new(),
            &[NANOPORE_FOUNDATIONS.to_string()],
            "1900-01-01",
            RefreshPolicy::RequireFresh,
        )
        .await
        .unwrap();
        let identifiers = works
            .iter()
            .map(|work| work.id.clone().unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(identifiers.len(), works.len());
        for doi in [
            "10.1093/bioinformatics/btv046",
            "10.1093/gigascience/giy037",
            "10.1371/journal.pone.0178751",
            "10.1038/s41586-024-07935-7",
            "10.64898/2026.01.08.698148",
            "10.64898/2026.05.19.726067",
            "10.21203/rs.3.rs-8098579/v1",
        ] {
            assert!(identifiers.contains(&format!("doi:{doi}")), "{doi}");
        }
        for work in &works {
            assert!(parse_date(work.publication_date.as_deref().unwrap()).is_some());
            assert!(!work.author_names().is_empty());
            assert!(!work.curated_categories.is_empty());
            assert_eq!(work.discovery_sources[0].key, NANOPORE_FOUNDATIONS);
        }

        let mut protein_only = works.clone();
        crate::nanopore::retain_relevant(&mut protein_only);
        assert!(protein_only.len() < works.len());
        assert!(!protein_only.iter().any(|work| {
            work.doi.as_deref() == Some("https://doi.org/10.1093/gigascience/giy037")
        }));

        let recent = fetch_sources(
            &reqwest::Client::new(),
            &[NANOPORE_FOUNDATIONS.to_string()],
            "2026-01-01",
            RefreshPolicy::RequireFresh,
        )
        .await
        .unwrap();
        assert!(!recent.is_empty());
        assert!(recent.len() < works.len());
        assert!(recent
            .iter()
            .all(|work| work.publication_date.as_deref().unwrap() >= "2026-01-01"));

        let mut enriched = works[0].clone();
        enriched.id = Some("https://openalex.org/W123".into());
        enriched.discovery_sources = vec![DiscoverySource::openalex()];
        let merged = crate::works::merge_works(vec![enriched], works);
        assert_eq!(merged.len(), identifiers.len());
        assert!(merged.iter().any(|work| {
            work.discovery_sources.iter().any(|source| source.key == "openalex")
                && work.discovery_sources.iter().any(|source| source.key == NANOPORE_FOUNDATIONS)
        }));
    }

    #[test]
    fn full_archive_keeps_undated_stable_id_papers() {
        let mut works = parse_peldom_with_diagnostics(FIXTURE).unwrap().0;

        filter_from(&mut works, "1900-01-01").unwrap();
        assert!(works.iter().any(|work| work.publication_date.is_none()));

        let undated = works
            .iter_mut()
            .find(|work| work.publication_date.is_none())
            .unwrap();
        undated.collection_date = Some(crate::openalex::CollectionDate {
            date: "2025-05-03".to_string(),
            commit_url: "https://example.com/commit".to_string(),
        });
        filter_from(&mut works, "2025-01-01").unwrap();
        assert!(works.iter().any(|work| {
            work.publication_date.is_none()
                && work
                    .collection_date
                    .as_ref()
                    .is_some_and(|date| date.date == "2025-05-03")
        }));
    }

    #[test]
    fn cutoff_compares_complete_publication_dates() {
        let mut works = vec![
            serde_json::from_value(serde_json::json!({
                "id": "before",
                "publication_date": "2026-01-01"
            }))
            .unwrap(),
            serde_json::from_value(serde_json::json!({
                "id": "after",
                "publication_date": "2026-09-02"
            }))
            .unwrap(),
        ];

        filter_from(&mut works, "2026-09-01").unwrap();

        assert_eq!(works.len(), 1);
        assert_eq!(works[0].id.as_deref(), Some("after"));
    }

    #[test]
    fn malformed_cutoff_is_rejected() {
        let mut works = Vec::new();

        assert!(filter_from(&mut works, "2026").is_err());
    }

    #[test]
    fn parses_repeated_surname_initial_citations_as_people() {
        assert_eq!(
            split_authors("Notin, P., Dias, M., Meier, J., Gal, Y"),
            vec!["P. Notin", "M. Dias", "J. Meier", "Y Gal"]
        );
    }

    #[test]
    fn separates_ampersand_delimited_authors() {
        assert_eq!(
            split_authors("Gaetano T. Montelione & David Baker"),
            vec!["Gaetano T. Montelione", "David Baker"]
        );
    }
}
