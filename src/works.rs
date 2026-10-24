use crate::openalex::{normalize_id, AuthorMatch, Authorship, OpenAlexAuthorMatch, Work};
use std::collections::{HashMap, HashSet};

/// A signature that identifies a work well enough to treat two records as versions of
/// each other. Two works are grouped when any of their keys match.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum VersionKey {
    Doi(String),
    AuthorIds(String, Vec<String>),
    AuthorNames(String, Vec<String>),
}

/// Merge two result sets, deduplicating exact records and grouping publication versions.
pub(crate) fn merge_works(primary: Vec<Work>, secondary: Vec<Work>) -> Vec<Work> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut version_groups: HashMap<VersionKey, usize> = HashMap::new();
    let mut works: Vec<Work> = Vec::new();

    for work in primary.into_iter().chain(secondary) {
        let keys = version_keys(&work);
        if let Some(index) = work.id.as_ref().and_then(|id| seen.get(id)).copied() {
            merge_work_version(&mut works[index], work);
            for key in keys {
                version_groups.entry(key).or_insert(index);
            }
            continue;
        }

        let group = keys.iter().find_map(|key| version_groups.get(key).copied());

        match group {
            Some(index) => {
                if let Some(id) = &work.id {
                    seen.insert(id.clone(), index);
                }
                merge_work_version(&mut works[index], work);
                for key in keys {
                    version_groups.entry(key).or_insert(index);
                }
            }
            None => {
                let index = works.len();
                if let Some(id) = &work.id {
                    seen.insert(id.clone(), index);
                }
                for key in keys {
                    version_groups.insert(key, index);
                }
                works.push(work);
            }
        }
    }

    works.sort_by(|left, right| sort_date(right).cmp(sort_date(left)));

    works
}

fn sort_date(work: &Work) -> &str {
    work.publication_date
        .as_deref()
        .or_else(|| {
            work.collection_date
                .as_ref()
                .map(|collection_date| collection_date.date.as_str())
        })
        .unwrap_or("")
}

pub(crate) fn doi_identifiers(work: &Work) -> Vec<String> {
    let mut identifiers = Vec::new();
    if let Some(doi) = work.doi.as_deref().map(normalize_doi) {
        if !doi.is_empty() {
            identifiers.push(doi);
        }
    }
    if let Some(arxiv_id) = work.id.as_deref().and_then(|id| id.strip_prefix("arxiv:")) {
        identifiers.push(format!(
            "10.48550/arxiv.{}",
            arxiv_id.to_ascii_lowercase()
        ));
    }
    for link in &work.alternate_links {
        let Ok(url) = url::Url::parse(link) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https")
            || !matches!(url.host_str(), Some("doi.org" | "dx.doi.org"))
            || !url.username().is_empty()
            || url.password().is_some()
        {
            continue;
        }
        let doi = normalize_doi(url.path().trim_start_matches('/'));
        if doi.starts_with("10.") && doi.contains('/') && !identifiers.contains(&doi) {
            identifiers.push(doi);
        }
    }
    identifiers
}

pub(crate) fn version_keys(work: &Work) -> Vec<VersionKey> {
    let mut keys = doi_identifiers(work)
        .into_iter()
        .map(VersionKey::Doi)
        .collect::<Vec<_>>();

    let title = work
        .title
        .as_ref()
        .or(work.display_name.as_ref())
        .map(|title| normalize_title(title))
        .filter(|title| !title.is_empty());

    let (Some(title), Some(authorships)) = (title, work.authorships.as_ref()) else {
        return keys;
    };
    if authorships.is_empty() {
        return keys;
    }

    let author_ids = authorships
        .iter()
        .map(|authorship| {
            authorship
                .author
                .as_ref()?
                .id
                .as_ref()
                .map(|id| normalize_id(id))
        })
        .collect::<Option<Vec<_>>>();
    if let Some(mut author_ids) = author_ids {
        author_ids.sort();
        author_ids.dedup();
        if !author_ids.is_empty() {
            keys.push(VersionKey::AuthorIds(title.clone(), author_ids));
        }
    }

    let name_sources: [fn(&Authorship) -> Option<&str>; 3] = [
        |authorship| {
            authorship
                .author
                .as_ref()
                .and_then(|author| author.display_name.as_deref())
                .or(authorship.raw_author_name.as_deref())
        },
        |authorship| {
            authorship
                .author
                .as_ref()
                .and_then(|author| author.display_name.as_deref())
        },
        |authorship| authorship.raw_author_name.as_deref(),
    ];

    for source in name_sources {
        let author_names = authorships
            .iter()
            .map(|authorship| {
                let name = normalize_author_name(source(authorship)?);
                (!name.is_empty()).then_some(name)
            })
            .collect::<Option<Vec<_>>>();

        if let Some(mut author_names) = author_names {
            author_names.sort();
            author_names.dedup();
            if !author_names.is_empty() {
                let key = VersionKey::AuthorNames(title.clone(), author_names);
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
    }

    keys
}

/// Mark authors using normalized exact-name matching. Canonical names and explicit
/// provider aliases are supplied by configuration resolution.
pub(crate) fn mark_authors_by_name(works: &mut [Work], tracked_names: &[String]) {
    let tracked_names = tracked_names
        .iter()
        .map(|name| normalize_author_name(name))
        .filter(|name| !name.is_empty())
        .collect::<HashSet<_>>();
    if tracked_names.is_empty() {
        return;
    }

    for work in works {
        let Some(authorships) = work.authorships.as_deref() else {
            continue;
        };
        for authorship in authorships {
            let display_name = authorship
                .author
                .as_ref()
                .and_then(|author| author.display_name.as_ref());
            let raw_name = authorship.raw_author_name.as_ref();
            let matched = display_name
                .into_iter()
                .chain(raw_name)
                .any(|name| tracked_names.contains(&normalize_author_name(name)));
            if matched {
                work.matched_author_names
                    .extend(display_name.into_iter().cloned());
                work.matched_author_names
                    .extend(raw_name.into_iter().cloned());
            }
        }
        work.matched_author_names.sort();
        work.matched_author_names.dedup();
    }
}

pub(crate) fn mark_feed_authors(works: &mut [Work], feed_author_ids: &[String]) {
    for work in works {
        let Some(authorships) = work.authorships.as_deref() else {
            continue;
        };
        for authorship in authorships {
            let Some(author) = authorship.author.as_ref() else {
                continue;
            };
            let Some(author_id) = author.id.as_deref().map(normalize_id) else {
                continue;
            };
            if !feed_author_ids.contains(&author_id) {
                continue;
            }
            for name in author
                .display_name
                .iter()
                .chain(authorship.raw_author_name.iter())
            {
                work.matched_author_names.push(name.clone());
                work.openalex_author_matches.push(OpenAlexAuthorMatch {
                    author_id: author_id.clone(),
                    matched_name: name.clone(),
                });
            }
        }
        work.matched_author_names.sort();
        work.matched_author_names.dedup();
        work.openalex_author_matches.sort();
        work.openalex_author_matches.dedup();
    }
}

pub(crate) fn normalize_author_name(name: &str) -> String {
    let tokens = author_name_tokens(name);

    let mut significant = tokens
        .iter()
        .filter(|token| token.chars().count() > 1)
        .cloned()
        .collect::<Vec<_>>();
    if significant.is_empty() {
        significant = tokens;
    }

    significant.sort();
    significant.dedup();
    significant.join(" ")
}

pub(crate) fn resolve_author_match(
    authorships: &[Authorship],
    matched: &AuthorMatch,
) -> Option<usize> {
    resolve_named_author_match(authorships, &matched.matched_name, &matched.queried_name)
}

pub(crate) fn resolve_openalex_author_match(
    authorships: &[Authorship],
    matched: &OpenAlexAuthorMatch,
    tracked_ids: &HashSet<String>,
    configured_name: Option<&str>,
) -> Option<usize> {
    let matched_id = normalize_id(&matched.author_id);
    let mut exact_matches = authorships
        .iter()
        .enumerate()
        .filter_map(|(index, authorship)| {
            authorship
                .author
                .as_ref()
                .and_then(|author| author.id.as_deref())
                .is_some_and(|id| normalize_id(id) == matched_id)
                .then_some(index)
        });
    if let Some(index) = exact_matches.next() {
        return exact_matches.next().is_none().then_some(index);
    }

    let index = resolve_named_author_match(
        authorships,
        &matched.matched_name,
        configured_name.unwrap_or(&matched.matched_name),
    )?;
    // A replacement ID may describe the same person, but not another tracked identity.
    let conflicting_id = authorships[index]
        .author
        .as_ref()
        .and_then(|author| author.id.as_deref())
        .is_some_and(|id| tracked_ids.contains(&normalize_id(id)));
    (!conflicting_id).then_some(index)
}

fn resolve_named_author_match(
    authorships: &[Authorship],
    matched_name: &str,
    queried_name: &str,
) -> Option<usize> {
    let exact_name = normalize_author_name_with_initials(matched_name);
    if exact_name.is_empty() {
        return None;
    }
    let exact_name = exact_name.as_str();
    let matching_authors = |exact: bool| {
        authorships
            .iter()
            .enumerate()
            .filter_map(move |(index, authorship)| {
                authorship
                    .author
                    .as_ref()
                    .and_then(|author| author.display_name.as_deref())
                    .into_iter()
                    .chain(authorship.raw_author_name.as_deref())
                    .any(|name| {
                        if exact {
                            normalize_author_name_with_initials(name) == exact_name
                        } else {
                            // The original query can rule out a conflicting expanded name.
                            compatible_author_names(name, matched_name)
                                && compatible_author_names(name, queried_name)
                        }
                    })
                    .then_some(index)
            })
    };
    let mut exact_matches = matching_authors(true);
    if let Some(index) = exact_matches.next() {
        return exact_matches.next().is_none().then_some(index);
    }
    let mut compatible_matches = matching_authors(false);
    let index = compatible_matches.next()?;
    compatible_matches.next().is_none().then_some(index)
}

fn compatible_author_names(left: &str, right: &str) -> bool {
    let Some((left_surname, left_given)) = author_name_parts(left) else {
        return false;
    };
    let Some((right_surname, right_given)) = author_name_parts(right) else {
        return false;
    };
    left_surname == right_surname
        && left_given.iter().zip(&right_given).all(|(left, right)| {
            left == right
                || ((left.chars().count() == 1 || right.chars().count() == 1)
                    && left.chars().next() == right.chars().next())
        })
}

fn author_name_parts(name: &str) -> Option<(Vec<String>, Vec<String>)> {
    let (surname, given) = if let Some((surname, given)) = name.split_once(',') {
        if given.contains(',') {
            return None;
        }
        (author_name_tokens(surname), author_name_tokens(given))
    } else {
        let mut given = author_name_tokens(name);
        let surname = given.pop()?;
        (vec![surname], given)
    };
    (!surname.is_empty() && !given.is_empty()).then_some((surname, given))
}

fn normalize_author_name_with_initials(name: &str) -> String {
    let tokens = if name.contains(',') {
        let Some((surname, mut given)) = author_name_parts(name) else {
            return String::new();
        };
        given.extend(surname);
        given
    } else {
        author_name_tokens(name)
    };
    tokens.join(" ")
}

fn author_name_tokens(name: &str) -> Vec<String> {
    name.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

pub(crate) fn normalize_doi(doi: &str) -> String {
    let doi = doi.trim().to_lowercase();
    let doi = doi
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("dx.")
        .trim_start_matches("doi.org/")
        .trim_start_matches("doi:");
    doi.trim().trim_end_matches('/').to_string()
}

fn normalize_title(title: &str) -> String {
    let mut normalized = String::new();
    let mut separated = false;

    for character in title.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            if separated && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character);
            separated = false;
        } else if !normalized.is_empty() {
            separated = true;
        }
    }

    normalized
}

fn merge_work_version(existing: &mut Work, mut candidate: Work) {
    let latest_version_date = [
        existing.latest_version_date.as_deref(),
        existing.publication_date.as_deref(),
        candidate.latest_version_date.as_deref(),
        candidate.publication_date.as_deref(),
    ]
    .into_iter()
    .flatten()
    .max()
    .map(str::to_string);
    let collection_date = existing
        .collection_date
        .take()
        .or_else(|| candidate.collection_date.take());
    let license = existing.license.take().or_else(|| candidate.license.take());
    let full_text_url = existing
        .full_text_url
        .take()
        .or_else(|| candidate.full_text_url.take());
    let published_doi = existing
        .published_doi
        .take()
        .or_else(|| candidate.published_doi.take());
    let mut matched_author_names = std::mem::take(&mut existing.matched_author_names);
    matched_author_names.append(&mut candidate.matched_author_names);
    matched_author_names.sort();
    matched_author_names.dedup();
    let mut author_matches = std::mem::take(&mut existing.author_matches);
    author_matches.append(&mut candidate.author_matches);
    author_matches.sort();
    author_matches.dedup();
    let mut openalex_author_matches = std::mem::take(&mut existing.openalex_author_matches);
    openalex_author_matches.append(&mut candidate.openalex_author_matches);
    openalex_author_matches.sort();
    openalex_author_matches.dedup();
    let mut discovery_sources = std::mem::take(&mut existing.discovery_sources);
    discovery_sources.append(&mut candidate.discovery_sources);
    discovery_sources.sort();
    discovery_sources.dedup();
    let mut curated_categories = std::mem::take(&mut existing.curated_categories);
    curated_categories.append(&mut candidate.curated_categories);
    curated_categories.sort();
    curated_categories.dedup();

    if version_quality(&candidate) > version_quality(existing) {
        std::mem::swap(existing, &mut candidate);
    }

    let selected_link = existing.best_link();
    let mut alternate_links = std::mem::take(&mut existing.alternate_links);
    alternate_links.append(&mut candidate.alternate_links);
    if let Some(link) = candidate.best_link() {
        alternate_links.push(link);
    }
    alternate_links.retain(|link| Some(link) != selected_link.as_ref());
    alternate_links.sort();
    alternate_links.dedup();
    existing.alternate_links = alternate_links;
    existing.latest_version_date = latest_version_date;
    existing.collection_date = collection_date;
    existing.license = license;
    existing.full_text_url = full_text_url;
    existing.published_doi = published_doi;
    existing.matched_author_names = matched_author_names;
    existing.author_matches = author_matches;
    existing.openalex_author_matches = openalex_author_matches;
    existing.discovery_sources = discovery_sources;
    existing.curated_categories = curated_categories;
}

fn version_quality(work: &Work) -> (bool, bool, bool, &str) {
    let has_pdf = work.oa_pdf_url().is_some();
    let has_abstract = work
        .abstract_inverted_index
        .as_ref()
        .is_some_and(|index| !index.is_empty());
    let is_published = [&work.primary_location, &work.best_oa_location]
        .into_iter()
        .flatten()
        .any(|location| location.version.as_deref() == Some("publishedVersion"));
    let publication_date = work.publication_date.as_deref().unwrap_or("");

    (has_pdf, has_abstract, is_published, publication_date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openalex::{Author, Authorship};

    #[test]
    fn doi_aliases_added_to_an_existing_id_are_indexed_for_later_versions() {
        let original: Work = serde_json::from_value(serde_json::json!({
            "id": "W1", "doi": "https://doi.org/10.1000/published", "title": "Published"
        })).unwrap();
        let mut enriched = original.clone();
        enriched.alternate_links = vec!["https://doi.org/10.1000/preprint".to_string()];
        let mut preprint: Work = serde_json::from_value(serde_json::json!({
            "id": "preprint", "doi": "https://doi.org/10.1000/preprint", "title": "Old title"
        })).unwrap();
        let source = crate::provenance::DiscoverySource::curated_collection(
            "example".to_string(), "Example".to_string(), "https://example.org".to_string(),
        );
        preprint.add_discovery_source(source.clone());
        let merged = merge_works(vec![original], vec![enriched, preprint]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].discovery_sources, vec![source]);
        assert!(doi_identifiers(&merged[0]).contains(&"10.1000/preprint".to_string()));
    }

    #[test]
    fn only_doi_resolver_links_become_doi_aliases() {
        let mut work = work_with_authors(&["Ada Lovelace"]);
        work.alternate_links = vec![
            "https://doi.org/10.1000/ALIAS".to_string(),
            "http://dx.doi.org/10.1000/alias/".to_string(),
            "https://example.org/10.1000/unrelated".to_string(),
            "https://doi.org/not-a-doi".to_string(),
            "https://user:password@doi.org/10.1000/private".to_string(),
            "ftp://doi.org/10.1000/ftp".to_string(),
        ];
        assert_eq!(doi_identifiers(&work), vec!["10.1000/alias"]);
        let arxiv: Work = serde_json::from_value(serde_json::json!({
            "id": "arxiv:2402.10516"
        })).unwrap();
        assert_eq!(doi_identifiers(&arxiv), vec!["10.48550/arxiv.2402.10516"]);
    }

    fn work_with_authors(names: &[&str]) -> Work {
        Work {
            id: Some("curated:1".to_string()),
            doi: None,
            title: Some("Curated paper".to_string()),
            display_name: None,
            publication_date: Some("2026-01-01".to_string()),
            latest_version_date: None,
            collection_date: None,
            cited_by_count: None,
            authorships: Some(
                names
                    .iter()
                    .map(|name| Authorship {
                        author: Some(Author {
                            id: None,
                            display_name: Some((*name).to_string()),
                        }),
                        raw_author_name: Some((*name).to_string()),
                    })
                    .collect(),
            ),
            primary_location: None,
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
            discovery_sources: Vec::new(),
            curated_categories: Vec::new(),
        }
    }

    #[test]
    fn resolves_author_associations_conservatively() {
        let cases: &[(&str, &str, &[&str], Option<usize>)] = &[
            (
                "P Chatterjee",
                "Pranam Chatterjee",
                &["P. K. Chatterjee"],
                Some(0),
            ),
            (
                "P K Chatterjee",
                "Pranam Chatterjee",
                &["P Chatterjee"],
                Some(0),
            ),
            (
                "P Chatterjee",
                "Pranam Chatterjee",
                &["Chatterjee, P. K."],
                Some(0),
            ),
            (
                "Chatterjee, P.",
                "Pranam Chatterjee",
                &["P. K. Chatterjee"],
                Some(0),
            ),
            (
                "P Chatterjee",
                "Pranam Chatterjee",
                &["Pranam K Chatterjee"],
                Some(0),
            ),
            ("D Baker", "David Baker", &["M Baker"], None),
            (
                "P K Chatterjee",
                "Pranam Chatterjee",
                &["P R Chatterjee"],
                None,
            ),
            (
                "Pranam Chatterjee",
                "Pranam Chatterjee",
                &["Paul Chatterjee"],
                None,
            ),
            (
                "P Chatterjee",
                "Pranam Chatterjee",
                &["Paul Chatterjee"],
                None,
            ),
            (
                "P Chatterjee",
                "Pranam K Chatterjee",
                &["P R Chatterjee"],
                None,
            ),
            (
                "P Kumar Chatterjee",
                "Pranam Chatterjee",
                &["P Kiran Chatterjee"],
                None,
            ),
            ("P Chatterjee", "Pranam Chatterjee", &["P K Smith"], None),
            ("P Chatterjee", "Pranam Chatterjee", &["Chatterjee"], None),
            ("", "Pranam Chatterjee", &[""], None),
            (
                "P Chatterjee",
                "Pranam Chatterjee",
                &["P K Chatterjee", "P R Chatterjee"],
                None,
            ),
            (
                "P Chatterjee",
                "Pranam Chatterjee",
                &["P K Chatterjee", "Chatterjee, P."],
                Some(1),
            ),
            (
                "P Chatterjee",
                "Pranam Chatterjee",
                &["P Chatterjee", "Chatterjee, P.", "P K Chatterjee"],
                None,
            ),
        ];
        for (matched_name, queried_name, names, expected) in cases {
            let work = work_with_authors(names);
            let matched = AuthorMatch {
                matched_name: (*matched_name).to_string(),
                queried_name: (*queried_name).to_string(),
            };
            assert_eq!(
                resolve_author_match(work.authorships.as_deref().unwrap(), &matched),
                *expected,
                "{matched_name:?} / {queried_name:?} against {names:?}"
            );
        }
    }

    #[test]
    fn identity_exact_names_preserve_given_name_order() {
        for (original, actual, expected) in [
            ("J A Smith", "A J Smith", None),
            ("J A Smith", "Smith, A J", None),
            ("John Alan Smith", "Alan John Smith", None),
            ("John Alan Smith", "Smith, Alan John", None),
            ("J A Smith", "Smith, J. A.", Some(0)),
            ("John Alan Smith", "Smith, John Alan", Some(0)),
            ("Ludwig van Beethoven", "van Beethoven, Ludwig", Some(0)),
        ] {
            let work = work_with_authors(&[actual]);
            let matched = AuthorMatch {
                queried_name: original.to_string(),
                matched_name: original.to_string(),
            };
            assert_eq!(
                resolve_author_match(work.authorships.as_deref().unwrap(), &matched),
                expected,
                "{original} -> {actual}",
            );
        }
        let work = work_with_authors(&["J J Smith", "J Smith"]);
        let matched = AuthorMatch {
            queried_name: "John Smith".to_string(),
            matched_name: "J Smith".to_string(),
        };
        assert_eq!(
            resolve_author_match(work.authorships.as_deref().unwrap(), &matched),
            Some(1),
        );
    }

    #[test]
    fn openalex_identity_resolution_prefers_ids_and_blocks_conflicting_tracked_ids() {
        let work: Work = serde_json::from_value(serde_json::json!({
            "authorships": [
                {"author": {"id": "https://openalex.org/A1", "display_name": "Provider spelling"}},
                {"author": {"id": "A2", "display_name": "John A Smith"}}
            ]
        }))
        .unwrap();
        let tracked = HashSet::from(["A1".to_string(), "A2".to_string()]);
        let matched = OpenAlexAuthorMatch {
            author_id: "A1".to_string(),
            matched_name: "John A Smith".to_string(),
        };
        let authorships = work.authorships.unwrap();
        assert_eq!(
            resolve_openalex_author_match(&authorships, &matched, &tracked, Some("John A Smith")),
            Some(0),
        );
        assert_eq!(
            resolve_openalex_author_match(
                &authorships[1..],
                &matched,
                &tracked,
                Some("John A Smith")
            ),
            None,
        );
        assert_eq!(
            resolve_openalex_author_match(
                &authorships[1..],
                &matched,
                &HashSet::from(["A1".to_string()]),
                Some("John A Smith"),
            ),
            Some(0),
        );
    }

    #[test]
    fn resolves_raw_names_and_counts_authorships_not_spellings() {
        let mut work = work_with_authors(&["Unrelated Display", "M Baker"]);
        work.authorships.as_mut().unwrap()[0].raw_author_name = Some("Baker, D. W.".to_string());
        let matched = AuthorMatch {
            queried_name: "David Baker".to_string(),
            matched_name: "D Baker".to_string(),
        };
        assert_eq!(
            resolve_author_match(work.authorships.as_deref().unwrap(), &matched),
            Some(0)
        );
        work.authorships.as_mut().unwrap()[0]
            .author
            .as_mut()
            .unwrap()
            .display_name = Some("D W Baker".to_string());
        assert_eq!(
            resolve_author_match(work.authorships.as_deref().unwrap(), &matched),
            Some(0)
        );
    }

    #[test]
    fn marks_configured_authors_on_records_without_provider_ids() {
        let mut works = vec![work_with_authors(&[
            "Baker, David",
            "Nicholas F Polizzi",
            "Untracked Author",
        ])];

        mark_authors_by_name(
            &mut works,
            &["David Baker".to_string(), "Nicholas F. Polizzi".to_string()],
        );

        assert_eq!(
            works[0].matched_author_names,
            vec!["Baker, David", "Nicholas F Polizzi"]
        );
    }

    #[test]
    fn configured_aliases_mark_the_corresponding_curated_author() {
        let mut works = vec![work_with_authors(&["David W Baker"])];

        mark_authors_by_name(&mut works, &["David W Baker".to_string()]);

        assert_eq!(works[0].matched_author_names, vec!["David W Baker"]);
    }

    #[test]
    fn does_not_fuzzily_match_different_authors() {
        let mut works = vec![work_with_authors(&["Daniel Baker"])];

        mark_authors_by_name(&mut works, &["David Baker".to_string()]);

        assert!(works[0].matched_author_names.is_empty());
    }

    #[test]
    fn merges_curated_arxiv_record_with_its_openalex_doi() {
        let enriched: Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W1",
            "doi": "https://doi.org/10.48550/arxiv.2605.26690",
            "title": "A title",
            "authorships": [{"author": {"display_name": "John Smith"}}]
        }))
        .unwrap();
        let curated: Work = serde_json::from_value(serde_json::json!({
            "id": "arxiv:2605.26690",
            "title": "A title",
            "authorships": [{"author": {"display_name": "J. Smith"}}]
        }))
        .unwrap();

        let merged = merge_works(vec![enriched], vec![curated]);

        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn preserves_collection_date_when_enriched_version_wins() {
        let enriched: Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W1",
            "doi": "https://doi.org/10.1000/example",
            "publication_date": "2026-01-02",
            "abstract_inverted_index": {"Abstract": [0]}
        }))
        .unwrap();
        let mut curated: Work = serde_json::from_value(serde_json::json!({
            "id": "doi:10.1000/example",
            "doi": "https://doi.org/10.1000/example"
        }))
        .unwrap();
        curated.collection_date = Some(crate::openalex::CollectionDate {
            date: "2025-05-03".to_string(),
            commit_url: "https://github.com/example/repo/commit/abc".to_string(),
        });

        let merged = merge_works(vec![enriched], vec![curated]);

        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0]
                .collection_date
                .as_ref()
                .map(|date| date.date.as_str()),
            Some("2025-05-03")
        );
    }

    #[test]
    fn merges_structured_provenance() {
        let mut provider: Work = serde_json::from_value(serde_json::json!({
            "id": "https://openalex.org/W1",
            "doi": "https://doi.org/10.1000/example"
        }))
        .unwrap();
        provider.add_discovery_source(crate::provenance::DiscoverySource::openalex());
        let mut curated: Work = serde_json::from_value(serde_json::json!({
            "id": "curated:1",
            "doi": "https://doi.org/10.1000/example"
        }))
        .unwrap();
        curated.add_discovery_source(crate::provenance::DiscoverySource::curated_collection(
            "collection".to_string(),
            "Collection".to_string(),
            "https://example.com/collection".to_string(),
        ));

        let merged = merge_works(vec![provider], vec![curated]);

        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].discovery_sources,
            vec![
                crate::provenance::DiscoverySource::openalex(),
                crate::provenance::DiscoverySource::curated_collection(
                    "collection".to_string(),
                    "Collection".to_string(),
                    "https://example.com/collection".to_string()
                )
            ]
        );
    }
}
