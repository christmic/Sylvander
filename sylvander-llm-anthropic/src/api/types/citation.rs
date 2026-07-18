//! Citations — strong-typed variants returned in text block
//! `citations` fields and `citations_delta` stream events.
//!
//! All variants carry `cited_text` plus location-specific metadata.
//! Discriminated by `type`.

use serde::{Deserialize, Serialize};

/// Strongly-typed citation. Discriminated union over the 5 location
/// types the API can return.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TextCitation {
    /// Character-level citation in a document.
    CharLocation(CitationCharLocation),
    /// Page-level citation in a PDF document.
    PageLocation(CitationPageLocation),
    /// Content-block-level citation (the minimal citable unit).
    ContentBlockLocation(CitationContentBlockLocation),
    /// Citation into a search result.
    SearchResultLocation(CitationsSearchResultLocation),
    /// Citation into a web search result.
    WebSearchResultLocation(CitationsWebSearchResultLocation),
}

/// Character-range citation inside a document. The `type: "char_location"`
/// discriminator is carried by the parent [`TextCitation`] enum tag, not
/// on this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationCharLocation {
    /// The cited text.
    pub cited_text: String,
    /// Index of the document in the request.
    pub document_index: u32,
    /// Start character index (inclusive).
    pub start_char_index: u32,
    /// End character index (exclusive).
    pub end_char_index: u32,
    /// Optional document title (when known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,
    /// Optional file ID for file-based documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
}

/// Page-range citation in a PDF document. The `type: "page_location"`
/// discriminator is carried by the parent [`TextCitation`] enum tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationPageLocation {
    /// The cited text.
    pub cited_text: String,
    /// Index of the document in the request.
    pub document_index: u32,
    /// Start page number (inclusive).
    pub start_page_number: u32,
    /// End page number (inclusive).
    pub end_page_number: u32,
    /// Optional document title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,
    /// Optional file ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
}

/// Content-block-range citation (the minimal citable unit in a
/// document). The `type: "content_block_location"` discriminator is
/// carried by the parent [`TextCitation`] enum tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationContentBlockLocation {
    /// The full text of the cited block range, concatenated.
    pub cited_text: String,
    /// Index of the document in the request.
    pub document_index: u32,
    /// Start block index (inclusive).
    pub start_block_index: u32,
    /// End block index (exclusive).
    pub end_block_index: u32,
    /// Optional document title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,
    /// Optional file ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
}

/// Citation into a search result. The `type: "search_result_location"`
/// discriminator is carried by the parent [`TextCitation`] enum tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationsSearchResultLocation {
    /// The cited text from the search result.
    pub cited_text: String,
    /// Index of the search result within the source block.
    pub search_result_index: u32,
    /// Index of the source block in the document.
    pub source_block_index: u32,
    /// Optional document title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_title: Option<String>,
    /// Optional file ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
}

/// Citation into a web search result. The
/// `type: "web_search_result_location"` discriminator is carried by the
/// parent [`TextCitation`] enum tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationsWebSearchResultLocation {
    /// The cited text from the web search result.
    pub cited_text: String,
    /// Encrypted index of the web search result.
    pub encrypted_index: String,
    /// Optional title of the web search result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional URL of the web search result (older models).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[cfg(test)]
#[path = "../../../tests/unit/api_types_citation.rs"]
mod tests;
