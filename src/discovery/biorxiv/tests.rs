use super::*;

fn api_work() -> ApiWork {
    ApiWork {
        doi: "10.1101/2026.01.02.123456".to_string(),
        title: "A useful preprint".to_string(),
        authors: "Doe, J.; Smith, A.".to_string(),
        date: "2026-01-02".to_string(),
        version: "2".to_string(),
        license: "cc_by".to_string(),
        category: "synthetic biology".to_string(),
        jatsxml: "https://www.biorxiv.org/content/early/source.xml".to_string(),
        abstract_text: "An abstract.".to_string(),
        published: "10.1000/published".to_string(),
    }
}

#[test]
fn normalizes_published_doi_urls() {
    assert_eq!(
        normalize_doi("https://doi.org/10.1000/published").as_deref(),
        Some("10.1000/published")
    );
}

#[test]
fn validates_supported_categories() {
    assert!(validate_categories(&[
        "synthetic_biology".to_string(),
        "bioinformatics".to_string()
    ])
    .is_ok());
    assert!(validate_categories(&[
        "bioengineering".to_string(),
        "systems_biology".to_string(),
        "genomics".to_string(),
        "biochemistry".to_string(),
        "biophysics".to_string(),
        "molecular_biology".to_string(),
    ])
    .is_ok());
    assert_eq!(
        validate_categories(&["neuroscience".to_string()]).unwrap_err(),
        "Unknown bioRxiv category \"neuroscience\"."
    );
}

#[test]
fn converts_biophysics_protein_preprints() {
    let mut source = api_work();
    source.category = "biophysics".to_string();
    source.title = "Single-molecule identification of full-length proteins with single-amino-acid resolution using nanopores".to_string();
    let work = convert_work("biophysics", source).unwrap();

    assert_eq!(
        work.discovery_sources[0],
        DiscoverySource::biorxiv_category("biophysics", "Biophysics")
    );
    let mut works = vec![work];
    crate::nanopore::retain_relevant(&mut works);
    assert_eq!(works.len(), 1);
}

#[test]
fn converts_complete_biorxiv_metadata() {
    let work = convert_work("synthetic_biology", api_work()).unwrap();

    assert_eq!(
        work.id.as_deref(),
        Some("biorxiv:10.1101/2026.01.02.123456v2")
    );
    assert_eq!(
        work.doi.as_deref(),
        Some("https://doi.org/10.1101/2026.01.02.123456")
    );
    assert_eq!(work.author_names(), vec!["Doe, J", "Smith, A"]);
    assert_eq!(work.abstract_text().as_deref(), Some("An abstract."));
    assert_eq!(work.license.as_deref(), Some("cc_by"));
    assert_eq!(
        work.full_text_url.as_deref(),
        Some("https://www.biorxiv.org/content/early/source.xml")
    );
    assert_eq!(work.published_doi.as_deref(), Some("10.1000/published"));
    assert_eq!(
        work.oa_pdf_url().as_deref(),
        Some("https://www.biorxiv.org/content/10.1101/2026.01.02.123456v2.full.pdf")
    );
    assert_eq!(
        work.discovery_sources[0],
        DiscoverySource::biorxiv_category("synthetic_biology", "Synthetic Biology")
    );
}

#[test]
fn ignores_na_optional_values() {
    let mut source = api_work();
    source.license = "NA".to_string();
    source.published = "NA".to_string();

    source.category = "bioinformatics".to_string();
    let work = convert_work("bioinformatics", source).unwrap();

    assert!(work.license.is_none());
    assert!(work.published_doi.is_none());
}

#[test]
fn parses_semicolon_delimited_authors() {
    assert_eq!(
        parse_authors("Doe, J.; Smith, A.; ")
            .into_iter()
            .map(|authorship| authorship.raw_author_name.unwrap())
            .collect::<Vec<_>>(),
        vec!["Doe, J", "Smith, A"]
    );
}

#[test]
fn deserializes_official_response_field_names() {
    let response: ApiResponse = serde_json::from_str(
        r#"{
            "messages": [{"total": "1"}],
            "collection": [{
                "doi": "10.1101/2026.01.02.123456",
                "title": "A useful preprint",
                "authors": "Doe, J.; Smith, A.",
                "date": "2026-01-02",
                "version": "1",
                "license": "cc_by",
                "category": "bioinformatics",
                "jatsxml": "https://www.biorxiv.org/source.xml",
                "abstract": "An abstract.",
                "published": "NA"
            }]
        }"#,
    )
    .unwrap();

    assert_eq!(response.collection[0].abstract_text, "An abstract.");
}
