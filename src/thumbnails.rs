//! On-disk cache of New Tab page tile images: page thumbnails and favicons.
//!
//! Thumbnails are captured from the rendered page (see
//! [`cosmic_webview::ServoState::screenshot`]); favicons are fetched from each
//! site's `/favicon.ico`. Both are stored as PNG under the XDG cache directory
//! and refreshed once they pass [`MAX_AGE`].

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use image::RgbaImage;

/// Cached tile images refresh after this age (30 days).
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Cached image paths for a single New Tab grid site.
///
/// A site can have a page thumbnail, a favicon, or both. The grid uses the
/// thumbnail as the tile background and overlays the favicon to identify the
/// site; either may be absent.
#[derive(Clone, Default)]
pub struct TileImage {
    /// Page thumbnail captured from the rendered page, if present on disk.
    pub thumbnail: Option<PathBuf>,
    /// Site favicon, if present on disk.
    pub favicon: Option<PathBuf>,
}

impl TileImage {
    /// True when neither a thumbnail nor a favicon is available.
    pub fn is_empty(&self) -> bool {
        self.thumbnail.is_none() && self.favicon.is_none()
    }
}

/// Thumbnail dimensions in pixels — a 16:10 tile.
const THUMB_W: u32 = 256;
const THUMB_H: u32 = 160;

/// Favicon dimensions in pixels.
const FAVICON_SIZE: u32 = 32;

/// Path to the thumbnail PNG for `url`.
pub fn thumbnail_path(url: &str) -> PathBuf {
    cache_subdir("thumbnails").join(hashed_name(url))
}

/// Path to the favicon PNG for `url`'s origin.
///
/// Keyed on the origin (scheme + host), so every page on a site shares one
/// favicon file.
pub fn favicon_path(url: &str) -> PathBuf {
    let key = origin_of(url).unwrap_or_else(|| url.to_string());
    cache_subdir("favicons").join(hashed_name(&key))
}

/// True if `path` is missing or its mtime is older than `max_age`.
pub fn is_stale(path: &Path, max_age: Duration) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return true;
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) => age > max_age,
        // mtime in the future — treat as fresh rather than re-fetching forever.
        Err(_) => false,
    }
}

/// Downscale `frame` to thumbnail size and write it as PNG for `url`.
pub fn save_thumbnail(url: &str, frame: &RgbaImage) -> anyhow::Result<()> {
    let path = thumbnail_path(url);
    ensure_parent(&path)?;
    let thumb = image::imageops::thumbnail(frame, THUMB_W, THUMB_H);
    thumb.save(&path)?;
    Ok(())
}

/// Fetch `<origin>/favicon.ico` and cache it as a PNG for `url`'s origin.
///
/// The request goes straight to the site itself (already visited), so it leaks
/// nothing beyond the visit that just happened. Unsupported formats (e.g. SVG)
/// are skipped silently — the caller falls back to a generic icon.
pub async fn fetch_favicon(url: String) -> anyhow::Result<()> {
    let Some(origin) = origin_of(&url) else {
        return Ok(());
    };
    let bytes = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(format!("{origin}/favicon.ico"))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let decoded = image::load_from_memory(&bytes)?;
    let favicon = decoded.thumbnail(FAVICON_SIZE, FAVICON_SIZE).to_rgba8();
    let path = favicon_path(&url);
    ensure_parent(&path)?;
    favicon.save(&path)?;
    Ok(())
}

fn cache_subdir(name: &str) -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cosmic-web")
        .join(name)
}

/// Deterministic `<hash>.png` filename for a cache key.
fn hashed_name(key: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    format!("{:016x}.png", hasher.finish())
}

/// `scheme://host` of a URL, or `None` if it cannot be parsed.
fn origin_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    Some(format!("{}://{}", parsed.scheme(), parsed.host_str()?))
}

fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_path_is_deterministic_and_per_url() {
        let a1 = thumbnail_path("https://a.example/");
        let a2 = thumbnail_path("https://a.example/");
        let b = thumbnail_path("https://b.example/");
        assert_eq!(a1, a2);
        assert_ne!(a1, b);
        assert_eq!(a1.extension().and_then(|e| e.to_str()), Some("png"));
    }

    #[test]
    fn favicon_path_is_keyed_on_origin() {
        // Two pages on the same origin share one favicon file…
        let p1 = favicon_path("https://site.example/page1");
        let p2 = favicon_path("https://site.example/page2?q=1");
        assert_eq!(p1, p2);
        // …but a different origin gets a different file.
        assert_ne!(p1, favicon_path("https://other.example/page1"));
    }

    #[test]
    fn missing_file_is_stale() {
        let path = std::env::temp_dir().join("cosmic-web-nonexistent-xyz.png");
        let _ = std::fs::remove_file(&path);
        assert!(is_stale(&path, MAX_AGE));
    }

    #[test]
    fn fresh_file_is_not_stale() {
        let path =
            std::env::temp_dir().join(format!("cosmic-web-fresh-{}.png", std::process::id()));
        std::fs::write(&path, b"x").expect("write temp file");
        assert!(!is_stale(&path, MAX_AGE));
        // A zero max-age makes any existing file count as stale.
        assert!(is_stale(&path, Duration::ZERO));
        let _ = std::fs::remove_file(&path);
    }
}
