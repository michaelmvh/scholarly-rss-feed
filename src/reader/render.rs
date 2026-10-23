use super::filters::{
    author_selection_fingerprint, is_view_param, AuthorSelection, FilterOptions, Period,
    ViewFilters, AUTHOR_MODE_PARAM, AUTHOR_OPTIONS_PARAM, AUTHOR_PARAM, AUTHOR_SELECTION_PARAM,
    PERIOD_PARAM,
};
use super::{Author, Feed, Publication};
use crate::provenance::{DiscoverySource, DiscoverySourceKind};
use crate::{
    ARXIV_CATEGORY_PARAM, BIORXIV_CATEGORY_PARAM, PAPER_SOURCE_MODE_PARAM, PAPER_SOURCE_PARAM,
};

pub const FAVICON: &str = include_str!("../assets/apple-touch-icon.svg");
pub const READER_CSS: &str = include_str!("../assets/reader.css");
pub const READER_JS: &str = include_str!("../assets/reader.js");

pub fn render_feed(feed: &Feed, params: &[(String, String)]) -> String {
    let title = escape_html(&feed.title);
    let description = escape_html(&feed.description);
    let rss_url = escape_html(&raw_feed_url(params));
    let options = FilterOptions::from_feed(feed);
    let filters = ViewFilters::from_params(params, &options);
    let today = chrono::Utc::now().date_naive();
    let publications = feed
        .publications
        .iter()
        .filter(|publication| filters.matches_on(publication, today))
        .collect::<Vec<_>>();
    let item_count = publications.len();
    let item_label = if filters != ViewFilters::default() || item_count != feed.publications.len() {
        format!("{item_count} of {} publications", feed.publications.len())
    } else if item_count == 1 {
        "1 publication".to_string()
    } else {
        format!("{item_count} publications")
    };
    let filter_form = render_filter_form(params, &filters, &options, &feed.paper_sources);

    let mut articles = String::new();
    for publication in publications {
        let title_markup = match publication.id.as_deref() {
            Some(article_id) => format!(
                r#"<a href="{}">{}</a>"#,
                escape_html(&article_url(params, article_id)),
                escape_html(&publication.title)
            ),
            None => escape_html(&publication.title),
        };
        let (date, separator, venue) = render_metadata(publication);
        let authors = render_authors(&publication.authors);
        let provenance = render_provenance(publication);

        articles.push_str(&format!(
            r#"<li class="publication">
  <p class="metadata">{date}{separator}{venue}</p>
  <h2>{title_markup}</h2>
  {authors}
  {provenance}
</li>
"#
        ));
    }

    if articles.is_empty() {
        articles.push_str(r#"<li class="empty">No recent publications found.</li>"#);
    }
    let author_key = if feed
        .publications
        .iter()
        .any(|publication| publication.authors.iter().any(|author| author.matched_feed))
    {
        r#"<p class="author-key"><strong class="notable-author">Highlighted authors</strong> matched this feed.</p>"#
    } else {
        ""
    };

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="description" content="{description}">
  <link rel="icon" href="/favicon.svg" type="image/svg+xml">
  <link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png">
  <link rel="alternate" type="application/rss+xml" title="{title}" href="{rss_url}">
  <title>{title}</title>
  <link rel="stylesheet" href="/reader.css?v=5">
  <script src="/reader.js?v=7" defer></script>
</head>
<body>
  <main>
    <header>
      <h1>{title}</h1>
      <p class="intro">{description}</p>
      <p class="feed-meta"><span>{item_label}</span><a href="{rss_url}">View raw RSS</a></p>
      {author_key}
      {filter_form}
    </header>
    <ol class="publication-list">
      {articles}
    </ol>
  </main>
</body>
</html>
"#
    )
}

pub fn render_article(
    feed: &Feed,
    article_id: &str,
    params: &[(String, String)],
) -> Option<String> {
    let publication = feed
        .publications
        .iter()
        .find(|publication| publication.id.as_deref() == Some(article_id))?;
    let feed_title = escape_html(&feed.title);
    let item_title = escape_html(&publication.title);
    let rss_url = escape_html(&raw_feed_url(params));
    let back_url = escape_html(&reader_url(params));
    let (date, separator, venue) = render_metadata(publication);
    let authors = render_authors(&publication.authors);
    let provenance = render_provenance(publication);
    let author_note = if publication.authors.iter().any(|author| author.matched_feed) {
        r#"<p class="author-note"><strong class="notable-author">Highlighted authors</strong> matched this feed.</p>"#
    } else {
        ""
    };
    let abstract_text = publication
        .abstract_text
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(escape_html)
        .unwrap_or_else(|| String::from("No abstract is available for this publication."));
    let article_link = publication
        .link
        .as_deref()
        .map(|link| {
            format!(
                r#"<a class="primary-action" href="{}" target="_blank" rel="external noopener">Read full article <span aria-hidden="true">↗</span></a>"#,
                escape_html(link)
            )
        })
        .unwrap_or_default();
    let pdf_link = publication
        .pdf_url
        .as_deref()
        .map(|link| {
            format!(
                r#"<a class="secondary-action" href="{}" target="_blank" rel="external noopener">Open PDF <span aria-hidden="true">↗</span></a>"#,
                escape_html(link)
            )
        })
        .unwrap_or_default();

    Some(format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <meta name="description" content="Abstract for {item_title}">
  <link rel="icon" href="/favicon.svg" type="image/svg+xml">
  <link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png">
  <link rel="alternate" type="application/rss+xml" title="{feed_title}" href="{rss_url}">
  <title>{item_title} · {feed_title}</title>
  <link rel="stylesheet" href="/reader.css?v=4">
</head>
<body>
  <main>
    <a class="back" href="{back_url}">← Back to all publications</a>
    <article class="article-detail">
      <p class="metadata">{date}{separator}{venue}</p>
      <h1>{item_title}</h1>
      {authors}
      {provenance}
      {author_note}
      <section class="abstract" aria-labelledby="abstract-heading">
        <h2 id="abstract-heading">Abstract</h2>
        <p>{abstract_text}</p>
      </section>
      <p class="actions">{article_link}{pdf_link}<a href="{rss_url}">View raw RSS</a></p>
    </article>
  </main>
</body>
</html>
"#
    ))
}

fn render_metadata(publication: &Publication) -> (String, &'static str, String) {
    let date = publication
        .publication_date
        .as_deref()
        .map(format_reader_date)
        .map(|value| format!(r#"<time>{}</time>"#, escape_html(&value)))
        .or_else(|| {
            publication.collection_date.as_ref().map(|collection_date| {
                let value = escape_html(&format_reader_date(&collection_date.date));
                let commit_url = escape_html(&collection_date.commit_url);
                format!(
                    r#"Added to collection <a href="{commit_url}" rel="external"><time>{value}</time></a>"#
                )
            })
        })
        .unwrap_or_default();
    let venue = publication
        .venue
        .as_deref()
        .map(escape_html)
        .unwrap_or_default();
    let separator = if !date.is_empty() && !venue.is_empty() {
        r#"<span aria-hidden="true"> · </span>"#
    } else {
        ""
    };

    (date, separator, venue)
}

fn render_authors(authors: &[Author]) -> String {
    if authors.is_empty() {
        return String::new();
    }

    let authors = authors
        .iter()
        .map(|author| {
            let name = escape_html(&author.name);
            if author.matched_feed {
                format!(r#"<strong class="notable-author">{name}</strong>"#)
            } else {
                name
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    format!(r#"<p class="authors">{authors}</p>"#)
}

fn render_provenance(publication: &Publication) -> String {
    let curated_sources = publication
        .discovery_sources
        .iter()
        .filter(|source| source.is_curated_collection())
        .collect::<Vec<_>>();
    let native_sources = publication
        .discovery_sources
        .iter()
        .filter(|source| {
            matches!(
                source.kind,
                DiscoverySourceKind::Biorxiv | DiscoverySourceKind::Arxiv
            )
        })
        .collect::<Vec<_>>();
    let render_sources = |sources: &[&DiscoverySource]| {
        sources
            .iter()
            .map(|source| {
                source.url.as_deref().map_or_else(
                    || escape_html(&source.label),
                    |url| {
                        format!(
                            r#"<a href="{}" rel="external">{}</a>"#,
                            escape_html(url),
                            escape_html(&source.label)
                        )
                    },
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut provenance = String::new();
    if !native_sources.is_empty() {
        provenance.push_str(&format!(
            r#"<p class="provenance">Preprint source: {}</p>"#,
            render_sources(&native_sources)
        ));
    }
    if curated_sources.is_empty() {
        return provenance;
    }

    let sources = render_sources(&curated_sources);
    let categories = if publication.curated_categories.is_empty() {
        String::new()
    } else {
        format!(
            " · {}",
            escape_html(&publication.curated_categories.join(" · "))
        )
    };

    provenance.push_str(&format!(
        r#"<p class="provenance">Curated by {sources}{categories}</p>"#
    ));
    provenance
}

fn render_filter_form(
    params: &[(String, String)],
    filters: &ViewFilters,
    options: &FilterOptions,
    paper_sources: &[super::PaperSourceOption],
) -> String {
    let hidden_params = params
        .iter()
        .filter(|(name, _)| {
            name != "rss"
                && name != "article"
                && name != BIORXIV_CATEGORY_PARAM
                && name != ARXIV_CATEGORY_PARAM
                && name != PAPER_SOURCE_MODE_PARAM
                && name != PAPER_SOURCE_PARAM
                && !is_view_param(name)
        })
        .map(|(name, value)| {
            format!(
                r#"<input type="hidden" name="{}" value="{}">"#,
                escape_html(name),
                escape_html(value)
            )
        })
        .collect::<String>();
    let periods = [
        (None, "All available"),
        (Some(Period::Days30), "Past 30 days"),
        (Some(Period::Days90), "Past 90 days"),
        (Some(Period::Year1), "Past year"),
    ]
    .into_iter()
    .map(|(period, label)| {
        render_option(
            period.map(Period::value).unwrap_or_default(),
            label,
            filters.period == period,
        )
    })
    .collect::<String>();
    let selected_authors = match filters.author_selection {
        AuthorSelection::Default => options.default_authors(),
        AuthorSelection::All => options.authors.keys().cloned().collect(),
        AuthorSelection::Custom => filters.authors.clone(),
    };
    let author_filter = render_author_filter(
        &selected_authors,
        &options.authors,
        filters.author_selection,
    );
    let paper_source_filter = render_paper_source_filter(paper_sources);
    let (filter_action_class, clear_link) = if filters == &ViewFilters::default() {
        ("filter-actions no-clear", String::new())
    } else {
        (
            "filter-actions",
            format!(
                r#"<a class="clear-filters" href="{}">Clear filters</a>"#,
                escape_html(&unfiltered_reader_url(params))
            ),
        )
    };

    format!(
        r#"<form class="filters" method="get" action="/">
        {hidden_params}
        <input type="hidden" name="{PAPER_SOURCE_MODE_PARAM}" value="custom">
        <div class="filter-fields">
          <label class="filter-select">Publication date<select name="{PERIOD_PARAM}" data-auto-submit>{periods}</select></label>
          {author_filter}
          {paper_source_filter}
        </div>
        <div class="{filter_action_class}"><button class="apply-filters" type="submit">Apply filters</button>{clear_link}</div>
      </form>"#
    )
}

fn render_paper_source_filter(options: &[super::PaperSourceOption]) -> String {
    if options.is_empty() {
        return String::new();
    }
    let selected = options.iter().filter(|option| option.selected).count();
    let summary = match selected {
        0 => "Default sources".to_string(),
        1 => options
            .iter()
            .find(|option| option.selected)
            .map(|option| option.label.clone())
            .unwrap_or_else(|| "1 selected".to_string()),
        count => format!("{count} selected"),
    };
    let mut repositories = options
        .iter()
        .map(|option| option.group)
        .collect::<Vec<_>>();
    repositories.sort_unstable();
    repositories.dedup();
    let groups = repositories
        .into_iter()
        .map(|group| {
            let checkboxes = options
                .iter()
                .filter(|option| option.group == group)
                .map(|option| {
                    let checked = if option.selected { " checked" } else { "" };
                    format!(
                        r#"<label class="filter-checkbox"><input type="checkbox" name="{}" value="{}" data-label="{}"{checked}><span>{}</span></label>"#,
                        PAPER_SOURCE_PARAM,
                        escape_html(&option.value),
                        escape_html(&option.label),
                        escape_html(&option.label)
                    )
                })
                .collect::<String>();
            format!(
                r#"<div class="filter-option-group"><strong>{}</strong>{checkboxes}</div>"#,
                escape_html(group)
            )
        })
        .collect::<String>();

    format!(
        r#"<details class="filter-multiselect" data-multiselect>
            <summary><span class="filter-label">Paper sources</span><span class="filter-value" data-selection-summary>{}</span></summary>
            <div class="filter-options">
              <div class="filter-option-list">{groups}</div>
            </div>
          </details>"#,
        escape_html(&summary)
    )
}

fn render_author_filter(
    selected: &[String],
    options: &std::collections::BTreeMap<String, String>,
    selection: AuthorSelection,
) -> String {
    let mode = match selection {
        AuthorSelection::Default => "default",
        AuthorSelection::All => "all",
        AuthorSelection::Custom => "custom",
    };
    let selection_fingerprint = author_selection_fingerprint(selected);
    let options_fingerprint =
        author_selection_fingerprint(&options.keys().cloned().collect::<Vec<_>>());
    let hidden = format!(
        r#"<input type="hidden" name="{AUTHOR_MODE_PARAM}" value="{mode}">
        <input type="hidden" name="{AUTHOR_SELECTION_PARAM}" value="{selection_fingerprint}">
        <input type="hidden" name="{AUTHOR_OPTIONS_PARAM}" value="{options_fingerprint}">"#
    );
    if options.is_empty() {
        return hidden;
    }

    let mut options = options.iter().collect::<Vec<_>>();
    options.sort_by(|left, right| left.1.cmp(right.1));
    let all_selected = selected.len() == options.len();
    let selection_summary = match selected {
        _ if all_selected => "All tracked authors".to_string(),
        [] => "No tracked authors".to_string(),
        [value] => options
            .iter()
            .find(|(option, _)| option.as_str() == value)
            .map(|(_, label)| (*label).clone())
            .unwrap_or_else(|| "1 selected".to_string()),
        values => format!("{} selected", values.len()),
    };
    let checkboxes = options
        .into_iter()
        .map(|(value, label)| {
            let checked = if selected.contains(value) {
                " checked"
            } else {
                ""
            };
            format!(
                r#"<label class="filter-checkbox"><input type="checkbox" name="{AUTHOR_PARAM}" value="{}" data-label="{}"{checked}><span>{}</span></label>"#,
                escape_html(value),
                escape_html(label),
                escape_html(label)
            )
        })
        .collect::<String>();
    let all_checked = if all_selected { " checked" } else { "" };
    format!(
        r#"<details class="filter-multiselect" data-author-picker>
            <summary><span class="filter-label">Tracked authors</span><span class="filter-value" data-selection-summary>{}</span></summary>
            {hidden}
            <div class="filter-options">
              <label class="filter-checkbox filter-select-all" hidden><input type="checkbox" data-select-all{all_checked}><span>All tracked authors</span></label>
              <div class="filter-option-list">{checkboxes}</div>
            </div>
          </details>"#,
        escape_html(&selection_summary)
    )
}

fn render_option(value: &str, label: &str, selected: bool) -> String {
    let selected = if selected { " selected" } else { "" };
    format!(
        r#"<option value="{}"{selected}>{}</option>"#,
        escape_html(value),
        escape_html(label)
    )
}

fn raw_feed_url(params: &[(String, String)]) -> String {
    let query = feed_query(params, false);
    if query.is_empty() {
        String::from("?rss")
    } else {
        format!("?{query}&rss")
    }
}

fn reader_url(params: &[(String, String)]) -> String {
    let query = feed_query(params, true);
    if query.is_empty() {
        String::from("/")
    } else {
        format!("/?{query}")
    }
}

fn unfiltered_reader_url(params: &[(String, String)]) -> String {
    let query = feed_query(params, false);
    if query.is_empty() {
        String::from("/")
    } else {
        format!("/?{query}")
    }
}

fn article_url(params: &[(String, String)], article_id: &str) -> String {
    let query = feed_query(params, true);
    let path = format!("/article/{}", encode_article_id(article_id));
    if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    }
}

pub fn article_id_from_path(path: &str) -> Option<String> {
    let encoded = path.strip_prefix("/article/")?;
    if encoded.is_empty() || encoded.len() % 2 != 0 {
        return None;
    }

    let bytes = encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex_value(pair[0])?;
            let low = hex_value(pair[1])?;
            Some((high << 4) | low)
        })
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}

fn encode_article_id(article_id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(article_id.len() * 2);
    for byte in article_id.bytes() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn feed_query(params: &[(String, String)], include_view_params: bool) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in params.iter().filter(|(name, _)| {
        name != "rss" && name != "article" && (include_view_params || !is_view_param(name))
    }) {
        serializer.append_pair(name, value);
    }
    serializer.finish()
}

fn format_reader_date(date: &str) -> String {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|parsed| parsed.format("%B %-d, %Y").to_string())
        .unwrap_or_else(|_| date.to_string())
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::super::filters::SOURCE_PARAM;
    use super::super::CollectionDate;
    use super::*;
    use crate::provenance::DiscoverySource;
    use scraper::{Html, Selector};

    fn sample_feed() -> Feed {
        Feed {
            title: "Research & <Development>".to_string(),
            description: "Recent \"research\"".to_string(),
            paper_sources: Vec::new(),
            publications: vec![Publication {
                id: Some("https://openalex.org/W1".to_string()),
                title: "<script>alert('title')</script>".to_string(),
                link: Some("https://example.com/?a=1&b=2".to_string()),
                pdf_url: Some("https://example.com/article.pdf?a=1&b=2".to_string()),
                publication_date: Some("2026-08-27".to_string()),
                collection_date: None,
                venue: Some("Example & Journal".to_string()),
                authors: vec![
                    Author {
                        name: "Ada Lovelace".to_string(),
                        filter_id: "ada lovelace".to_string(),
                        matched_feed: true,
                        optional: false,
                    },
                    Author {
                        name: "Grace Hopper".to_string(),
                        filter_id: "grace-hopper".to_string(),
                        matched_feed: true,
                        optional: false,
                    },
                    Author {
                        name: "Alan Turing".to_string(),
                        filter_id: "alan-turing".to_string(),
                        matched_feed: false,
                        optional: false,
                    },
                ],
                abstract_text: Some("<strong>abstract</strong>".to_string()),
                discovery_sources: vec![
                    DiscoverySource::openalex(),
                    DiscoverySource::curated_collection(
                        "curated-source".to_string(),
                        "Curated Source".to_string(),
                        "https://example.com/source?a=1&b=2".to_string(),
                    ),
                ],
                curated_categories: vec!["Protein design".to_string()],
            }],
        }
    }

    fn optional_author_feed() -> Feed {
        let mut feed = sample_feed();
        let mut optional_only = feed.publications[0].clone();
        optional_only.title = "Optional-only paper".to_string();
        optional_only.id = Some("optional-paper".to_string());
        optional_only
            .authors
            .retain(|author| author.name == "Grace Hopper");
        optional_only.authors[0].optional = true;
        feed.publications[0].authors[1].optional = true;
        let mut untracked = optional_only.clone();
        untracked.title = "Untracked collection paper".to_string();
        untracked.id = Some("untracked-paper".to_string());
        untracked.authors.clear();
        feed.publications.extend([optional_only, untracked]);
        feed
    }

    fn checked_authors(html: &str) -> Vec<String> {
        Html::parse_document(html)
            .select(&Selector::parse("input[name=view_author][checked]").unwrap())
            .map(|input| input.value().attr("value").unwrap().to_string())
            .collect()
    }

    fn submitted_form(html: &str) -> Vec<(String, String)> {
        let document = Html::parse_document(html);
        let inputs = Selector::parse("form.filters input[name]").unwrap();
        let selects = Selector::parse("form.filters select[name]").unwrap();
        let selected_option = Selector::parse("option[selected]").unwrap();
        let mut params = document
            .select(&inputs)
            .filter(|input| {
                input.value().attr("disabled").is_none()
                    && (input.value().attr("type") != Some("checkbox")
                        || input.value().attr("checked").is_some())
            })
            .map(|input| {
                (
                    input.value().attr("name").unwrap().to_string(),
                    input.value().attr("value").unwrap_or("").to_string(),
                )
            })
            .collect::<Vec<_>>();
        params.extend(document.select(&selects).map(|select| {
            (
                select.value().attr("name").unwrap().to_string(),
                select
                    .select(&selected_option)
                    .next()
                    .unwrap()
                    .value()
                    .attr("value")
                    .unwrap()
                    .to_string(),
            )
        }));
        params
    }

    #[test]
    fn colliding_author_filters_keep_nonoptional_defaults_when_another_checkbox_changes() {
        let mut feed = sample_feed();
        let template = feed.publications[0].clone();
        feed.publications = [
            ("John A Smith", true),
            ("John B Smith", false),
            ("Ada Lovelace", false),
            ("Grace Hopper", true),
            ("Alan Turing", true),
        ]
        .into_iter()
        .map(|(name, optional)| {
            let mut publication = template.clone();
            publication.id = Some(name.to_string());
            publication.title = format!("{name} paper");
            publication.authors = vec![Author {
                name: name.to_string(),
                filter_id: crate::works::normalize_author_name(name),
                matched_feed: true,
                optional,
            }];
            publication
        })
        .collect();
        for _ in 0..2 {
            let initial = render_feed(&feed, &[]);
            assert_eq!(checked_authors(&initial), ["ada lovelace", "john smith"]);
            assert!(initial.contains("John B Smith paper"));
            assert!(!initial.contains("John A Smith paper"));
            for javascript in [false, true] {
                let mut submitted = submitted_form(&initial);
                submitted.push((AUTHOR_PARAM.to_string(), "grace hopper".to_string()));
                if javascript {
                    submitted.retain(|(name, _)| name != AUTHOR_SELECTION_PARAM);
                    for (name, value) in &mut submitted {
                        if name == AUTHOR_MODE_PARAM {
                            *value = "custom".to_string();
                        }
                    }
                }
                let edited = render_feed(&feed, &submitted);
                assert!(edited.contains(r#"name="view_authors" value="custom""#));
                assert!(edited.contains("John B Smith paper"));
                assert!(edited.contains("Grace Hopper paper"));
                assert!(!edited.contains("Alan Turing paper"));
                assert_eq!(
                    checked_authors(&edited),
                    ["ada lovelace", "grace hopper", "john smith"]
                );
            }
            feed.publications.reverse();
        }
    }

    #[test]
    fn configured_author_form_stays_within_request_limits() {
        let config: crate::config::Config =
            toml::from_str(include_str!("../../feeds.toml")).unwrap();
        let mut feed = sample_feed();
        feed.publications[0].authors = config.feeds["bioml"]
            .people
            .iter()
            .map(|person| Author {
                name: person.name.clone(),
                filter_id: crate::works::normalize_author_name(&person.name),
                matched_feed: true,
                optional: person.optional,
            })
            .collect();
        let html = render_feed(&feed, &[]);
        let mut params = submitted_form(&html);
        let authors = checked_authors(&html);
        assert!(serde_json::to_string(&authors).unwrap().len() > crate::MAX_QUERY_VALUE_LENGTH);
        let fingerprint = params
            .iter()
            .find(|(name, _)| name == AUTHOR_SELECTION_PARAM)
            .unwrap();
        assert_eq!(fingerprint.1.len(), 64);
        for (name, value) in &mut params {
            if name == PERIOD_PARAM {
                *value = "90d".to_string();
            }
        }
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&params)
            .finish();
        assert!(crate::validate_dynamic_request(&query, &params).is_ok());
        assert_eq!(
            ViewFilters::from_params(&params, &FilterOptions::from_feed(&feed)).author_selection,
            AuthorSelection::Default
        );
    }

    #[test]
    fn large_author_forms_stay_within_request_limits() {
        let mut feed = sample_feed();
        feed.publications[0].authors = (0..150)
            .map(|index| Author {
                name: format!("Author {index}"),
                filter_id: format!("author-{index}"),
                matched_feed: true,
                optional: false,
            })
            .collect();
        for mode in ["default", "all", "custom"] {
            let mut initial = vec![(AUTHOR_MODE_PARAM.to_string(), mode.to_string())];
            if mode == "custom" {
                initial.extend(
                    feed.publications[0].authors[..125]
                        .iter()
                        .map(|author| (AUTHOR_PARAM.to_string(), author.filter_id.clone())),
                );
            }
            let mut params = submitted_form(&render_feed(&feed, &initial));
            assert!(params.len() > crate::MAX_QUERY_PARAMS);
            for (name, value) in &mut params {
                if name == PERIOD_PARAM {
                    *value = "90d".to_string();
                }
            }
            let query = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(&params)
                .finish();
            assert!(crate::validate_dynamic_request(&query, &params).is_ok());
            let html = render_feed(&feed, &params);
            assert!(html.contains(&format!(r#"name="view_authors" value="{mode}""#)));
            assert_eq!(
                checked_authors(&html).len(),
                if mode == "custom" { 125 } else { 150 }
            );
        }
    }

    #[test]
    fn custom_author_selection_does_not_broaden_when_sources_shrink() {
        let feed = optional_author_feed();
        let initial = vec![
            (AUTHOR_MODE_PARAM.to_string(), "custom".to_string()),
            (AUTHOR_PARAM.to_string(), "ada lovelace".to_string()),
        ];
        let submitted = submitted_form(&render_feed(&feed, &initial));
        let mut narrower = feed.clone();
        for publication in &mut narrower.publications {
            publication
                .authors
                .retain(|author| author.filter_id == "ada lovelace");
        }
        for params in [&initial, &submitted] {
            let html = render_feed(&narrower, params);
            assert!(html.contains(r#"name="view_authors" value="custom""#));
            assert!(!html.contains("Untracked collection paper"));
            assert!(!html.contains("Optional-only paper"));
            let restored = render_feed(&feed, &submitted_form(&html));
            assert_eq!(checked_authors(&restored), ["ada lovelace"]);
            assert!(!restored.contains("Optional-only paper"));
            assert!(!restored.contains("Untracked collection paper"));
        }
        let mut no_authors = feed.clone();
        no_authors
            .publications
            .retain(|paper| paper.authors.is_empty());
        let html = render_feed(&no_authors, &submitted);
        assert!(html.contains(r#"name="view_authors" value="custom""#));
        assert!(html.contains("0 of 1 publications"));
    }

    #[test]
    fn no_javascript_author_edits_use_original_options_across_source_changes() {
        let feed = optional_author_feed();
        let mut expanded = feed.clone();
        let mut added = feed.publications[1].clone();
        added.title = "New source paper".to_string();
        added.authors[0].name = "New author".to_string();
        added.authors[0].filter_id = "new-author".to_string();
        expanded.publications.push(added);
        for mode in ["default", "custom"] {
            let mut initial = vec![(AUTHOR_MODE_PARAM.to_string(), mode.to_string())];
            if mode == "custom" {
                initial.push((AUTHOR_PARAM.to_string(), "ada lovelace".to_string()));
            }
            let mut params = submitted_form(&render_feed(&feed, &initial));
            params.push((AUTHOR_PARAM.to_string(), "grace-hopper".to_string()));
            let html = render_feed(&expanded, &params);
            assert!(html.contains(r#"name="view_authors" value="all""#));
            assert!(html.contains("New source paper"));
            assert!(html.contains("Untracked collection paper"));
            assert_eq!(checked_authors(&html).len(), 3);
            assert_eq!(raw_feed_url(&params), "?paper_sources=custom&rss");
            assert_eq!(unfiltered_reader_url(&params), "/?paper_sources=custom");
        }

        let mut params = submitted_form(&render_feed(
            &feed,
            &[(AUTHOR_MODE_PARAM.to_string(), "all".to_string())],
        ));
        params.retain(|(name, value)| name != AUTHOR_PARAM || value != "grace-hopper");
        let mut narrower = feed.clone();
        for publication in &mut narrower.publications {
            publication
                .authors
                .retain(|author| author.filter_id == "ada lovelace");
        }
        let html = render_feed(&narrower, &params);
        assert!(html.contains(r#"name="view_authors" value="custom""#));
        assert!(!html.contains("Untracked collection paper"));
    }

    #[test]
    fn all_selection_survives_source_changes_and_supports_no_javascript_edits() {
        let mut feed = optional_author_feed();
        let html = render_feed(&feed, &[(AUTHOR_MODE_PARAM.to_string(), "all".to_string())]);
        let params = submitted_form(&html);
        assert!(params.contains(&(AUTHOR_MODE_PARAM.to_string(), "all".to_string())));
        let mut added = feed.publications[0].clone();
        added.id = Some("new-source-paper".to_string());
        added.title = "New source paper".to_string();
        added.authors = vec![Author {
            name: "New tracked author".to_string(),
            filter_id: "new author".to_string(),
            matched_feed: true,
            optional: true,
        }];
        feed.publications.push(added);

        let all = render_feed(&feed, &params);
        assert_eq!(
            checked_authors(&all),
            ["ada lovelace", "grace-hopper", "new author"]
        );
        assert!(all.contains("Untracked collection paper"));
        assert!(all.contains("New source paper"));
        assert!(all.contains(r#"name="view_authors" value="all""#));
        assert_eq!(raw_feed_url(&params), "?paper_sources=custom&rss");

        let mut edited = params.clone();
        edited.retain(|(name, value)| name != AUTHOR_PARAM || value != "grace-hopper");
        let custom = render_feed(&feed, &edited);
        assert_eq!(checked_authors(&custom), ["ada lovelace"]);
        assert!(!custom.contains("Optional-only paper"));
        assert!(!custom.contains("New source paper"));
        assert!(!custom.contains("Untracked collection paper"));
        assert!(custom.contains(r#"name="view_authors" value="custom""#));

        edited.retain(|(name, _)| name != AUTHOR_PARAM);
        assert!(render_feed(&feed, &edited).contains("0 of 4 publications"));
    }

    #[test]
    fn all_selection_survives_a_source_with_no_tracked_authors() {
        let mut feed = optional_author_feed();
        feed.publications
            .retain(|publication| publication.authors.is_empty());
        let html = render_feed(&feed, &[(AUTHOR_MODE_PARAM.to_string(), "all".to_string())]);
        let params = submitted_form(&html);
        assert!(params.contains(&(AUTHOR_MODE_PARAM.to_string(), "all".to_string())));
        assert!(checked_authors(&html).is_empty());
        let restored = render_feed(&optional_author_feed(), &params);
        assert_eq!(checked_authors(&restored), ["ada lovelace", "grace-hopper"]);
        assert!(restored.contains("Optional-only paper"));
        assert!(restored.contains("Untracked collection paper"));
    }

    #[test]
    fn default_reader_selects_regular_authors_and_hides_optional_only_papers() {
        let html = render_feed(&optional_author_feed(), &[]);

        assert_eq!(checked_authors(&html), ["ada lovelace"]);
        assert!(html.contains("2 of 3 publications"));
        assert!(!html.contains("Optional-only paper"));
        assert!(html.contains("Untracked collection paper"));
        assert!(
            html.contains(r#"name="view_author" value="grace-hopper" data-label="Grace Hopper">"#)
        );
        assert!(html.contains(r#"name="view_authors" value="default""#));
        let document = Html::parse_document(&html);
        let defaults_input = Selector::parse("input[name=view_author_selection]").unwrap();
        assert_eq!(
            document
                .select(&defaults_input)
                .next()
                .unwrap()
                .value()
                .attr("value"),
            Some(author_selection_fingerprint(&["ada lovelace".to_string()]).as_str())
        );
        assert!(!html.contains("Clear filters"));
    }

    #[test]
    fn explicit_selection_can_include_optional_authors_or_no_authors() {
        let feed = optional_author_feed();
        let params = vec![
            (AUTHOR_MODE_PARAM.to_string(), "custom".to_string()),
            (AUTHOR_PARAM.to_string(), "grace-hopper".to_string()),
        ];
        let html = render_feed(&feed, &params);
        assert_eq!(checked_authors(&html), ["grace-hopper"]);
        assert!(html.contains("Optional-only paper"));
        assert!(!html.contains("Untracked collection paper"));
        assert!(html.contains("2 of 3 publications"));
        assert_eq!(raw_feed_url(&params), "?rss");
        assert_eq!(unfiltered_reader_url(&params), "/");
        let article = render_article(&feed, "optional-paper", &params).unwrap();
        assert!(article.contains(r#"href="/?view_authors=custom&amp;view_author=grace-hopper""#));

        let none = render_feed(
            &feed,
            &[(AUTHOR_MODE_PARAM.to_string(), "custom".to_string())],
        );
        assert!(checked_authors(&none).is_empty());
        assert!(none.contains("0 of 3 publications"));
        assert!(none.contains("No tracked authors"));

        let all = render_feed(&feed, &[(AUTHOR_MODE_PARAM.to_string(), "all".to_string())]);
        assert_eq!(checked_authors(&all), ["ada lovelace", "grace-hopper"]);
        assert!(all.contains("3 of 3 publications"));
        assert!(all.contains("Untracked collection paper"));
    }

    #[test]
    fn author_form_submission_preserves_defaults_and_supports_no_javascript_changes() {
        let feed = optional_author_feed();
        let mut params = submitted_form(&render_feed(&feed, &[]));
        let defaults = render_feed(&feed, &params);
        assert!(!defaults.contains("Optional-only paper"));
        assert!(defaults.contains("Untracked collection paper"));

        params.push((AUTHOR_PARAM.to_string(), "grace-hopper".to_string()));
        let all = render_feed(&feed, &params);
        assert!(all.contains("Optional-only paper"));
        assert!(all.contains("Untracked collection paper"));

        params.retain(|(name, _)| name != AUTHOR_PARAM);
        let none = render_feed(&feed, &params);
        assert!(none.contains("0 of 3 publications"));
    }

    #[test]
    fn defaults_survive_paper_source_changes_that_add_author_options() {
        let mut feed = optional_author_feed();
        let params = vec![
            (AUTHOR_MODE_PARAM.to_string(), "default".to_string()),
            (
                AUTHOR_SELECTION_PARAM.to_string(),
                author_selection_fingerprint(&["ada lovelace".to_string()]),
            ),
            (AUTHOR_PARAM.to_string(), "ada lovelace".to_string()),
        ];
        feed.publications[2].authors.push(Author {
            name: "New tracked author".to_string(),
            filter_id: "new author".to_string(),
            matched_feed: true,
            optional: false,
        });
        let html = render_feed(&feed, &params);
        assert_eq!(checked_authors(&html), ["ada lovelace", "new author"]);
        assert!(!html.contains("Optional-only paper"));
        assert!(html.contains("Untracked collection paper"));
        assert_eq!(raw_feed_url(&params), "?rss");

        let explicit_default = render_feed(
            &feed,
            &[(AUTHOR_MODE_PARAM.to_string(), "default".to_string())],
        );
        assert_eq!(
            checked_authors(&explicit_default),
            ["ada lovelace", "new author"]
        );
    }

    #[test]
    fn all_optional_defaults_keep_untracked_papers_even_after_form_submission() {
        let mut feed = optional_author_feed();
        for publication in &mut feed.publications {
            for author in &mut publication.authors {
                author.optional = true;
            }
        }
        for params in [
            vec![],
            vec![(AUTHOR_MODE_PARAM.to_string(), "default".to_string())],
        ] {
            let html = render_feed(&feed, &params);
            assert!(checked_authors(&html).is_empty());
            assert!(html.contains("1 of 3 publications"));
            assert!(html.contains("Untracked collection paper"));
            assert!(!html.contains("Optional-only paper"));
        }
    }

    #[test]
    fn authors_without_optional_flags_are_all_checked_without_excluding_other_sources() {
        let mut feed = optional_author_feed();
        for publication in &mut feed.publications {
            for author in &mut publication.authors {
                author.optional = false;
            }
        }
        let html = render_feed(&feed, &[]);
        assert_eq!(checked_authors(&html), ["ada lovelace", "grace-hopper"]);
        assert!(html.contains("3 publications"));
        assert!(html.contains("Untracked collection paper"));
    }

    #[test]
    fn renders_optional_native_categories_unselected() {
        let mut feed = sample_feed();
        feed.paper_sources = vec![
            super::super::PaperSourceOption {
                value: "core".to_string(),
                label: "Tracked authors and journals".to_string(),
                group: "Core feed",
                selected: true,
            },
            super::super::PaperSourceOption {
                value: "biorxiv:synthetic_biology".to_string(),
                label: "Synthetic Biology".to_string(),
                group: "bioRxiv",
                selected: false,
            },
            super::super::PaperSourceOption {
                value: "arxiv:q-bio.BM".to_string(),
                label: "Biomolecules (q-bio.BM)".to_string(),
                group: "arXiv",
                selected: false,
            },
        ];

        let html = render_feed(&feed, &[]);

        assert!(html.contains("Paper sources"));
        assert!(html.contains(r#"name="paper_sources" value="custom""#));
        assert!(!html.contains("No sources"));
        assert!(html.contains(
            r#"name="paper_source" value="core" data-label="Tracked authors and journals" checked"#
        ));
        assert!(html.contains(r#"name="paper_source" value="biorxiv:synthetic_biology""#));
        assert!(html.contains(r#"name="paper_source" value="arxiv:q-bio.BM""#));
        assert!(!html.contains(
            r#"value="biorxiv:synthetic_biology" data-label="Synthetic Biology" checked"#
        ));
    }

    #[test]
    fn renders_native_preprint_provenance() {
        let mut feed = sample_feed();
        feed.publications[0]
            .discovery_sources
            .push(DiscoverySource::arxiv_category("q-bio.BM", "Biomolecules"));

        let html = render_feed(&feed, &[]);

        assert!(html.contains("Preprint source:"));
        assert!(html.contains("arXiv: Biomolecules"));
        assert!(html.contains("https://arxiv.org/list/q-bio.BM/recent"));
    }

    #[test]
    fn raw_feed_url_preserves_existing_parameters() {
        let params = vec![
            ("feed".to_string(), "my field".to_string()),
            ("author_id".to_string(), "A1".to_string()),
            (
                BIORXIV_CATEGORY_PARAM.to_string(),
                "synthetic_biology".to_string(),
            ),
            (PAPER_SOURCE_MODE_PARAM.to_string(), "custom".to_string()),
            (PAPER_SOURCE_PARAM.to_string(), "arxiv:q-bio.BM".to_string()),
            (PERIOD_PARAM.to_string(), "90d".to_string()),
            (AUTHOR_PARAM.to_string(), "ada-lovelace".to_string()),
        ];

        assert_eq!(
            raw_feed_url(&params),
            "?feed=my+field&author_id=A1&biorxiv_category=synthetic_biology&paper_sources=custom&paper_source=arxiv%3Aq-bio.BM&rss"
        );
        assert_eq!(raw_feed_url(&[]), "?rss");
    }

    #[test]
    fn filters_publications_by_tracked_author_and_curated_source() {
        let mut feed = sample_feed();
        let mut second_publication = feed.publications[0].clone();
        second_publication.id = Some("https://openalex.org/W2".to_string());
        second_publication.title = "Second publication".to_string();
        second_publication
            .discovery_sources
            .retain(|source| !source.is_curated_collection());
        feed.publications.push(second_publication);
        let params = vec![
            (AUTHOR_PARAM.to_string(), "ada lovelace".to_string()),
            (SOURCE_PARAM.to_string(), "curated-source".to_string()),
        ];

        let html = render_feed(&feed, &params);

        assert!(html.contains("1 of 2 publications"));
        assert!(html.contains("&lt;script&gt;alert(&#39;title&#39;)&lt;/script&gt;"));
        assert!(!html.contains("Second publication"));
        assert!(html.contains(
            r#"<input type="checkbox" name="view_author" value="ada lovelace" data-label="Ada Lovelace" checked>"#
        ));
        assert!(!html.contains("Collection source"));
        assert!(html.contains("?view_author=ada+lovelace&amp;view_source=curated-source"));
        assert!(html.contains(r#"<script src="/reader.js?v=7" defer></script>"#));
        assert!(html.contains(
            r#"<label class="filter-checkbox filter-select-all" hidden><input type="checkbox" data-select-all><span>All tracked authors</span>"#
        ));
    }

    #[test]
    fn unknown_filter_values_are_ignored() {
        let params = vec![
            (AUTHOR_PARAM.to_string(), "unknown-author".to_string()),
            (SOURCE_PARAM.to_string(), "unknown-source".to_string()),
        ];

        let html = render_feed(&sample_feed(), &params);

        assert!(html.contains("1 publication"));
        assert!(!html.contains("1 of 1 publications"));
        assert!(!html.contains("Clear filters"));
    }

    #[test]
    fn filters_publications_by_relative_publication_date() {
        let mut feed = sample_feed();
        let today = chrono::Utc::now().date_naive();
        feed.publications[0].publication_date = Some(today.format("%Y-%m-%d").to_string());
        let mut old_publication = feed.publications[0].clone();
        old_publication.id = Some("https://openalex.org/W2".to_string());
        old_publication.title = "Older publication".to_string();
        old_publication.publication_date = Some(
            (today - chrono::Duration::days(31))
                .format("%Y-%m-%d")
                .to_string(),
        );
        feed.publications.push(old_publication);

        let html = render_feed(&feed, &[(PERIOD_PARAM.to_string(), "30d".to_string())]);

        assert!(html.contains("1 of 2 publications"));
        assert!(!html.contains("Older publication"));
    }

    #[test]
    fn reader_escapes_content_and_links_to_article_preview() {
        let params = vec![("feed".to_string(), "myfield".to_string())];
        let html = render_feed(&sample_feed(), &params);

        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Research &amp; &lt;Development&gt;"));
        assert!(html.contains("&lt;script&gt;alert(&#39;title&#39;)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert"));
        assert!(
            html.contains("/article/68747470733a2f2f6f70656e616c65782e6f72672f5731?feed=myfield")
        );
        assert!(html.contains("href=\"?feed=myfield&amp;rss\""));
        assert!(html.contains(r#"<link rel="icon" href="/favicon.svg" type="image/svg+xml">"#));
        assert!(html.contains(
            r#"<link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png">"#
        ));
        assert!(html.contains("Curated by"));
        assert!(html.contains("https://example.com/source?a=1&amp;b=2"));
        assert!(!html.contains("Exclude collection-only papers"));
        assert!(html.contains(
            r#"<label class="filter-checkbox filter-select-all" hidden><input type="checkbox" data-select-all checked><span>All tracked authors</span>"#
        ));
    }

    #[test]
    fn feed_uses_list_semantics_instead_of_article_candidates() {
        let mut feed = sample_feed();
        let mut second_publication = feed.publications[0].clone();
        second_publication.id = Some("https://openalex.org/W2".to_string());
        second_publication.title = "Second publication".to_string();
        feed.publications.push(second_publication);

        let document = Html::parse_document(&render_feed(&feed, &[]));
        let list = Selector::parse("main > ol.publication-list").unwrap();
        let publication = Selector::parse("ol.publication-list > li.publication").unwrap();
        let article = Selector::parse("article").unwrap();

        assert_eq!(document.select(&list).count(), 1);
        assert_eq!(
            document.select(&publication).count(),
            feed.publications.len()
        );
        assert_eq!(document.select(&article).count(), 0);
    }

    #[test]
    fn empty_feed_still_renders_valid_list_content() {
        let mut feed = sample_feed();
        feed.publications.clear();

        let document = Html::parse_document(&render_feed(&feed, &[]));
        let empty_item = Selector::parse("ol.publication-list > li.empty").unwrap();

        assert_eq!(document.select(&empty_item).count(), 1);
    }

    #[test]
    fn article_preview_links_externally_and_highlights_all_matching_authors() {
        let params = vec![
            ("feed".to_string(), "myfield".to_string()),
            (PERIOD_PARAM.to_string(), "90d".to_string()),
        ];
        let html = render_article(&sample_feed(), "https://openalex.org/W1", &params).unwrap();
        let document = Html::parse_document(&html);
        let article = Selector::parse("main > article.article-detail").unwrap();
        let publication_list = Selector::parse("ol.publication-list").unwrap();

        assert!(html.contains("https://example.com/?a=1&amp;b=2"));
        assert!(html.contains(
            r#"<a class="secondary-action" href="https://example.com/article.pdf?a=1&amp;b=2" target="_blank" rel="external noopener">Open PDF"#
        ));
        assert!(html.contains("&lt;strong&gt;abstract&lt;/strong&gt;"));
        assert!(html.contains(r#"<link rel="icon" href="/favicon.svg" type="image/svg+xml">"#));
        assert!(html.contains(
            r#"<link rel="apple-touch-icon" sizes="180x180" href="/apple-touch-icon.png">"#
        ));
        assert_eq!(document.select(&article).count(), 1);
        assert_eq!(document.select(&publication_list).count(), 0);
        assert!(html.contains(
            r#"<strong class="notable-author">Ada Lovelace</strong>, <strong class="notable-author">Grace Hopper</strong>, Alan Turing"#
        ));
        assert!(html.contains(r#"class="back" href="/?feed=myfield&amp;view_period=90d""#));
    }

    #[test]
    fn article_actions_open_new_tabs_without_changing_internal_navigation() {
        let html = render_article(&sample_feed(), "https://openalex.org/W1", &[]).unwrap();
        let document = Html::parse_document(&html);
        let external_actions = Selector::parse("a.primary-action, a.secondary-action").unwrap();
        let links = document.select(&external_actions).collect::<Vec<_>>();

        assert_eq!(links.len(), 2);
        for link in links {
            assert_eq!(link.value().attr("target"), Some("_blank"));
            let rel = link.value().attr("rel").unwrap();
            assert!(rel.split_whitespace().any(|value| value == "external"));
            assert!(rel.split_whitespace().any(|value| value == "noopener"));
        }

        let internal_links =
            Selector::parse("a.back, .actions a:not(.primary-action):not(.secondary-action)").unwrap();
        let links = document.select(&internal_links).collect::<Vec<_>>();
        assert_eq!(links.len(), 2);
        for link in links {
            assert_eq!(link.value().attr("target"), None);
        }
    }

    #[test]
    fn labels_collection_date_without_presenting_it_as_publication_date() {
        let mut feed = sample_feed();
        feed.publications[0].publication_date = None;
        feed.publications[0].collection_date = Some(CollectionDate {
            date: "2025-05-03".to_string(),
            commit_url: "https://github.com/example/repo/commit/abc".to_string(),
        });

        let html = render_feed(&feed, &[]);

        assert!(html.contains("Added to collection"));
        assert!(html.contains("<time>May 3, 2025</time>"));
        assert!(html.contains("https://github.com/example/repo/commit/abc"));
    }

    #[test]
    fn article_paths_have_unique_reader_cache_keys_and_round_trip_ids() {
        let first_id = "https://example.com/articles/β-catenin?id=1";
        let second_id = "https://example.com/articles/β-catenin?id=2";
        let first_url = article_url(&[], first_id);
        let second_url = article_url(&[], second_id);
        let first_path = first_url.split('?').next().unwrap();
        let second_path = second_url.split('?').next().unwrap();

        assert!(first_path.starts_with("/article/"));
        assert_ne!(first_path, second_path);
        assert_eq!(article_id_from_path(first_path).as_deref(), Some(first_id));
        assert_eq!(
            article_id_from_path(second_path).as_deref(),
            Some(second_id)
        );
        assert_eq!(article_id_from_path("/"), None);
        assert_eq!(article_id_from_path("/article/"), None);
        assert_eq!(article_id_from_path("/article/not-hex"), None);
        assert_eq!(article_id_from_path("/article/ff"), None);
    }
}
