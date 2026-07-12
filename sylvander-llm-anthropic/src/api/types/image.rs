//! Image content blocks (base64 inline).

use serde::{Deserialize, Serialize};

use super::cache::CacheControl;

/// User-turn image content block. The image is provided as a base64-encoded
/// payload with its media type.
///
/// Wire format:
/// ```json
/// {
///   "type": "image",
///   "source": {
///     "type": "base64",
///     "media_type": "image/png",
///     "data": "<base64 bytes>"
///   }
/// }
/// ```
///
/// URL-based image sources are **not** supported in v2 — the protocol SDK
/// only implements base64 inline. If a URL source is needed, fetch the
/// bytes locally first and re-encode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageBlock {
    /// Always `"image"`.
    #[serde(rename = "type")]
    pub kind: ImageBlockKind,
    /// The image data.
    pub source: ImageSource,
    /// Optional cache control breakpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

impl ImageBlock {
    /// Create a base64-encoded PNG image block.
    #[must_use]
    pub fn png(data: impl Into<String>) -> Self {
        Self {
            kind: ImageBlockKind::Image,
            source: ImageSource::Base64(Base64ImageSource {
                media_type: ImageMediaType::Png,
                data: data.into(),
            }),
            cache_control: None,
        }
    }

    /// Create a base64-encoded JPEG image block.
    #[must_use]
    pub fn jpeg(data: impl Into<String>) -> Self {
        Self {
            kind: ImageBlockKind::Image,
            source: ImageSource::Base64(Base64ImageSource {
                media_type: ImageMediaType::Jpeg,
                data: data.into(),
            }),
            cache_control: None,
        }
    }

    /// Attach a cache control breakpoint.
    #[must_use]
    pub fn with_cache_control(mut self, cc: CacheControl) -> Self {
        self.cache_control = Some(cc);
        self
    }
}

/// Image block discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageBlockKind {
    /// Image content block.
    #[serde(rename = "image")]
    Image,
}

/// Image source variant. Currently only base64 is supported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    /// Base64-encoded image.
    Base64(Base64ImageSource),
}

/// Base64 image source payload. The `type: "base64"` discriminator is
/// carried by the parent [`ImageSource`] enum tag, not on this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Base64ImageSource {
    /// MIME type of the image.
    pub media_type: ImageMediaType,
    /// Base64-encoded image bytes (no data URI prefix).
    pub data: String,
}

impl Base64ImageSource {
    /// Create a new base64 image source.
    #[must_use]
    pub fn new(media_type: ImageMediaType, data: impl Into<String>) -> Self {
        Self {
            media_type,
            data: data.into(),
        }
    }
}

/// Supported image MIME types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageMediaType {
    /// JPEG image.
    #[serde(rename = "image/jpeg")]
    Jpeg,
    /// PNG image.
    #[serde(rename = "image/png")]
    Png,
    /// GIF image.
    #[serde(rename = "image/gif")]
    Gif,
    /// WebP image.
    #[serde(rename = "image/webp")]
    Webp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_block_wire_format() {
        let block = ImageBlock::png("iVBORw0KGgoAAAANSUhEUg==");
        let json = serde_json::to_string(&block).unwrap();
        let back: ImageBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn jpeg_block_wire_format() {
        let block = ImageBlock::jpeg("/9j/4AAQS");
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""media_type":"image/jpeg""#));
    }

    #[test]
    fn cache_control_omitted_when_none() {
        let block = ImageBlock::png("xxx");
        let json = serde_json::to_string(&block).unwrap();
        assert!(!json.contains("cache_control"));
    }

    #[test]
    fn cache_control_included_when_set() {
        let block = ImageBlock::png("xxx").with_cache_control(CacheControl::ephemeral());
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""cache_control":"#));
    }
}
