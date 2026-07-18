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
