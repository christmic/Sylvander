use super::*;

#[test]
fn detects_entry() {
    let c = "127.0.0.1 localhost\n127.0.0.1\ttoken9.test\t# token9\n";
    assert!(has_entry(c, "token9.test"));
    assert!(!has_entry(c, "other.test"));
}

#[test]
fn ignores_commented_lines() {
    let c = "# 127.0.0.1 token9.test\n";
    assert!(!has_entry(c, "token9.test"));
}

#[test]
fn requires_loopback_ip() {
    let c = "10.0.0.1 token9.test\n";
    assert!(!has_entry(c, "token9.test"));
}
