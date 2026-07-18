use super::*;

#[test]
fn signature_verify() {
    let crypto = WechatCrypto::new(
        "callback-token".into(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "corp".into(),
    )
    .unwrap();
    let mut parts = ["callback-token", "1700000000", "nonce", "ciphertext"];
    parts.sort_unstable();
    let mut hasher = Sha1::new();
    hasher.update(parts.join("").as_bytes());
    let signature = hex_digest(&hasher.finalize());

    assert!(crypto.verify_signature(&signature, "1700000000", "nonce", "ciphertext"));
    assert!(!crypto.verify_signature(&signature, "1700000001", "nonce", "ciphertext"));
}

#[test]
fn parse_xml_extracts_fields() {
    let xml = r"<xml><ToUserName><![CDATA[bot]]></ToUserName><FromUserName><![CDATA[alice]]></FromUserName><CreateTime>1700000000</CreateTime><MsgType><![CDATA[text]]></MsgType><Content><![CDATA[hello world]]></Content><MsgId>123456</MsgId></xml>";
    let msg = parse_message_xml(xml).unwrap();
    assert_eq!(msg.from_user_name, "alice");
    assert_eq!(msg.create_time, 1_700_000_000);
    assert_eq!(msg.msg_type, "text");
    assert_eq!(msg.content, "hello world");
    assert_eq!(msg.msg_id, "123456");
}

#[test]
fn aes_cbc_round_trips_multiple_blocks() {
    let key = [0x2a; 32];
    let mut padded = vec![0x11; 31];
    padded.push(1);
    let encrypted = encrypt_cbc(&key, &padded).unwrap();
    assert_ne!(encrypted, padded);
    assert_eq!(decrypt_cbc(&key, &encrypted).unwrap(), vec![0x11; 31]);
}

#[test]
fn decrypt_rejects_ciphertext_for_another_enterprise() {
    let key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let sender = WechatCrypto::new("token".into(), key, "corp-a".into()).unwrap();
    let receiver = WechatCrypto::new("token".into(), key, "corp-b".into()).unwrap();
    let envelope = sender.encrypt("<xml/>", "1", "nonce").unwrap();
    let encrypted = envelope
        .split("<Encrypt><![CDATA[")
        .nth(1)
        .and_then(|value| value.split("]]></Encrypt>").next())
        .unwrap();

    assert!(matches!(
        receiver.decrypt(encrypted),
        Err(CryptoError::CorpIdMismatch)
    ));
}

#[test]
fn parse_xml_unescapes_text_entities() {
    let xml = r"<xml><Content>one &amp; two &#x1F980;</Content></xml>";
    assert_eq!(parse_message_xml(xml).unwrap().content, "one & two 🦀");
}
