//! User scripts and user stylesheet stored as files under the XDG config dir.
//!
//! Layout:
//! - `<config>/cosmic-web/userscripts/*.js` — one user script per file.
//! - `<config>/cosmic-web/user.css` — the user stylesheet.
//!
//! Enable/disable state lives in [`BrowserConfig`] (`disabled_userscripts`,
//! `user_stylesheet_enabled`); the files themselves are edited externally. The
//! settings UI lists the scripts and shows the directory so users know where to
//! drop new ones.

use std::path::PathBuf;

use crate::config::BrowserConfig;

/// The `cosmic-web` config directory (created lazily by callers that write).
fn base_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("cosmic-web")
}

/// Directory holding `*.js` user scripts.
pub fn scripts_dir() -> PathBuf {
    base_dir().join("userscripts")
}

/// Path of the user stylesheet file.
pub fn stylesheet_path() -> PathBuf {
    base_dir().join("user.css")
}

/// A user script file and whether it is currently enabled.
#[derive(Clone, Debug)]
pub struct ScriptEntry {
    /// File name, e.g. `dark-mode.js`. Used as the toggle/remove key.
    pub name: String,
    pub enabled: bool,
}

/// Lists the `*.js` files in the userscripts directory, sorted by name, marking
/// each enabled unless it appears in `config.disabled_userscripts`. Returns an
/// empty list if the directory does not exist yet.
pub fn list_scripts(config: &BrowserConfig) -> Vec<ScriptEntry> {
    let mut names: Vec<String> = match std::fs::read_dir(scripts_dir()) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter_map(|n| n.into_string().ok())
            .filter(|n| n.ends_with(".js"))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
        .into_iter()
        .map(|name| ScriptEntry {
            enabled: !config.disabled_userscripts.contains(&name),
            name,
        })
        .collect()
}

/// Reads the enabled user scripts and (if enabled) the user stylesheet into a
/// [`cosmic_webview::UserContent`] ready for `set_user_content`.
pub fn load(config: &BrowserConfig) -> cosmic_webview::UserContent {
    let scripts = list_scripts(config)
        .into_iter()
        .filter(|s| s.enabled)
        .filter_map(|s| {
            let path = scripts_dir().join(&s.name);
            std::fs::read_to_string(&path)
                .ok()
                .map(|source| cosmic_webview::UserContentItem {
                    source,
                    path: Some(path),
                })
        })
        .collect();

    let stylesheets = if config.user_stylesheet_enabled {
        let path = stylesheet_path();
        std::fs::read_to_string(&path)
            .ok()
            .filter(|css| !css.trim().is_empty())
            .map(|source| {
                vec![cosmic_webview::UserContentItem {
                    source,
                    path: Some(path),
                }]
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    cosmic_webview::UserContent {
        scripts,
        stylesheets,
    }
}

/// Deletes a user script file. Errors (e.g. missing file) are ignored.
pub fn remove_script(name: &str) {
    let path = scripts_dir().join(name);
    if let Err(e) = std::fs::remove_file(&path) {
        tracing::warn!("Failed to remove userscript {name}: {e}");
    }
}

/// Opens the userscripts directory in the file manager, creating it first.
pub fn open_scripts_dir() {
    let dir = scripts_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("Failed to create userscripts dir: {e}");
        return;
    }
    open_path(&dir);
}

/// Opens the user stylesheet in the default editor, creating an empty file and
/// its parent directory first if needed.
pub fn open_stylesheet() {
    let path = stylesheet_path();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("Failed to create config dir: {e}");
        return;
    }
    if !path.exists()
        && let Err(e) = std::fs::write(&path, "/* cosmic-web user stylesheet */\n")
    {
        tracing::warn!("Failed to create user.css: {e}");
        return;
    }
    open_path(&path);
}

/// Launches `xdg-open` on `path`, detached. Failures are logged, not fatal.
fn open_path(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("xdg-open").arg(path).spawn() {
        tracing::warn!("Failed to open {}: {e}", path.display());
    }
}
