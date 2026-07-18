use super::*;

#[test]
fn fuzzy_match_supports_substring_and_subsequence() {
    assert!(fuzzy_score("src/panel/input.rs", "input").is_some());
    assert!(fuzzy_score("src/panel/input.rs", "spi").is_some());
    assert!(fuzzy_score("src/panel/input.rs", "zzz").is_none());
}
