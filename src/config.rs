use cosmic_config::CosmicConfigEntry;
use cosmic_config::cosmic_config_derive::CosmicConfigEntry;
use serde::{Deserialize, Serialize};

/// What a tab shows when it opens — used for both startup and new tabs.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum StartupPage {
    /// Show the built-in New Tab page.
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
    /// Human-readable display name used in the awesomebar engine row.
    pub fn name(&self) -> &str {
        match self {
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Kagi => "Kagi",
            Self::Brave => "Brave",
            Self::Ecosia => "Ecosia",
            Self::Startpage => "Startpage",
            Self::Qwant => "Qwant",
            Self::Custom(_) => "Custom",
        }
    }

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

    /// Returns `(hostname, query_param_name)` needed to recognise and parse search
    /// result URLs produced by this engine.
    ///
    /// For `Custom`, both values are derived from the URL template: the host comes
    /// from the parsed template URL, and the query parameter is whichever one
    /// contains the `{query}` placeholder.  Returns `None` if the template is
    /// malformed or carries no `{query}` parameter.
    pub fn search_host_and_param(&self) -> Option<(String, String)> {
        match self {
            Self::DuckDuckGo => Some(("duckduckgo.com".into(), "q".into())),
            Self::Kagi => Some(("kagi.com".into(), "q".into())),
            Self::Brave => Some(("search.brave.com".into(), "q".into())),
            Self::Ecosia => Some(("www.ecosia.org".into(), "q".into())),
            Self::Startpage => Some(("www.startpage.com".into(), "q".into())),
            Self::Qwant => Some(("www.qwant.com".into(), "q".into())),
            Self::Custom(template) => {
                // Replace the placeholder with a sentinel value so we can identify
                // which query parameter it ends up in after URL parsing.
                const SENTINEL: &str = "QSENTINEL42";
                let dummy = template.replace("{query}", SENTINEL);
                let parsed = url::Url::parse(&dummy).ok()?;
                let host = parsed.host_str()?.to_owned();
                let param = parsed
                    .query_pairs()
                    .find(|(_, v)| v.contains(SENTINEL))
                    .map(|(k, _)| k.into_owned())?;
                Some((host, param))
            }
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

/// All built-in (non-custom) search engines, in display order.
pub const BUILTIN_ENGINES: &[SearchEngine] = &[
    SearchEngine::DuckDuckGo,
    SearchEngine::Kagi,
    SearchEngine::Brave,
    SearchEngine::Ecosia,
    SearchEngine::Startpage,
    SearchEngine::Qwant,
];

/// If `url` is a search results page from any engine in `engines`, return the decoded query.
///
/// The hostname and query parameter name are derived from each engine's own
/// [`SearchEngine::search_host_and_param`] — no separate hardcoded lists.
pub fn extract_search_query(url: &str, engines: &[SearchEngine]) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    for engine in engines {
        if let Some((engine_host, param)) = engine.search_host_and_param()
            && host == engine_host
        {
            return parsed
                .query_pairs()
                .find(|(k, _)| k.as_ref() == param.as_str())
                .map(|(_, v)| v.into_owned());
        }
    }
    None
}

/// Tracker- and cookie-blocking strength.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum CookieProtectionLevel {
    /// Blocks common trackers and cross-site cookies (EasyPrivacy + custom lists).
    #[default]
    Standard,
    /// Blocks more aggressively (EasyPrivacy + uBlock Origin + custom lists).
    /// May break some sites.
    Strict,
}

impl CookieProtectionLevel {
    /// Human-readable name shown in the indicator popover.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Strict => "Strict",
        }
    }
}

/// Time range for the "Clear browsing data" action. Only history is filtered by
/// range; cookies and site data are always cleared in full (Servo's engine has no
/// time-based cookie filter).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClearRange {
    LastHour,
    LastDay,
    LastWeek,
    LastMonth,
    #[default]
    AllTime,
}

impl ClearRange {
    /// Human-readable label shown in the range dropdown.
    pub fn label(&self) -> &'static str {
        match self {
            Self::LastHour => "Last hour",
            Self::LastDay => "Last 24 hours",
            Self::LastWeek => "Last 7 days",
            Self::LastMonth => "Last 4 weeks",
            Self::AllTime => "All time",
        }
    }

    /// Number of seconds the range covers, or `None` for "all time".
    pub fn within_seconds(&self) -> Option<i64> {
        let day = 24 * 60 * 60;
        match self {
            Self::LastHour => Some(60 * 60),
            Self::LastDay => Some(day),
            Self::LastWeek => Some(7 * day),
            Self::LastMonth => Some(28 * day),
            Self::AllTime => None,
        }
    }

    /// All variants in dropdown order.
    pub const ALL: [ClearRange; 5] = [
        Self::LastHour,
        Self::LastDay,
        Self::LastWeek,
        Self::LastMonth,
        Self::AllTime,
    ];
}

/// A user-configured filter list (URL and metadata).
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FilterListEntry {
    /// Short label shown in the settings UI.
    pub label: String,
    /// HTTPS URL to the filter list (ABP / uBlock format).
    pub url: String,
    /// Whether this list is currently active.
    pub enabled: bool,
}

/// Default response to a site's permission request when the user has made no
/// per-site choice.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum PermissionDefault {
    /// Prompt the user every time (browser default).
    #[default]
    Ask,
    /// Grant the permission automatically.
    Allow,
    /// Deny the permission automatically.
    Block,
}

impl PermissionDefault {
    /// Human-readable label shown in the defaults dropdown.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::Allow => "Allow",
            Self::Block => "Block",
        }
    }

    /// All variants in dropdown order.
    pub const ALL: [PermissionDefault; 3] = [Self::Ask, Self::Allow, Self::Block];
}

/// A permission category that exposes a configurable global default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionKind {
    Geolocation,
    Notifications,
    Camera,
    Microphone,
    PersistentStorage,
}

impl PermissionKind {
    /// Human-readable label shown in the settings UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Geolocation => "Location",
            Self::Notifications => "Notifications",
            Self::Camera => "Camera",
            Self::Microphone => "Microphone",
            Self::PersistentStorage => "Persistent storage",
        }
    }

    /// All configurable categories, in display order.
    pub const ALL: [PermissionKind; 5] = [
        Self::Geolocation,
        Self::Notifications,
        Self::Camera,
        Self::Microphone,
        Self::PersistentStorage,
    ];
}

/// Global default response for each configurable permission category.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct PermissionDefaults {
    pub geolocation: PermissionDefault,
    pub notifications: PermissionDefault,
    pub camera: PermissionDefault,
    pub microphone: PermissionDefault,
    pub persistent_storage: PermissionDefault,
}

impl PermissionDefaults {
    /// The configured default for `kind`.
    pub fn get(&self, kind: PermissionKind) -> PermissionDefault {
        match kind {
            PermissionKind::Geolocation => self.geolocation,
            PermissionKind::Notifications => self.notifications,
            PermissionKind::Camera => self.camera,
            PermissionKind::Microphone => self.microphone,
            PermissionKind::PersistentStorage => self.persistent_storage,
        }
    }

    /// Set the default for `kind`.
    pub fn set(&mut self, kind: PermissionKind, value: PermissionDefault) {
        match kind {
            PermissionKind::Geolocation => self.geolocation = value,
            PermissionKind::Notifications => self.notifications = value,
            PermissionKind::Camera => self.camera = value,
            PermissionKind::Microphone => self.microphone = value,
            PermissionKind::PersistentStorage => self.persistent_storage = value,
        }
    }
}

/// Persistent browser configuration, stored via `cosmic_config`.
#[derive(Clone, CosmicConfigEntry, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
#[version = 1]
pub struct BrowserConfig {
    /// What the first tab shows when the browser starts.
    pub startup_page: StartupPage,
    /// What a newly opened tab shows. Independent of `startup_page` so the New
    /// Tab page stays reachable even when startup is set to a fixed URL.
    pub new_tab_page: StartupPage,
    /// Fixed URL used when `startup_page == StartupPage::Url`.
    pub home_url: String,
    /// Search engine used for non-URL input in the address bar.
    pub search_engine: SearchEngine,
    /// Number of columns in the New Tab page top-sites grid.
    pub new_tab_columns: u16,
    /// Number of rows in the New Tab page top-sites grid.
    pub new_tab_rows: u16,
    /// Default tracker/cookie protection level for new sites.
    pub cookie_protection: CookieProtectionLevel,
    /// User-configured external filter lists (ABP / uBlock format).
    pub custom_filter_lists: Vec<FilterListEntry>,
    /// Global default response for each configurable permission category.
    pub permission_defaults: PermissionDefaults,
    /// Whether sites may use the camera and microphone. Off by default because
    /// Servo grants `getUserMedia` without a per-site prompt.
    pub allow_camera_microphone: bool,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            startup_page: StartupPage::default(),
            new_tab_page: StartupPage::NewTab,
            home_url: String::from("https://start.duckduckgo.com"),
            search_engine: SearchEngine::default(),
            new_tab_columns: 4,
            new_tab_rows: 2,
            cookie_protection: CookieProtectionLevel::default(),
            custom_filter_lists: Vec::new(),
            permission_defaults: PermissionDefaults::default(),
            allow_camera_microphone: false,
        }
    }
}
