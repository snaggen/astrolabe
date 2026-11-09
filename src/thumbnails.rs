//! On-disk cache of New Tab page tile images: page thumbnails and favicons.
//!
//! Thumbnails are captured from the rendered page (see
//! [`astrolabe_webview::ServoState::screenshot`]); favicons are fetched from each
//! site's `/favicon.ico`. Both are stored as PNG under the XDG cache directory
//! and refreshed once they pass [`MAX_AGE`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use image::RgbaImage;
use libcosmic::widget::image::Handle;

/// Cached tile images refresh after this age (30 days).
pub const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Cached images for a single New Tab grid site.
///
/// A site can have a page thumbnail, a favicon, or both. The grid uses the
/// thumbnail as the tile background and overlays the favicon to identify the
/// site; either may be absent.
#[derive(Clone, Default)]
pub struct TileImage {
    /// Page thumbnail captured from the rendered page, if present on disk.
    pub thumbnail: Option<Handle>,
    /// Site favicon, if present on disk.
    pub favicon: Option<Handle>,
}

/// Image handles for the files on disk, renewed when a file changes.
///
/// The renderer caches an image by its handle's id, and a handle made from a
/// path keeps the same id however often the file is rewritten, so a recaptured
/// thumbnail would not show until restart. Handles here are made from the
/// file's bytes and kept while its modification time stays the same.
#[derive(Default)]
pub struct TileImageCache {
    entries: HashMap<PathBuf, (SystemTime, Handle)>,
}

impl TileImageCache {
    /// Handle for the image at `path`, or `None` if it cannot be read.
    pub fn load(&mut self, path: &Path) -> Option<Handle> {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
        if let Some((cached, handle)) = self.entries.get(path)
            && *cached == modified
        {
            return Some(handle.clone());
        }
        let handle = Handle::from_bytes(std::fs::read(path).ok()?);
        self.entries
            .insert(path.to_path_buf(), (modified, handle.clone()));
        Some(handle)
    }

    /// Forget every image not in `paths`, so removed sites free their memory.
    pub fn retain(&mut self, paths: &std::collections::HashSet<PathBuf>) {
        self.entries.retain(|path, _| paths.contains(path));
    }
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

/// Downscale an engine-reported `favicon` to favicon size and cache it as a PNG
/// for `url`'s origin.
///
/// Preferred over [`fetch_favicon`]: the engine resolves `<link rel="icon">`
/// (including SVG and `data:` URIs), and writing a fresh file here suppresses the
/// HTTP fallback for the same origin.
pub fn save_favicon(url: &str, favicon: &RgbaImage) -> anyhow::Result<()> {
    let path = favicon_path(url);
    ensure_parent(&path)?;
    let scaled = image::imageops::thumbnail(favicon, FAVICON_SIZE, FAVICON_SIZE);
    scaled.save(&path)?;
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
        .join("astrolabe")
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
    fn tile_image_cache_renews_handle_when_file_changes() {
        let path = std::env::temp_dir().join(format!("astrolabe-tile-{}.png", std::process::id()));
        std::fs::write(&path, b"first").expect("write test file");
        let mut cache = TileImageCache::default();

        let first = cache.load(&path).expect("file is readable");
        let again = cache.load(&path).expect("file is readable");
        assert_eq!(first.id(), again.id());

        std::fs::write(&path, b"second").expect("write test file");
        let file = std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open test file");
        file.set_modified(SystemTime::now() + Duration::from_secs(10))
            .expect("set modification time");
        let renewed = cache.load(&path).expect("file is readable");
        assert_ne!(first.id(), renewed.id());

        std::fs::remove_file(&path).expect("remove test file");
        assert!(cache.load(&path).is_none());
    }

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
        let path = std::env::temp_dir().join("astrolabe-nonexistent-xyz.png");
        let _ = std::fs::remove_file(&path);
        assert!(is_stale(&path, MAX_AGE));
    }

    #[test]
    fn fresh_file_is_not_stale() {
        let path = std::env::temp_dir().join(format!("astrolabe-fresh-{}.png", std::process::id()));
        std::fs::write(&path, b"x").expect("write temp file");
        assert!(!is_stale(&path, MAX_AGE));
        // A zero max-age makes any existing file count as stale.
        assert!(is_stale(&path, Duration::ZERO));
        let _ = std::fs::remove_file(&path);
    }
}
