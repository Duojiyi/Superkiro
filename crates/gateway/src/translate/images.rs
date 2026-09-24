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
//! - Pixels are decoded off the async workers, a few at a time process-wide, and each
//!   distinct image once: a conversation resends every earlier image on every turn.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::imageops::FilterType;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::Semaphore;
use tokio::time::Instant;

pub const DEFAULT_MAX_LONG_SIDE: u32 = 1568;
pub const DEFAULT_MAX_BYTES: usize = 400_000;
pub const DEFAULT_JPEG_QUALITY: u8 = 85;
pub const MAX_DECODE_DIMENSION: u32 = 8_192;
pub const MAX_DECODE_ALLOC: u64 = 64 * 1024 * 1024;
pub const MAX_IMAGE_PAYLOAD_BYTES: usize = 20 * 1024 * 1024;

/// Shrink results kept for reuse: at most this many, holding at most this many bytes.
const SHRINK_CACHE_ENTRIES: usize = 4_096;
const SHRINK_CACHE_BYTES: usize = 64 * 1024 * 1024;

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

/// What one inbound image becomes in the provider request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedImage {
    /// Forwarded exactly as the client sent it.
    Original { format: &'static str },
    /// Forwarded as this JPEG re-encoding (base64), small enough for any provider.
    Resized { base64_data: Arc<str> },
    /// Replaced by a note to the model.
    Omitted(Omission),
}

/// Why an image reaches the model as a note instead of as an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Omission {
    /// Not an image the gateway can read. Forwarding it would make the provider reject
    /// this turn and every later one, since the conversation keeps resending it.
    Unreadable,
    /// Older than the most recent images one request may carry.
    OverCount,
    /// It had to be shrunk, and there was no capacity to do that in time.
    Busy,
}

/// An image judged from its header alone, before any pixel is decoded.
#[derive(Debug)]
pub enum Inspection {
    /// Nothing more to do: forward it as is, or omit it.
    Ready(PreparedImage),
    /// Too large to forward. Holds the validated raw bytes to shrink.
    NeedsShrink(Vec<u8>),
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

/// The format base64 image data carries, from its first bytes alone.
pub fn sniff_format(base64_data: &str) -> Option<&'static str> {
    let head: String = base64_data
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .take(16)
        .collect();
    detect_image_format_from_bytes(&BASE64.decode(head).ok()?)
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

fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    limits
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
    reader.limits(decode_limits());

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
    reader.limits(decode_limits());
    reader.into_dimensions().map_err(|error| error.to_string())
}

/// Judge an image from its header. Small images and GIFs within the byte budget go as
/// they are (a GIF may be animated); anything larger must be shrunk first.
///
/// The format is the one the bytes carry. A declared format that disagrees (a PNG saved
/// as `.jpg`) would only make the provider reject the image under the wrong media type.
pub fn inspect_image(base64_data: &str) -> Inspection {
    let Ok((raw_bytes, detected, width, height)) = validate_image_input("", base64_data) else {
        return Inspection::Ready(PreparedImage::Omitted(Omission::Unreadable));
    };
    let fits_bytes = raw_bytes.len() <= DEFAULT_MAX_BYTES;
    let fits_size = width <= DEFAULT_MAX_LONG_SIDE && height <= DEFAULT_MAX_LONG_SIDE;
    if fits_bytes && (fits_size || detected == "gif") {
        Inspection::Ready(PreparedImage::Original { format: detected })
    } else {
        Inspection::NeedsShrink(raw_bytes)
    }
}

/// Decode, downsize and re-encode a validated image as JPEG, returned as base64. `None`
/// when the pixels cannot be decoded within the decoder limits. CPU-bound: keep it off
/// the async workers.
fn shrink_pixels(raw_bytes: &[u8]) -> Option<String> {
    let mut reader = image::ImageReader::new(Cursor::new(raw_bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(decode_limits());
    let decoded = reader.decode().ok()?;

    let (width, height) = (decoded.width(), decoded.height());
    let long_side = width.max(height);
    let resized = if long_side > DEFAULT_MAX_LONG_SIDE {
        let scale = DEFAULT_MAX_LONG_SIDE as f64 / long_side as f64;
        let new_w = ((width as f64 * scale).round() as u32).max(1);
        let new_h = ((height as f64 * scale).round() as u32).max(1);
        decoded.resize(new_w, new_h, FilterType::Triangle)
    } else {
        decoded
    };

    // JPEG has no alpha. Dropping it would turn transparent areas black and hide dark
    // strokes on a transparent background, so flatten onto white first.
    let opaque = if resized.color().has_alpha() {
        let top = resized.into_rgba8();
        let mut page =
            image::RgbaImage::from_pixel(top.width(), top.height(), image::Rgba([255; 4]));
        image::imageops::overlay(&mut page, &top, 0, 0);
        image::DynamicImage::ImageRgba8(page).into_rgb8()
    } else {
        resized.into_rgb8()
    };

    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(
        &mut Cursor::new(&mut encoded),
        DEFAULT_JPEG_QUALITY,
    )
    .encode_image(&opaque)
    .ok()?;
    (!encoded.is_empty()).then(|| BASE64.encode(&encoded))
}

/// Process an inbound image with pre-decode validation and downsampling rules.
///
/// Decodes on the calling thread. Request handling uses [`shrink_image`] instead, which
/// bounds and caches the decode work.
pub fn maybe_shrink_image(format: &str, base64_data: &str) -> ProcessedImage {
    let unchanged = |format: &str, is_valid: bool| ProcessedImage {
        format: format.to_string(),
        base64_data: base64_data.to_string(),
        was_resized: false,
        is_valid,
    };
    match inspect_image(base64_data) {
        Inspection::Ready(PreparedImage::Original { format }) => unchanged(format, true),
        Inspection::Ready(_) => unchanged(format, false),
        Inspection::NeedsShrink(raw_bytes) => match shrink_pixels(&raw_bytes) {
            Some(base64_data) => ProcessedImage {
                format: "jpeg".to_string(),
                base64_data,
                was_resized: true,
                is_valid: true,
            },
            None => unchanged(format, false),
        },
    }
}

/// Decodes run at most this many at a time process-wide. Each may hold up to
/// `MAX_DECODE_ALLOC` of pixels, and the blocking pool alone would allow hundreds.
fn decode_permit_count() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    (cores / 2).max(2)
}

fn decode_permits() -> &'static Arc<Semaphore> {
    static PERMITS: LazyLock<Arc<Semaphore>> =
        LazyLock::new(|| Arc::new(Semaphore::new(decode_permit_count())));
    &PERMITS
}

/// Shrink results by SHA-256 of the raw image, least recently used evicted first. `None`
/// records an image whose pixels cannot be decoded, so it is not decoded again either.
#[derive(Default)]
struct ShrinkCache {
    entries: HashMap<[u8; 32], CacheEntry>,
    bytes: usize,
    clock: u64,
}

struct CacheEntry {
    value: Option<Arc<str>>,
    last_used: u64,
}

impl CacheEntry {
    fn size(&self) -> usize {
        self.value.as_ref().map_or(0, |value| value.len()) + 64
    }
}

impl ShrinkCache {
    fn get(&mut self, key: &[u8; 32]) -> Option<Option<Arc<str>>> {
        self.clock += 1;
        let entry = self.entries.get_mut(key)?;
        entry.last_used = self.clock;
        Some(entry.value.clone())
    }

    fn insert(&mut self, key: [u8; 32], value: Option<Arc<str>>) {
        self.clock += 1;
        let entry = CacheEntry {
            value,
            last_used: self.clock,
        };
        if entry.size() > SHRINK_CACHE_BYTES {
            return;
        }
        self.bytes += entry.size();
        if let Some(old) = self.entries.insert(key, entry) {
            self.bytes -= old.size();
        }
        while self.bytes > SHRINK_CACHE_BYTES || self.entries.len() > SHRINK_CACHE_ENTRIES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes -= evicted.size();
            }
        }
    }
}

fn shrink_cache() -> &'static Mutex<ShrinkCache> {
    static CACHE: LazyLock<Mutex<ShrinkCache>> = LazyLock::new(Mutex::default);
    &CACHE
}

fn cached_shrink(key: &[u8; 32]) -> Option<Option<Arc<str>>> {
    shrink_cache().lock().ok()?.get(key)
}

fn remember_shrink(key: [u8; 32], value: Option<Arc<str>>) {
    if let Ok(mut cache) = shrink_cache().lock() {
        cache.insert(key, value);
    }
}

fn shrunk(value: Option<Arc<str>>) -> PreparedImage {
    match value {
        Some(base64_data) => PreparedImage::Resized { base64_data },
        None => PreparedImage::Omitted(Omission::Unreadable),
    }
}

/// Shrink one image from [`Inspection::NeedsShrink`] without blocking the async workers.
///
/// A cached result is returned at once. Otherwise the decode waits for one of the
/// process-wide permits and runs on the blocking pool. If `deadline` passes first, the
/// image is `Busy` for this request; a decode already running still finishes, keeps its
/// permit until then, and caches its result for the next turn.
pub async fn shrink_image(raw_bytes: Vec<u8>, deadline: Instant) -> PreparedImage {
    let key: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, &raw_bytes)
        .as_ref()
        .try_into()
        .expect("SHA-256 digests are 32 bytes");
    if let Some(value) = cached_shrink(&key) {
        return shrunk(value);
    }
    // A request past its budget starts no new decode, even with permits free.
    if Instant::now() >= deadline {
        return PreparedImage::Omitted(Omission::Busy);
    }
    let permit =
        match tokio::time::timeout_at(deadline, decode_permits().clone().acquire_owned()).await {
            Ok(Ok(permit)) => permit,
            _ => return PreparedImage::Omitted(Omission::Busy),
        };
    // Another request may have decoded the same image while this one waited.
    if let Some(value) = cached_shrink(&key) {
        return shrunk(value);
    }
    let decode = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        // A decoder panic must not leave the image to be retried on every turn.
        let value = std::panic::catch_unwind(|| shrink_pixels(&raw_bytes))
            .ok()
            .flatten()
            .map(Arc::<str>::from);
        remember_shrink(key, value.clone());
        value
    });
    match tokio::time::timeout_at(deadline, decode).await {
        Ok(Ok(value)) => shrunk(value),
        Ok(Err(_)) => PreparedImage::Omitted(Omission::Unreadable),
        Err(_) => PreparedImage::Omitted(Omission::Busy),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30]));
        let mut out = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn inspection_never_decodes_and_sorts_by_header() {
        let small = BASE64.encode(png(64, 64));
        assert!(matches!(
            inspect_image(&small),
            Inspection::Ready(PreparedImage::Original { format: "png" })
        ));
        assert!(matches!(
            inspect_image(&BASE64.encode(png(2000, 100))),
            Inspection::NeedsShrink(_)
        ));
        for data in [
            "not base64!".to_string(),
            BASE64.encode(b"GIF89a_mock_gif_data"),
            BASE64.encode(b"plain text"),
        ] {
            assert!(matches!(
                inspect_image(&data),
                Inspection::Ready(PreparedImage::Omitted(Omission::Unreadable))
            ));
        }
    }

    #[test]
    fn sniffing_reads_the_format_from_the_first_bytes() {
        let encoded = BASE64.encode(png(4, 4));
        assert_eq!(sniff_format(&encoded), Some("png"));
        let wrapped = format!(
            " {}
{}",
            &encoded[..10],
            &encoded[10..]
        );
        assert_eq!(sniff_format(&wrapped), Some("png"));
        assert_eq!(
            sniff_format(&BASE64.encode(b"GIF89a and more bytes")),
            Some("gif")
        );
        assert_eq!(sniff_format("not base64 at all!"), None);
    }

    #[test]
    fn transparent_pixels_become_white_not_black() {
        let clear = image::RgbaImage::from_pixel(2000, 10, image::Rgba([0, 0, 0, 0]));
        let mut raw = Vec::new();
        clear
            .write_to(&mut Cursor::new(&mut raw), image::ImageFormat::Png)
            .unwrap();
        let jpeg = BASE64.decode(shrink_pixels(&raw).unwrap()).unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap().into_rgb8();
        assert!(decoded
            .pixels()
            .all(|pixel| pixel.0.iter().all(|&c| c > 240)));
    }

    #[test]
    fn cache_evicts_least_recently_used_within_its_budget() {
        let mut cache = ShrinkCache::default();
        let big: Arc<str> = "x".repeat(SHRINK_CACHE_BYTES / 3).into();
        cache.insert([1; 32], Some(big.clone()));
        cache.insert([2; 32], Some(big.clone()));
        assert!(cache.get(&[1; 32]).is_some());
        cache.insert([3; 32], Some(big));
        assert!(
            cache.get(&[2; 32]).is_none(),
            "least recently used goes first"
        );
        assert!(cache.get(&[1; 32]).is_some() && cache.get(&[3; 32]).is_some());
        assert!(cache.bytes <= SHRINK_CACHE_BYTES);
        for n in 0..(SHRINK_CACHE_ENTRIES + 10) {
            cache.insert((n as u64).to_le_bytes().repeat(4).try_into().unwrap(), None);
        }
        assert!(cache.entries.len() <= SHRINK_CACHE_ENTRIES);
    }

    #[tokio::test]
    async fn shrink_decodes_each_image_once() {
        let raw = png(3000, 40);
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        let first = shrink_image(raw.clone(), deadline).await;
        let PreparedImage::Resized { base64_data } = &first else {
            panic!("a large image is resized, got {first:?}");
        };
        let key: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, &raw)
            .as_ref()
            .try_into()
            .unwrap();
        assert_eq!(cached_shrink(&key), Some(Some(base64_data.clone())));
        assert_eq!(shrink_image(raw, deadline).await, first);
    }

    #[tokio::test]
    async fn a_decode_that_cannot_start_in_time_is_busy() {
        // Holding every permit stands in for a gateway already decoding at capacity.
        let held = decode_permits()
            .clone()
            .acquire_many_owned(decode_permit_count() as u32)
            .await
            .unwrap();
        let deadline = Instant::now() + std::time::Duration::from_millis(50);
        assert_eq!(
            shrink_image(png(2500, 30), deadline).await,
            PreparedImage::Omitted(Omission::Busy)
        );
        drop(held);
    }

    #[tokio::test]
    async fn undecodable_pixels_are_omitted_and_remembered() {
        // A valid header over truncated pixel data passes inspection but cannot decode.
        let mut raw = png(2400, 2400);
        raw.truncate(200);
        let Inspection::NeedsShrink(raw) = inspect_image(&BASE64.encode(&raw)) else {
            panic!("the header alone says it needs shrinking");
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        assert_eq!(
            shrink_image(raw.clone(), deadline).await,
            PreparedImage::Omitted(Omission::Unreadable)
        );
        let key: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, &raw)
            .as_ref()
            .try_into()
            .unwrap();
        assert_eq!(cached_shrink(&key), Some(None));
    }
}
