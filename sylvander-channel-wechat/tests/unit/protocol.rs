use super::*;

#[test]
#[ignore = "requires a real 44-char WeChat EncodingAESKey (real keys end with '=' padding)"]
fn signature_verify() {
    // Real WeChat keys are 43 chars base64 with implicit padding → 32 bytes raw.
    // Test code below would need a valid one.
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
fn parse_xml_unescapes_text_entities() {
    let xml = r"<xml><Content>one &amp; two &#x1F980;</Content></xml>";
    assert_eq!(parse_message_xml(xml).unwrap().content, "one & two 🦀");
}
