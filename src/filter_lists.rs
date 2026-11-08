//! Filter list download and disk cache.
//!
//! Lists are stored in `$XDG_CACHE_HOME/astrolabe/filter-lists/<id>.txt`.
//! A cached file is considered fresh for [`MAX_AGE`]; after that it is
//! re-downloaded transparently in the background.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Result;

const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60); // 7 days

/// Built-in filter list URLs for the Standard protection level.
pub const STANDARD_LISTS: &[(&str, &str)] = &[(
    "easyprivacy",
    "https://easylist.to/easylist/easyprivacy.txt",
)];

/// Additional filter list URLs applied on top of Standard for Strict mode.
pub const STRICT_EXTRA_LISTS: &[(&str, &str)] = &[
    (
        "ublock-filters",
        "https://ublockorigin.github.io/uAssets/filters/filters.min.txt",
    ),
    (
        "ublock-privacy",
        "https://ublockorigin.github.io/uAssets/filters/privacy.min.txt",
    ),
];

/// Returns the cache directory for filter lists, creating it if absent.
pub fn cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("astrolabe").join("filter-lists"))
}

/// Load filter-list text from disk cache, downloading if missing or stale.
///
/// Returns empty string on any error so the engine still starts (just
/// without that list's rules).
pub async fn load_or_refresh(id: &str, url: &str, cache_dir: &Path) -> String {
    let path = cache_dir.join(format!("{id}.txt"));

    // Use cached version if it's fresh.
    if let Ok(age) = file_age(&path)
        && age < MAX_AGE
        && let Ok(text) = tokio::fs::read_to_string(&path).await
    {
        return text;
    }

    // Download; fall back to stale cache on failure.
    match download(url).await {
        Ok(text) => {
            let _ = tokio::fs::create_dir_all(cache_dir).await;
            let _ = tokio::fs::write(&path, &text).await;
            text
        }
        Err(err) => {
            tracing::warn!("Failed to download filter list {id} from {url}: {err:#}");
            tokio::fs::read_to_string(&path).await.unwrap_or_default()
        }
    }
}

async fn download(url: &str) -> Result<String> {
    let text = reqwest::get(url).await?.error_for_status()?.text().await?;
    Ok(text)
}

fn file_age(path: &Path) -> Result<Duration> {
    let modified = std::fs::metadata(path)?.modified()?;
    Ok(SystemTime::now().duration_since(modified)?)
}
