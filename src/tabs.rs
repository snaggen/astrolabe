use async_channel::Receiver;
use cosmic_webview::ServoState;
use dpi::PhysicalSize;
use url::Url;

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
}

impl TabState {
    /// Create a new tab loading `url` at the given physical `size` and HiDPI `scale`.
    pub fn new(url: Url, size: PhysicalSize<u32>, scale: f32) -> (Self, Receiver<()>) {
        let url_bar_text = url.to_string();
        let (servo, rx) = ServoState::new(url, size, scale);
        (
            Self {
                servo,
                url_bar_text,
                url_bar_focused: false,
                logged_url: None,
                show_new_tab_page: false,
                thumb_captured_url: None,
            },
            rx,
        )
    }
}
