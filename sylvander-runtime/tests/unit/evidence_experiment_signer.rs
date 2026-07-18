use super::*;

#[test]
fn hmac_matches_the_standard_vector() {
    assert_eq!(
        hmac_sha256(&[0x0b; 20], b"Hi There"),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
}
