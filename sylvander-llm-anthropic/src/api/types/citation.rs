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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn char_location_round_trip() {
        let citation = TextCitation::CharLocation(CitationCharLocation {
            cited_text: "hello world".to_string(),
            document_index: 0,
            start_char_index: 6,
            end_char_index: 11,
            document_title: Some("Doc".to_string()),
            file_id: None,
        });
        let json = serde_json::to_string(&citation).unwrap();
        assert_eq!(
            json,
            r#"{"type":"char_location","cited_text":"hello world","document_index":0,"start_char_index":6,"end_char_index":11,"document_title":"Doc"}"#
        );
        let back: TextCitation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, citation);
    }

    #[test]
    fn page_location_round_trip() {
        let citation = TextCitation::PageLocation(CitationPageLocation {
            cited_text: "page 2 content".to_string(),
            document_index: 0,
            start_page_number: 2,
            end_page_number: 2,
            document_title: None,
            file_id: Some("file_abc".to_string()),
        });
        let json = serde_json::to_string(&citation).unwrap();
        let back: TextCitation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, citation);
    }

    #[test]
    fn content_block_location_round_trip() {
        let citation = TextCitation::ContentBlockLocation(CitationContentBlockLocation {
            cited_text: "block 1".to_string(),
            document_index: 0,
            start_block_index: 0,
            end_block_index: 1,
            document_title: None,
            file_id: None,
        });
        let json = serde_json::to_string(&citation).unwrap();
        let back: TextCitation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, citation);
    }

    #[test]
    fn search_result_location_round_trip() {
        let citation = TextCitation::SearchResultLocation(CitationsSearchResultLocation {
            cited_text: "match".to_string(),
            search_result_index: 0,
            source_block_index: 5,
            document_title: None,
            file_id: None,
        });
        let json = serde_json::to_string(&citation).unwrap();
        let back: TextCitation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, citation);
    }

    #[test]
    fn web_search_result_location_round_trip() {
        let citation = TextCitation::WebSearchResultLocation(CitationsWebSearchResultLocation {
            cited_text: "excerpt".to_string(),
            encrypted_index: "enc_xyz".to_string(),
            title: Some("Page Title".to_string()),
            url: None,
        });
        let json = serde_json::to_string(&citation).unwrap();
        let back: TextCitation = serde_json::from_str(&json).unwrap();
        assert_eq!(back, citation);
    }

    #[test]
    fn deserialize_from_wire_json() {
        let variants = [
            json!({"type": "char_location", "cited_text": "x", "document_index": 0, "start_char_index": 0, "end_char_index": 1}),
            json!({"type": "page_location", "cited_text": "x", "document_index": 0, "start_page_number": 1, "end_page_number": 1}),
            json!({"type": "content_block_location", "cited_text": "x", "document_index": 0, "start_block_index": 0, "end_block_index": 1}),
            json!({"type": "search_result_location", "cited_text": "x", "search_result_index": 0, "source_block_index": 0}),
            json!({"type": "web_search_result_location", "cited_text": "x", "encrypted_index": "e"}),
        ];
        for v in variants {
            let _: TextCitation = serde_json::from_value(v).expect("parse");
        }
    }
}
