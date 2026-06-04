use cosmic_config::CosmicConfigEntry;
use cosmic_config::cosmic_config_derive::CosmicConfigEntry;
use serde::{Deserialize, Serialize};

/// Where to navigate when the browser starts or a new tab is opened.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum StartupPage {
    /// Show the built-in New Tab page (about:blank until Fas 7).
    #[default]
    NewTab,
    /// Navigate to a fixed URL.
    Url(String),
    /// Open a search using the default search engine.
    SearchEngine,
}

/// Built-in search engines.
///
/// Google and Bing are intentionally excluded. `Custom` covers any engine
/// not listed here via a URL template containing `{query}`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SearchEngine {
    /// <https://duckduckgo.com>
    #[default]
    DuckDuckGo,
    /// <https://kagi.com> — subscription-based, no ads or tracking.
    Kagi,
    /// <https://search.brave.com> — independent index, privacy-focused.
    Brave,
    /// <https://www.ecosia.org> — plants trees with ad revenue.
    Ecosia,
    /// <https://www.startpage.com> — anonymous Google proxy.
    Startpage,
    /// <https://www.qwant.com> — European, independent index.
    Qwant,
    /// Custom search URL. Must contain `{query}` as a placeholder.
    Custom(String),
}

impl SearchEngine {
    /// Build a search URL for the given query, percent-encoding it for the query string.
    pub fn search_url(&self, query: &str) -> String {
        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        match self {
            Self::DuckDuckGo => format!("https://duckduckgo.com/?q={encoded}"),
            Self::Kagi => format!("https://kagi.com/search?q={encoded}"),
            Self::Brave => format!("https://search.brave.com/search?q={encoded}"),
            Self::Ecosia => format!("https://www.ecosia.org/search?q={encoded}"),
            Self::Startpage => format!("https://www.startpage.com/search?q={encoded}"),
            Self::Qwant => format!("https://www.qwant.com/?q={encoded}&t=web"),
            Self::Custom(template) => template.replace("{query}", &encoded),
        }
    }

    /// Return an autocomplete suggestion URL for the given partial query, or
    /// `None` for engines that have no public suggestion API.
    /// Used by the Awesomebar (Fas 6).
    #[allow(dead_code)]
    pub fn suggestion_url(&self, query: &str) -> Option<String> {
        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        match self {
            Self::DuckDuckGo => Some(format!("https://duckduckgo.com/ac/?q={encoded}&type=list")),
            Self::Kagi => Some(format!(
                "https://kagisuggest.com/api/autosuggest?q={encoded}"
            )),
            Self::Brave => Some(format!("https://search.brave.com/api/suggest?q={encoded}")),
            Self::Ecosia => Some(format!(
                "https://ac.ecosia.org/autocomplete?q={encoded}&type=list"
            )),
            Self::Qwant => Some(format!(
                "https://api.qwant.com/v3/suggest?q={encoded}&locale=en_US"
            )),
            // No public suggestion API.
            Self::Startpage | Self::Custom(_) => None,
        }
    }
}

/// Persistent browser configuration, stored via `cosmic_config`.
#[derive(Clone, CosmicConfigEntry, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
#[version = 1]
pub struct BrowserConfig {
    /// What to show when opening a new tab or starting the browser.
    pub startup_page: StartupPage,
    /// Fixed URL used when `startup_page == StartupPage::Url`.
    pub home_url: String,
    /// Search engine used for non-URL input in the address bar.
    pub search_engine: SearchEngine,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            startup_page: StartupPage::default(),
            home_url: String::from("https://start.duckduckgo.com"),
            search_engine: SearchEngine::default(),
        }
    }
}
