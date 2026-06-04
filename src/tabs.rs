use async_channel::Receiver;
use cosmic_webview::{ServoState, WebViewFrame};
use dpi::PhysicalSize;
use url::Url;

/// State owned by a single browser tab.
///
/// The URL bar belongs here (not at the window level) so each tab
/// independently remembers its address field text and focus state —
/// the same way Firefox does it.
pub struct TabState {
    pub servo: ServoState,
    pub frame: WebViewFrame,
    /// Current text in this tab's URL bar.
    pub url_bar_text: String,
    /// True while this tab's URL bar has keyboard focus.
    pub url_bar_focused: bool,
}

impl TabState {
    /// Create a new tab loading `url` at the given physical `size` and HiDPI `scale`.
    pub fn new(url: Url, size: PhysicalSize<u32>, scale: f32) -> (Self, Receiver<()>) {
        let url_bar_text = url.to_string();
        let (servo, rx) = ServoState::new(url, size, scale);
        (
            Self {
                servo,
                frame: WebViewFrame::new(),
                url_bar_text,
                url_bar_focused: false,
            },
            rx,
        )
    }
}
