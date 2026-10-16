use super::filters::ViewFilters;
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
</head>
<body><main>{body}</main></body>
</html>
"#
    )
}

pub(crate) fn publication_card(site: &Site, article: &Article) -> String {
    let publication = &article.publication;
    let (date, separator, venue) = render_metadata(publication);
    format!(
        r#"<li class="publication" data-article="{}"><p class="metadata">{date}{separator}{venue}</p><h2><a href="{}">{}</a></h2>{}{}</li>"#,
        article.id,
        escape_html(&site.path(&format!("article/{}/", article.id))),
        escape_html(&publication.title),
        render_authors(&publication.authors),
        render_provenance(publication),
    )
}

pub(crate) fn feed(site: &Site, feed: &FeedDocument, navigation: &[(String, String)]) -> String {
    let articles = feed
        .articles
        .iter()
        .filter(|article| {
            ViewFilters::default().matches_on(&article.publication, chrono::Utc::now().date_naive())
        })
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
    let body = format!(
        r#"<header><nav aria-label="Feeds">{navigation}</nav><h1>{}</h1><p class="intro">{}</p><p class="feed-meta"><a href="{}">Subscribe to this feed</a></p><p>Snapshot generated <time>{}</time>. Source data may be older.</p></header><ol class="publication-list">{articles}</ol>"#,
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
