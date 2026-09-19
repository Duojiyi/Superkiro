//! Inbound image compression, validation, and resize module.
//!
//! Spec §4.3 & T05:
//! - Pre-decode validation: invalid base64, fake MIME / magic byte mismatch,
//!   oversized dimensions (>8192px), decode allocation limits (64 MiB).
//! - Long side <= 1568 px (Anthropic recommended vision patch boundary)
//! - Raw size <= 400 KB (avoids upstream payload limit rejects)
//! - JPEG quality 85
//! - GIF format preserved (may be animated)
//! - Small images pass through directly (zero overhead)
//! - Never panics: invalid images are safely rejected/flagged before decoding.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::imageops::FilterType;
use std::io::Cursor;

pub const DEFAULT_MAX_LONG_SIDE: u32 = 1568;
pub const DEFAULT_MAX_BYTES: usize = 400_000;
pub const DEFAULT_JPEG_QUALITY: u8 = 85;
pub const MAX_DECODE_DIMENSION: u32 = 8_192;
pub const MAX_DECODE_ALLOC: u64 = 64 * 1024 * 1024;
pub const MAX_IMAGE_PAYLOAD_BYTES: usize = 20 * 1024 * 1024;

/// Output of image processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedImage {
    pub format: String,
    pub base64_data: String,
    pub was_resized: bool,
    pub is_valid: bool,
}

/// Pre-decode image validation errors (T05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageValidationError {
    InvalidBase64(String),
    PayloadTooLarge(usize),
    UnsupportedFormat(String),
    MimeMismatch { declared: String, detected: String },
    DimensionsTooLarge { width: u32, height: u32 },
    DecodeLimitExceeded(String),
}

/// Detect genuine image format from magic header bytes.
pub fn detect_image_format_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 8 && bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("png")
    } else if bytes.len() >= 3 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpeg")
    } else if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        Some("gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// Normalize declared format string (e.g. "jpg" -> "jpeg").
fn normalize_format(fmt: &str) -> &str {
    let lower = fmt.trim();
    if lower.eq_ignore_ascii_case("jpg") || lower.eq_ignore_ascii_case("jpeg") {
        "jpeg"
    } else if lower.eq_ignore_ascii_case("png") {
        "png"
    } else if lower.eq_ignore_ascii_case("gif") {
        "gif"
    } else if lower.eq_ignore_ascii_case("webp") {
        "webp"
    } else {
        lower
    }
}

/// Validate image headers, magic bytes, base64 encoding, and dimensions before full decode (T05).
pub fn validate_image_input(
    declared_format: &str,
    base64_data: &str,
) -> Result<(Vec<u8>, &'static str, u32, u32), ImageValidationError> {
    let trimmed = base64_data.trim();
    if trimmed.is_empty() {
        return Err(ImageValidationError::InvalidBase64(
            "empty image data".into(),
        ));
    }

    let raw_bytes = BASE64
        .decode(trimmed)
        .map_err(|e| ImageValidationError::InvalidBase64(e.to_string()))?;

    if raw_bytes.len() > MAX_IMAGE_PAYLOAD_BYTES {
        return Err(ImageValidationError::PayloadTooLarge(raw_bytes.len()));
    }

    let detected = detect_image_format_from_bytes(&raw_bytes)
        .ok_or_else(|| ImageValidationError::UnsupportedFormat("unknown magic bytes".into()))?;

    let norm_declared = normalize_format(declared_format);
    if !norm_declared.is_empty() && norm_declared != detected {
        return Err(ImageValidationError::MimeMismatch {
            declared: declared_format.to_string(),
            detected: detected.to_string(),
        });
    }

    // Inspect dimensions with strict limits before pixel buffer allocation
    let mut reader = image::ImageReader::new(Cursor::new(&raw_bytes))
        .with_guessed_format()
        .map_err(|e| ImageValidationError::DecodeLimitExceeded(e.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);

    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| ImageValidationError::DecodeLimitExceeded(e.to_string()))?;

    if width > MAX_DECODE_DIMENSION || height > MAX_DECODE_DIMENSION {
        return Err(ImageValidationError::DimensionsTooLarge { width, height });
    }

    Ok((raw_bytes, detected, width, height))
}

/// Validate image headers and decoder resource limits before any full decode.
pub fn validate_image_bytes(raw_bytes: &[u8]) -> Result<(u32, u32), String> {
    let mut reader = image::ImageReader::new(Cursor::new(raw_bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    reader.into_dimensions().map_err(|error| error.to_string())
}

/// Process an inbound image with pre-decode validation and downsampling rules.
pub fn maybe_shrink_image(format: &str, base64_data: &str) -> ProcessedImage {
    let (raw_bytes, detected_format, width, height) =
        match validate_image_input(format, base64_data) {
            Ok(v) => v,
            Err(_) => {
                return ProcessedImage {
                    format: format.to_string(),
                    base64_data: base64_data.to_string(),
                    was_resized: false,
                    is_valid: false,
                };
            }
        };

    // Rule 1: GIF is preserved as-is (may be animated)
    if detected_format == "gif" {
        return ProcessedImage {
            format: "gif".to_string(),
            base64_data: base64_data.to_string(),
            was_resized: false,
            is_valid: true,
        };
    }

    // Rule 2: Small images (<= 400KB and <= 1568px) pass through directly
    if raw_bytes.len() <= DEFAULT_MAX_BYTES
        && width <= DEFAULT_MAX_LONG_SIDE
        && height <= DEFAULT_MAX_LONG_SIDE
    {
        return ProcessedImage {
            format: detected_format.to_string(),
            base64_data: base64_data.to_string(),
            was_resized: false,
            is_valid: true,
        };
    }

    // Rule 3: Large image (> 400KB or dimensions > 1568px) -> Decode, resize & re-encode
    let dynamic_img = match image::ImageReader::new(Cursor::new(&raw_bytes))
        .with_guessed_format()
        .ok()
        .and_then(|mut reader| {
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(MAX_DECODE_DIMENSION);
            limits.max_image_height = Some(MAX_DECODE_DIMENSION);
            limits.max_alloc = Some(MAX_DECODE_ALLOC);
            reader.limits(limits);
            reader.decode().ok()
        }) {
        Some(img) => img,
        None => {
            return ProcessedImage {
                format: detected_format.to_string(),
                base64_data: base64_data.to_string(),
                was_resized: false,
                is_valid: true,
            };
        }
    };

    let (orig_w, orig_h) = (dynamic_img.width(), dynamic_img.height());
    let max_dim = orig_w.max(orig_h);

    let resized_img = if max_dim > DEFAULT_MAX_LONG_SIDE {
        let scale = DEFAULT_MAX_LONG_SIDE as f64 / max_dim as f64;
        let new_w = ((orig_w as f64 * scale).round() as u32).max(1);
        let new_h = ((orig_h as f64 * scale).round() as u32).max(1);
        dynamic_img.resize(new_w, new_h, FilterType::Triangle)
    } else {
        dynamic_img
    };

    // Re-encode as JPEG with quality 85
    let mut encoded_buf = Vec::new();
    let mut cursor = Cursor::new(&mut encoded_buf);

    let encode_result = {
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, DEFAULT_JPEG_QUALITY);
        encoder.encode_image(&resized_img)
    };

    if encode_result.is_ok() && !encoded_buf.is_empty() {
        let new_base64 = BASE64.encode(&encoded_buf);
        ProcessedImage {
            format: "jpeg".to_string(),
            base64_data: new_base64,
            was_resized: true,
            is_valid: true,
        }
    } else {
        ProcessedImage {
            format: detected_format.to_string(),
            base64_data: base64_data.to_string(),
            was_resized: false,
            is_valid: true,
        }
    }
}
