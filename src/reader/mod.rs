mod filters;
mod render;

use crate::provenance::DiscoverySource;
use serde::{Deserialize, Serialize};

pub(crate) use filters::AUTHOR_PARAM;
pub use render::{
    article_id_from_path, render_article, render_feed, FAVICON, READER_CSS, READER_JS,
};

#[derive(Clone, Debug)]
pub struct Feed {
    pub title: String,
    pub description: String,
    pub publications: Vec<Publication>,
    pub paper_sources: Vec<PaperSourceOption>,
}

#[derive(Clone, Debug)]
pub struct PaperSourceOption {
    pub value: String,
    pub label: String,
    pub group: &'static str,
    pub selected: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Publication {
    pub id: Option<String>,
    pub title: String,
    pub link: Option<String>,
    pub pdf_url: Option<String>,
    pub publication_date: Option<String>,
    pub collection_date: Option<CollectionDate>,
    pub venue: Option<String>,
    pub authors: Vec<Author>,
    pub abstract_text: Option<String>,
    pub discovery_sources: Vec<DiscoverySource>,
    pub curated_categories: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Author {
    pub name: String,
    pub filter_id: String,
    pub matched_feed: bool,
    pub optional: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CollectionDate {
    pub date: String,
    pub commit_url: String,
}
