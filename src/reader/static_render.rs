use super::filters::{FilterOptions, ViewFilters};
use super::render::{escape_html, render_authors, render_metadata, render_provenance};
use crate::static_export::{Article, FeedDocument, Site};

fn layout(site: &Site, title: &str, rss: &str, body: &str) -> String {
    let title = escape_html(title);
    let rss = escape_html(rss);
    let root = escape_html(site.base_path());
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title}</title>
  <link rel="icon" href="{root}favicon.svg" type="image/svg+xml">
  <link rel="apple-touch-icon" sizes="180x180" href="{root}apple-touch-icon.png">
  <link rel="stylesheet" href="{root}reader.css">
  <link rel="alternate" type="application/rss+xml" title="{title}" href="{rss}">
  <script type="module" src="{root}static-reader.mjs"></script>
</head>
<body><main data-static-root="{root}">{body}</main></body>
</html>
"#
    )
}

pub(crate) fn publication_card(site: &Site, article: &Article) -> String {
    let publication = &article.publication;
    let (date, separator, venue) = render_metadata(publication);
    let default_visible =
        ViewFilters::default().matches_on(publication, chrono::Utc::now().date_naive());
    let hidden = if default_visible { "" } else { " hidden" };
    let authors = publication
        .authors
        .iter()
        .filter(|author| author.matched_feed)
        .map(|author| &author.filter_id)
        .collect::<Vec<_>>();
    let sources = publication
        .discovery_sources
        .iter()
        .map(|source| match source.kind {
            crate::provenance::DiscoverySourceKind::OpenAlex
            | crate::provenance::DiscoverySourceKind::GoogleScholar => "core".to_string(),
            crate::provenance::DiscoverySourceKind::CuratedCollection => {
                format!("curated:{}", source.key)
            }
            _ => source.key.clone(),
        })
        .collect::<Vec<_>>();
    let author_json = escape_html(&serde_json::to_string(&authors).expect("author IDs serialize"));
    let source_json = escape_html(&serde_json::to_string(&sources).expect("source IDs serialize"));
    format!(
        r#"<li class="publication" data-article="{}" data-date="{}" data-default-author="{default_visible}" data-authors="{author_json}" data-sources="{source_json}"{hidden}><p class="metadata">{date}{separator}{venue}</p><h2><a href="{}">{}</a></h2>{}{}</li>"#,
        article.id,
        escape_html(publication.publication_date.as_deref().unwrap_or("")),
        escape_html(&site.path(&format!("article/{}/", article.id))),
        escape_html(&publication.title),
        render_authors(&publication.authors),
        render_provenance(publication),
    )
}

pub(crate) fn feed(site: &Site, feed: &FeedDocument, navigation: &[(String, String)]) -> String {
    let visible = feed
        .articles
        .iter()
        .filter(|article| {
            ViewFilters::default().matches_on(&article.publication, chrono::Utc::now().date_naive())
        })
        .count();
    let empty_hidden = if visible == 0 { "" } else { " hidden" };
    let articles = feed
        .articles
        .iter()
        .map(|article| publication_card(site, article))
        .collect::<String>();
    let articles = if articles.is_empty() {
        r#"<li class="empty">No recent publications found.</li>"#.to_string()
    } else {
        articles
    };
    let navigation = navigation
        .iter()
        .map(|(name, title)| {
            format!(
                r#"<a href="{}">{}</a>"#,
                escape_html(&site.path(&format!("feeds/{name}/"))),
                escape_html(title)
            )
        })
        .collect::<Vec<_>>()
        .join(" · ");
    let filters = filter_controls(feed, visible);
    let body = format!(
        r#"<header><nav aria-label="Feeds">{navigation}</nav><h1>{}</h1><p class="intro">{}</p><p class="feed-meta"><a href="{}">Subscribe to this feed</a></p><p>Snapshot generated <time>{}</time>. Source data may be older.</p>{filters}</header><ol class="publication-list">{articles}</ol><p data-filter-empty{empty_hidden}>No publications match these filters.</p>"#,
        escape_html(&feed.title),
        escape_html(&feed.description),
        escape_html(&site.path(&format!("feeds/{}/feed.xml", feed.name))),
        escape_html(&feed.generated_at),
    );
    layout(
        site,
        &feed.title,
        &site.path(&format!("feeds/{}/feed.xml", feed.name)),
        &body,
    )
}

fn filter_controls(feed: &FeedDocument, visible: usize) -> String {
    let total = feed.articles.len();
    let options = FilterOptions::from_feed(&super::Feed {
        title: feed.title.clone(),
        description: feed.description.clone(),
        paper_sources: vec![],
        publications: feed
            .articles
            .iter()
            .map(|article| article.publication.clone())
            .collect(),
    });
    let authors = options.authors.iter().map(|(id, label)| {
        let checked = if options.optional_authors.contains(id) { "" } else { " checked" };
        format!(r#"<label class="filter-checkbox"><input type="checkbox" name="author" value="{}" data-default="{}"{checked}>{}</label>"#,
            escape_html(id), !options.optional_authors.contains(id), escape_html(label))
    }).collect::<String>();
    let sources = feed.sources.iter().map(|source| format!(
        r#"<label class="filter-checkbox"><input type="checkbox" name="source" value="{}" checked>{}</label>"#,
        escape_html(&source.value), escape_html(&source.label)
    )).collect::<String>();
    format!(
        r#"<p data-filter-notice>Enable JavaScript to filter this snapshot.</p>
<p data-filter-error role="alert" hidden></p>
<form class="filters" data-static-filters hidden>
<div class="filter-fields">
<label class="filter-select">Publication date<select name="period"><option value="">All available</option><option value="30d">Past 30 days</option><option value="90d">Past 90 days</option><option value="1y">Past year</option></select></label>
<label class="filter-select">Author selection<select name="authors"><option value="default">Configured defaults</option><option value="all">All tracked authors</option><option value="custom">Custom selection</option></select></label>
<details class="filter-multiselect"><summary>Tracked authors</summary><div class="filter-options">{authors}</div></details>
<details class="filter-multiselect"><summary>Paper sources</summary><div class="filter-options">{sources}</div></details>
</div><div class="filter-actions"><button type="submit">Apply filters</button><button type="reset">Clear filters</button></div></form>
<p data-filter-count aria-live="polite">{visible} of {total} publications</p>
<p>Subscriptions include all articles in this named feed; browser filters do not change RSS.</p>"#
    )
}

pub(crate) fn article(site: &Site, article: &Article) -> String {
    let publication = &article.publication;
    let (date, separator, venue) = render_metadata(publication);
    let link = |url: &Option<String>, label| {
        url.as_deref()
            .map(|url| {
                format!(
                    r#"<a href="{}" target="_blank" rel="external noopener">{label}</a>"#,
                    escape_html(url)
                )
            })
            .unwrap_or_default()
    };
    let body = format!(
        r#"<a class="back" href="{}">Back to publications</a><article class="article-detail"><p class="metadata">{date}{separator}{venue}</p><h1>{}</h1>{}{}<section class="abstract" aria-labelledby="abstract-heading"><h2 id="abstract-heading">Abstract</h2><p>{}</p></section><p class="actions">{}{}</p></article>"#,
        escape_html(&site.path(&format!("feeds/{}/", article.feed))),
        escape_html(&publication.title),
        render_authors(&publication.authors),
        render_provenance(publication),
        escape_html(
            publication
                .abstract_text
                .as_deref()
                .unwrap_or("No abstract is available for this publication.")
        ),
        link(&publication.link, "Read full article"),
        link(&publication.pdf_url, "Open PDF"),
    );
    layout(
        site,
        &publication.title,
        &site.path(&format!("feeds/{}/feed.xml", article.feed)),
        &body,
    )
}

pub(crate) fn not_found(site: &Site) -> String {
    layout(
        site,
        "Page not found",
        &site.path("feed.xml"),
        &format!(
            r#"<h1>Page not found</h1><p>This URL may belong to the retired reader.</p><a href="{}">Open the reading site</a>"#,
            escape_html(site.base_path())
        ),
    )
}
