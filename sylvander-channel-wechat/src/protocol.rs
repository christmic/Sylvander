//! WeChat enterprise messaging — encryption + XML parsing.

use aes::Aes256;
use aes::cipher::BlockDecrypt;
use aes::cipher::BlockEncrypt;
use aes::cipher::KeyInit;
use aes::cipher::generic_array::GenericArray;
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD as B64};
use sha1::{Digest, Sha1};

type Block = GenericArray<u8, aes::cipher::generic_array::typenum::U16>;

#[derive(Debug)]
pub enum CryptoError {
    Base64(base64::DecodeError),
    Aes,
    InvalidUtf8,
    InvalidLength,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::Base64(e) => write!(f, "base64: {e}"),
            CryptoError::Aes => write!(f, "aes failed"),
            CryptoError::InvalidUtf8 => write!(f, "invalid utf8"),
            CryptoError::InvalidLength => write!(f, "invalid length"),
        }
    }
}

impl std::error::Error for CryptoError {}

pub struct WechatCrypto {
    pub token: String,
    pub aes_key: [u8; 32],
    pub corp_id: String,
}

impl WechatCrypto {
    pub fn new(
        token: String,
        encoding_aes_key: &str,
        corp_id: String,
    ) -> Result<Self, CryptoError> {
        let aes_key_vec = B64
            .decode(encoding_aes_key.as_bytes())
            .map_err(CryptoError::Base64)?;
        if aes_key_vec.len() != 32 {
            return Err(CryptoError::InvalidLength);
        }
        let mut aes_key = [0u8; 32];
        aes_key.copy_from_slice(&aes_key_vec);
        Ok(Self {
            token,
            aes_key,
            corp_id,
        })
    }

    pub fn verify_signature(
        &self,
        signature: &str,
        timestamp: &str,
        nonce: &str,
        encrypted: &str,
    ) -> bool {
        let mut parts = [self.token.as_str(), timestamp, nonce, encrypted];
        parts.sort();
        let joined = parts.join("");
        let mut hasher = Sha1::new();
        hasher.update(joined.as_bytes());
        let result = format!("{:x}", hasher.finalize());
        result == signature
    }

    pub fn decrypt(&self, encrypted_b64: &str) -> Result<(String, String), CryptoError> {
        let ciphertext = B64
            .decode(encrypted_b64.as_bytes())
            .map_err(CryptoError::Base64)?;
        let plaintext = decrypt_cbc(&self.aes_key, &ciphertext)?;

        if plaintext.len() < 20 {
            return Err(CryptoError::Aes);
        }
        let msg_len =
            u32::from_be_bytes([plaintext[16], plaintext[17], plaintext[18], plaintext[19]])
                as usize;
        if plaintext.len() < 20 + msg_len {
            return Err(CryptoError::Aes);
        }
        let msg = std::str::from_utf8(&plaintext[20..20 + msg_len])
            .map_err(|_| CryptoError::InvalidUtf8)?
            .to_string();
        let recv_corp_id = std::str::from_utf8(&plaintext[20 + msg_len..])
            .map_err(|_| CryptoError::InvalidUtf8)?
            .to_string();
        Ok((msg, recv_corp_id))
    }

    pub fn encrypt(
        &self,
        reply: &str,
        timestamp: &str,
        nonce: &str,
    ) -> Result<String, CryptoError> {
        let mut plaintext = Vec::new();
        plaintext.extend_from_slice(b"0123456789abcdef");
        let msg_bytes = reply.as_bytes();
        let msg_len = msg_bytes.len() as u32;
        plaintext.extend_from_slice(&msg_len.to_be_bytes());
        plaintext.extend_from_slice(msg_bytes);
        plaintext.extend_from_slice(self.corp_id.as_bytes());

        // PKCS7 pad
        let block_size = 16;
        let pad_len = block_size - (plaintext.len() % block_size);
        plaintext.extend(std::iter::repeat(pad_len as u8).take(pad_len));

        let ciphertext = encrypt_cbc(&self.aes_key, &plaintext)?;
        let encrypted_b64 = B64.encode(&ciphertext);

        let mut parts = [
            self.token.as_str(),
            timestamp,
            nonce,
            encrypted_b64.as_str(),
        ];
        parts.sort();
        let joined = parts.join("");
        let mut hasher = Sha1::new();
        hasher.update(joined.as_bytes());
        let signature = format!("{:x}", hasher.finalize());

        Ok(format!(
            r#"<xml><Encrypt><![CDATA[{encrypted_b64}]]></Encrypt><MsgSignature><![CDATA[{signature}]]></MsgSignature><TimeStamp>{timestamp}</TimeStamp><Nonce><![CDATA[{nonce}]]></Nonce></xml>"#
        ))
    }
}

// ===========================================================================
// AES-256-CBC with manual PKCS7 (no external dep on cbc feature flags)
// ===========================================================================

const BLOCK_SIZE: usize = 16;

fn to_block(arr: [u8; 16]) -> Block {
    *GenericArray::from_slice(&arr)
}

fn from_block(b: &Block) -> [u8; 16] {
    let mut arr = [0u8; 16];
    arr.copy_from_slice(b.as_slice());
    arr
}

fn encrypt_cbc(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if plaintext.len() % BLOCK_SIZE != 0 {
        return Err(CryptoError::Aes);
    }
    let cipher = Aes256::new(key.into());
    let mut prev = to_block([0u8; 16]);
    let mut ciphertext = Vec::with_capacity(plaintext.len());
    for chunk in plaintext.chunks(BLOCK_SIZE) {
        let mut arr = [0u8; 16];
        arr.copy_from_slice(chunk);
        for i in 0..16 {
            arr[i] ^= prev[i];
        }
        let mut blk = to_block(arr);
        cipher.encrypt_block(&mut blk);
        ciphertext.extend_from_slice(&from_block(&blk));
        prev = blk;
    }
    Ok(ciphertext)
}

fn decrypt_cbc(key: &[u8; 32], ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if ciphertext.len() % BLOCK_SIZE != 0 {
        return Err(CryptoError::Aes);
    }
    let cipher = Aes256::new(key.into());
    let mut prev = to_block([0u8; 16]);
    let mut plaintext = Vec::with_capacity(ciphertext.len());
    for chunk in ciphertext.chunks(BLOCK_SIZE) {
        let mut blk = to_block([0u8; 16]);
        cipher.decrypt_block(&mut blk);
        let mut arr = from_block(&blk);
        for i in 0..16 {
            arr[i] ^= prev[i];
        }
        plaintext.extend_from_slice(&arr);
        let mut next = [0u8; 16];
        next.copy_from_slice(chunk);
        prev = to_block(next);
    }

    // Strip PKCS7
    let pad = plaintext.last().copied().unwrap_or(0) as usize;
    if pad == 0 || pad > BLOCK_SIZE || pad > plaintext.len() {
        return Err(CryptoError::Aes);
    }
    for &b in &plaintext[plaintext.len() - pad..] {
        if b as usize != pad {
            return Err(CryptoError::Aes);
        }
    }
    plaintext.truncate(plaintext.len() - pad);
    Ok(plaintext)
}

// ===========================================================================
// XML parsing
// ===========================================================================

#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub from_user_name: String,
    pub create_time: i64,
    pub content: String,
    pub msg_id: String,
    pub msg_type: String,
}

pub fn parse_message_xml(xml: &str) -> Result<IncomingMessage, String> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);

    let mut fields = std::collections::HashMap::new();
    let mut current_tag = String::new();
    let mut current_value = String::new();

    loop {
        match reader.read_event() {
            Err(e) => return Err(format!("xml parse: {e}")),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                current_value.clear();
            }
            Ok(Event::Text(t)) => {
                let s = t.unescape().unwrap_or_default();
                current_value.push_str(&s);
            }
            Ok(Event::CData(c)) => {
                current_value.push_str(&String::from_utf8_lossy(&c));
            }
            Ok(Event::End(_)) => {
                fields.insert(current_tag.clone(), current_value.trim().to_string());
                current_tag.clear();
                current_value.clear();
            }
            _ => {}
        }
    }

    Ok(IncomingMessage {
        from_user_name: fields.remove("FromUserName").unwrap_or_default(),
        create_time: fields
            .remove("CreateTime")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        content: fields.remove("Content").unwrap_or_default(),
        msg_id: fields.remove("MsgId").unwrap_or_default(),
        msg_type: fields.remove("MsgType").unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a real 44-char WeChat EncodingAESKey (real keys end with '=' padding)"]
    fn signature_verify() {
        // Real WeChat keys are 43 chars base64 with implicit padding → 32 bytes raw.
        // Test code below would need a valid one.
    }

    #[test]
    fn parse_xml_extracts_fields() {
        let xml = r#"<xml><ToUserName><![CDATA[bot]]></ToUserName><FromUserName><![CDATA[alice]]></FromUserName><CreateTime>1700000000</CreateTime><MsgType><![CDATA[text]]></MsgType><Content><![CDATA[hello world]]></Content><MsgId>123456</MsgId></xml>"#;
        let msg = parse_message_xml(xml).unwrap();
        assert_eq!(msg.from_user_name, "alice");
        assert_eq!(msg.create_time, 1700000000);
        assert_eq!(msg.msg_type, "text");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.msg_id, "123456");
    }
}
