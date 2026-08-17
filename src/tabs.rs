use async_channel::Receiver;
use cosmic_webview::{
    AllowOrDenyRequest, BluetoothDeviceSelectionRequest, FindBarState, PermissionRequest,
    ServoState,
};
use dpi::PhysicalSize;
use url::Url;

/// What a tab displays. Most tabs render a web page; a `Settings` tab renders
/// the in-app settings page instead of its (unused) webview.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TabKind {
    /// A normal web page (or the internal New Tab page).
    #[default]
    Web,
    /// The in-app settings page.
    Settings,
}

/// State owned by a single browser tab.
///
/// The URL bar belongs here (not at the window level) so each tab
/// independently remembers its address field text and focus state —
/// the same way Firefox does it.
///
/// The frame buffer lives inside [`ServoState`]; the engine fills it
/// directly and `view()` passes `servo.frame()` to the webview widget.
pub struct TabState {
    pub servo: ServoState,
    /// What this tab displays — a web page or an in-app page.
    pub kind: TabKind,
    /// Current text in this tab's URL bar.
    pub url_bar_text: String,
    /// True while this tab's URL bar has keyboard focus.
    pub url_bar_focused: bool,
    /// The URL that was last written to the history database for this tab.
    /// Used to avoid logging the same URL twice in a row.
    pub logged_url: Option<String>,
    /// True while this tab shows the internal New Tab page instead of the
    /// webview. Set when the tab opens as a New Tab and cleared on navigation.
    pub show_new_tab_page: bool,
    /// URL whose thumbnail this tab has already captured, to avoid recapturing
    /// the same page on every wake once it has finished loading.
    pub thumb_captured_url: Option<String>,
    /// Per-site cookie-protection level chosen via the URL-bar indicator, or
    /// `None` to follow the global default. Reset when the tab navigates.
    pub cookie_protection_override: Option<crate::config::CookieProtectionLevel>,
    /// Per-site `prefers-color-scheme` chosen via the URL-bar indicator, or
    /// `None` to follow the global preference. Reset when the tab navigates.
    pub theme_override: Option<crate::config::ThemeChoice>,
    /// Host the overrides above were loaded for. Compared against the page's
    /// host on every engine wake to notice that the tab moved to another site.
    pub settings_domain: Option<String>,
    /// A permission request from the page awaiting the user's allow/deny choice.
    /// At most one is shown at a time; a newer request replaces an unanswered one.
    pub pending_permission: Option<PermissionRequest>,
    /// A Bluetooth device-selection request from the page awaiting the user's
    /// choice. Shown as a picker dialog; a newer request replaces an unanswered one.
    pub pending_bluetooth: Option<BluetoothDeviceSelectionRequest>,
    /// A `beforeunload` request from a page that reports unsaved work, awaiting
    /// the user's confirmation. Dropping it lets the page go, as Servo defaults to.
    pub pending_unload: Option<AllowOrDenyRequest>,
    /// Set when an engine-level change (e.g. web fonts) needs this tab reloaded,
    /// deferred until the tab becomes visible to avoid reloading every tab at once.
    pub needs_reload: bool,
    /// Active find-in-page session, or `None` when the find bar is closed.
    pub find_bar: Option<FindBarState>,
}

impl TabState {
    /// Create a new tab loading `url` at the given physical `size` and HiDPI `scale`.
    pub fn new(url: Url, size: PhysicalSize<u32>, scale: f32) -> (Self, Receiver<()>) {
        let url_bar_text = url.to_string();
        let (servo, rx) = ServoState::new(url, size, scale);
        (Self::with_servo(servo, url_bar_text), rx)
    }

    /// Wrap an existing engine-created [`ServoState`] — e.g. a popup from
    /// `window.open()` or a `target="_blank"` link — in a tab.
    ///
    /// The engine already drives the WebView, so no new waker receiver is
    /// produced: all tabs share the one channel returned by the first tab.
    pub fn adopt(servo: ServoState, url_bar_text: String) -> Self {
        Self::with_servo(servo, url_bar_text)
    }

    /// Build a `Web` tab around an already-created [`ServoState`].
    fn with_servo(servo: ServoState, url_bar_text: String) -> Self {
        Self {
            servo,
            kind: TabKind::Web,
            url_bar_text,
            url_bar_focused: false,
            logged_url: None,
            show_new_tab_page: false,
            thumb_captured_url: None,
            cookie_protection_override: None,
            theme_override: None,
            settings_domain: None,
            pending_permission: None,
            pending_bluetooth: None,
            pending_unload: None,
            needs_reload: false,
            find_bar: None,
        }
    }
}
