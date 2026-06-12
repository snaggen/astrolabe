use std::sync::LazyLock;

use cosmic_webview::PermissionFeature;
use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::config::{BUILTIN_ENGINES, CookieProtectionLevel, SearchEngine};
use crate::db::{Suggestion, SuggestionKind};

/// Stable widget id for the URL bar text input, so keyboard shortcuts can move
/// focus to it (Ctrl+L). Must stay constant across `view()` calls.
pub static URL_BAR_ID: LazyLock<widget::Id> = LazyLock::new(|| widget::Id::new("url_bar_input"));

/// Connection security for the active page, derived from its URL scheme.
///
/// Servo does not expose TLS certificate details through the embedding API, so
/// the indicator distinguishes only encrypted from unencrypted transports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityStatus {
    /// `https://` — the transport is encrypted.
    Secure,
    /// `http://` — the transport is not encrypted.
    Insecure,
    /// Internal pages (`about:`, the New Tab page) where security does not apply.
    Unknown,
}

impl SecurityStatus {
    /// Classify a URL by its scheme.
    pub fn from_url(url: &str) -> Self {
        if url.starts_with("https://") {
            Self::Secure
        } else if url.starts_with("http://") {
            Self::Insecure
        } else {
            Self::Unknown
        }
    }
}

/// Identifies which URL-bar indicator popover is open. Only one opens at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlBarPopover {
    Security,
    Cookie,
    Permissions,
    Zoom,
}

/// The address bar field: a single input-looking control with clickable
/// indicators on the left, the editable URL text in the middle, and the
/// bookmark star on the right.
///
/// Rendered as a styled container wrapping a borderless ([`Inline`]) text
/// input, because the text input's `leading_icon` slot is decorative — it does
/// not forward click events — so the left indicators must be real widgets
/// beside the input rather than icons inside it.
///
/// [`Inline`]: libcosmic::theme::TextInput::Inline
pub struct AddressBar<'a> {
    /// Current URL-bar text.
    pub text: &'a str,
    /// Connection security of the active page.
    pub security: SecurityStatus,
    /// Effective cookie-protection level for the active site (override, else
    /// the global default).
    pub cookie_level: CookieProtectionLevel,
    /// Permissions the user has granted for the active page; the indicator is
    /// hidden when empty.
    pub permissions: &'a [PermissionFeature],
    /// Whether the active page is bookmarked (controls the star icon).
    pub is_bookmarked: bool,
    /// Active page zoom (`1.0` = 100 %); the zoom label is hidden at 100 %.
    pub zoom: f32,
    /// Which indicator popover is currently open, if any.
    pub open_popover: Option<UrlBarPopover>,
}

impl<'a> AddressBar<'a> {
    /// Build the address bar element.
    pub fn view(self) -> Element<'a, Message> {
        let input = widget::text_input("https://example.com", self.text)
            .id(URL_BAR_ID.clone())
            .on_focus(Message::UrlBarFocused)
            .on_unfocus(Message::UrlBarUnfocused)
            .on_input(Message::UrlBarChanged)
            .on_submit(|_| Message::Navigate)
            .select_on_focus(true)
            .style(libcosmic::theme::TextInput::Inline)
            .width(Length::Fill);

        // Cookie and security indicators apply only to real web pages, not to
        // internal pages like the New Tab page.
        let is_web_page = self.security != SecurityStatus::Unknown;
        let cookie = is_web_page.then(|| cookie_indicator(self.cookie_level, self.open_popover));

        // The row keeps a fixed slot layout: absent indicators render as a
        // zero-size placeholder. This keeps the text input at a constant child
        // index so iced never reconciles its widget state against a sibling's
        // (which panics in `TextInput::layout`).
        let row = widget::row::with_capacity(6)
            .align_y(Alignment::Center)
            .push(slot(security_indicator(self.security, self.open_popover)))
            .push(slot(cookie))
            .push(slot(permission_indicator(
                self.permissions,
                self.open_popover,
            )))
            .push(input)
            .push(slot(zoom_indicator(self.zoom, self.open_popover)))
            .push(bookmark_button(self.is_bookmarked));

        widget::container(row)
            .padding([0, 4])
            .width(Length::Fill)
            .class(libcosmic::theme::Container::Card)
            .into()
    }
}

/// Wrap an optional indicator in a fixed row slot: the widget when present, or
/// a zero-size placeholder so the slot still occupies a stable child index.
fn slot(indicator: Option<Element<'_, Message>>) -> Element<'_, Message> {
    indicator.unwrap_or_else(|| widget::Space::new().into())
}

/// The security indicator: a clickable icon with a hover tooltip and a popover
/// describing the connection state. Returns `None` for internal pages where no
/// security state applies.
fn security_indicator<'a>(
    security: SecurityStatus,
    open: Option<UrlBarPopover>,
) -> Option<Element<'a, Message>> {
    let (icon_name, tip) = match security {
        SecurityStatus::Secure => ("channel-secure-symbolic", "Connection is secure"),
        SecurityStatus::Insecure => ("channel-insecure-symbolic", "Connection is not secure"),
        SecurityStatus::Unknown => return None,
    };

    let button = widget::button::icon(widget::icon::from_name(icon_name))
        .on_press(Message::ToggleUrlBarPopover(UrlBarPopover::Security));
    let anchor = widget::tooltip(button, widget::text(tip), widget::tooltip::Position::Bottom);

    let mut popover = widget::popover(anchor)
        .position(widget::popover::Position::Bottom)
        .on_close(Message::CloseUrlBarPopover);
    if open == Some(UrlBarPopover::Security) {
        popover = popover.popup(security_popup(security));
    }
    Some(popover.into())
}

/// Popover body shown when the security indicator is clicked.
fn security_popup<'a>(security: SecurityStatus) -> Element<'a, Message> {
    let body = match security {
        SecurityStatus::Secure => "Your connection to this site is encrypted.",
        SecurityStatus::Insecure => {
            "Your connection to this site is not encrypted. \
             Information you send could be visible to others."
        }
        SecurityStatus::Unknown => "No security information is available for this page.",
    };
    widget::container(widget::text(body))
        .padding(12)
        .width(Length::Fixed(280.0))
        .class(libcosmic::theme::Container::Dialog)
        .into()
}

/// The cookie/tracking-protection indicator: a clickable shield icon with a
/// hover tooltip and a popover for choosing the per-site level.
fn cookie_indicator<'a>(
    level: CookieProtectionLevel,
    open: Option<UrlBarPopover>,
) -> Element<'a, Message> {
    let button = widget::button::icon(widget::icon::from_name("security-medium-symbolic"))
        .on_press(Message::ToggleUrlBarPopover(UrlBarPopover::Cookie));
    let anchor = widget::tooltip(
        button,
        widget::text(format!("Tracking protection: {}", level.name())),
        widget::tooltip::Position::Bottom,
    );

    let mut popover = widget::popover(anchor)
        .position(widget::popover::Position::Bottom)
        .on_close(Message::CloseUrlBarPopover);
    if open == Some(UrlBarPopover::Cookie) {
        popover = popover.popup(cookie_popup(level));
    }
    popover.into()
}

/// Popover body: the current level and the three selectable options.
///
/// Selecting an option sets a per-site override. The choice takes effect once
/// Fas 9 wires blocking into Servo's network layer.
fn cookie_popup<'a>(level: CookieProtectionLevel) -> Element<'a, Message> {
    let mut column = widget::column::with_capacity(4)
        .spacing(8)
        .width(Length::Fixed(240.0))
        .push(widget::text::body("Tracking protection for this site"));

    for option in [
        CookieProtectionLevel::Standard,
        CookieProtectionLevel::Strict,
        CookieProtectionLevel::Custom,
    ] {
        let button = if option == level {
            widget::button::suggested(option.name())
        } else {
            widget::button::standard(option.name())
        };
        column = column.push(
            button
                .on_press(Message::SetCookieProtectionOverride(option))
                .width(Length::Fill),
        );
    }

    widget::container(column)
        .padding(12)
        .class(libcosmic::theme::Container::Dialog)
        .into()
}

/// Human-readable name for a permission feature.
fn permission_label(feature: PermissionFeature) -> &'static str {
    match feature {
        PermissionFeature::Geolocation => "Location",
        PermissionFeature::Notifications => "Notifications",
        PermissionFeature::Push => "Push messages",
        PermissionFeature::Midi => "MIDI devices",
        PermissionFeature::Camera => "Camera",
        PermissionFeature::Microphone => "Microphone",
        PermissionFeature::Speaker => "Speaker selection",
        PermissionFeature::DeviceInfo => "Device information",
        PermissionFeature::BackgroundSync => "Background sync",
        PermissionFeature::Bluetooth => "Bluetooth",
        PermissionFeature::PersistentStorage => "Persistent storage",
        PermissionFeature::ScreenWakeLock(_) => "Screen wake lock",
        PermissionFeature::Gamepad => "Gamepad",
    }
}

/// The permissions indicator: a key icon shown only when the page has active
/// grants. Clicking it lists them with a revoke action.
fn permission_indicator<'a>(
    permissions: &[PermissionFeature],
    open: Option<UrlBarPopover>,
) -> Option<Element<'a, Message>> {
    if permissions.is_empty() {
        return None;
    }

    let button = widget::button::icon(widget::icon::from_name("dialog-password-symbolic"))
        .on_press(Message::ToggleUrlBarPopover(UrlBarPopover::Permissions));
    let anchor = widget::tooltip(
        button,
        widget::text("Site permissions"),
        widget::tooltip::Position::Bottom,
    );

    let mut popover = widget::popover(anchor)
        .position(widget::popover::Position::Bottom)
        .on_close(Message::CloseUrlBarPopover);
    if open == Some(UrlBarPopover::Permissions) {
        popover = popover.popup(permission_popup(permissions));
    }
    Some(popover.into())
}

/// Popover body listing each granted permission with a revoke button.
fn permission_popup<'a>(permissions: &[PermissionFeature]) -> Element<'a, Message> {
    let mut column = widget::column::with_capacity(permissions.len() + 1)
        .spacing(8)
        .width(Length::Fixed(260.0))
        .push(widget::text::body("Permissions for this site"));

    for &feature in permissions {
        let row = widget::row::with_capacity(2)
            .align_y(Alignment::Center)
            .push(widget::text(permission_label(feature)).width(Length::Fill))
            .push(widget::button::text("Revoke").on_press(Message::PermissionRevoke(feature)));
        column = column.push(row);
    }

    widget::container(column)
        .padding(12)
        .class(libcosmic::theme::Container::Dialog)
        .into()
}

/// Inline banner that asks the user to allow or block a permission request.
/// Shown below the toolbar while a request is pending.
pub fn permission_prompt<'a>(feature: PermissionFeature) -> Element<'a, Message> {
    let row = widget::row::with_capacity(3)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(
            widget::text(format!(
                "This site is requesting access to: {}",
                permission_label(feature)
            ))
            .width(Length::Fill),
        )
        .push(widget::button::standard("Block").on_press(Message::PermissionDeny))
        .push(widget::button::suggested("Allow").on_press(Message::PermissionAllow));

    widget::container(row)
        .padding(8)
        .width(Length::Fill)
        .class(libcosmic::theme::Container::Primary)
        .into()
}

/// The zoom indicator: a percentage label shown only when zoom differs from
/// 100 %. Clicking it opens a popover with zoom controls.
fn zoom_indicator<'a>(zoom: f32, open: Option<UrlBarPopover>) -> Option<Element<'a, Message>> {
    let percent = (zoom * 100.0).round() as i32;
    if percent == 100 {
        return None;
    }

    let label = widget::button::text(format!("{percent} %"))
        .on_press(Message::ToggleUrlBarPopover(UrlBarPopover::Zoom));
    let anchor = widget::tooltip(
        label,
        widget::text("Page zoom – click to adjust"),
        widget::tooltip::Position::Bottom,
    );

    let mut popover = widget::popover(anchor)
        .position(widget::popover::Position::Bottom)
        .on_close(Message::CloseUrlBarPopover);
    if open == Some(UrlBarPopover::Zoom) {
        popover = popover.popup(zoom_popup(percent));
    }
    Some(popover.into())
}

/// Popover body with `−  <percent>  +` controls and a reset button.
fn zoom_popup<'a>(percent: i32) -> Element<'a, Message> {
    let controls = widget::row::with_capacity(3)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(
            widget::button::icon(widget::icon::from_name("list-remove-symbolic"))
                .on_press(Message::ZoomOut),
        )
        .push(widget::text(format!("{percent} %")))
        .push(
            widget::button::icon(widget::icon::from_name("list-add-symbolic"))
                .on_press(Message::ZoomIn),
        );

    let column = widget::column::with_capacity(2)
        .spacing(8)
        .align_x(Alignment::Center)
        .push(controls)
        .push(widget::button::text("Reset").on_press(Message::ZoomReset));

    widget::container(column)
        .padding(12)
        .class(libcosmic::theme::Container::Dialog)
        .into()
}

/// The bookmark star shown at the right edge of the address bar.
fn bookmark_button<'a>(is_bookmarked: bool) -> Element<'a, Message> {
    let icon = if is_bookmarked {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    };
    widget::button::icon(widget::icon::from_name(icon))
        .on_press(Message::BookmarkToggle)
        .into()
}

/// Fetch autocomplete suggestions from the engine's suggestion API.
///
/// Returns an empty vec (silently) if the engine has no API, the request
/// times out, or the response cannot be parsed.
pub async fn fetch_suggestions(engine: SearchEngine, query: String) -> Vec<Suggestion> {
    let Some(url) = engine.suggestion_url(&query) else {
        return vec![];
    };
    let Ok(resp) = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(3))
        .header("Accept", "application/json")
        .send()
        .await
    else {
        return vec![];
    };
    let Ok(text) = resp.text().await else {
        return vec![];
    };
    parse_opensearch(&text)
}

/// Parse an OpenSearch-format suggestion response.
///
/// Format: `["query", ["suggestion1", "suggestion2", ...], ...]`
/// Used by DuckDuckGo, Ecosia, Brave and many others.
fn parse_opensearch(json: &str) -> Vec<Suggestion> {
    let Ok(serde_json::Value::Array(root)) = serde_json::from_str(json) else {
        return vec![];
    };
    let Some(serde_json::Value::Array(items)) = root.get(1) else {
        return vec![];
    };
    items
        .iter()
        .filter_map(|v| v.as_str())
        .map(|s| Suggestion {
            url: s.to_owned(),
            title: String::new(),
            kind: SuggestionKind::Search,
        })
        .collect()
}

// Max suggestions shown before cutting off (avoids a tall unscrollable popup).
const MAX_SUGGESTIONS: usize = 6;

// All built-in engines shown as quick-search buttons.
const ENGINES: &[SearchEngine] = BUILTIN_ENGINES;

/// Popup shown below the URL bar while the user is typing.
///
/// Always displays the quick-search engine row at the bottom. Local history
/// and bookmark matches appear above it (up to [`MAX_SUGGESTIONS`] entries).
pub fn suggestion_list<'a>(
    suggestions: &'a [Suggestion],
    selected: Option<usize>,
    active_engine: &'a SearchEngine,
) -> Element<'a, Message> {
    let visible = &suggestions[..suggestions.len().min(MAX_SUGGESTIONS)];

    let mut col = widget::column::with_capacity(MAX_SUGGESTIONS + 2);

    for (i, s) in visible.iter().enumerate() {
        col = col.push(suggestion_row(i, s, selected == Some(i)));
    }

    if !visible.is_empty() {
        col = col.push(widget::divider::horizontal::default());
    }

    widget::container(col.push(engine_row(active_engine)).width(Length::Fill))
        .class(libcosmic::theme::Container::Secondary)
        .width(Length::Fill)
        .into()
}

fn suggestion_row<'a>(index: usize, s: &'a Suggestion, selected: bool) -> Element<'a, Message> {
    let icon_name = match s.kind {
        SuggestionKind::Bookmark => "user-bookmarks-symbolic",
        SuggestionKind::History => "document-open-recent-symbolic",
        SuggestionKind::Search => "system-search-symbolic",
        SuggestionKind::SavedQuery => "document-open-recent-symbolic",
    };

    let title: &str = if s.title.is_empty() { &s.url } else { &s.title };

    // SavedQuery shows just the query text on one line; others show title + URL below.
    let info: Element<'_, Message> = if matches!(s.kind, SuggestionKind::SavedQuery) {
        widget::text(title).width(Length::Fill).into()
    } else {
        widget::column::with_capacity(2)
            .push(widget::text(title).width(Length::Fill))
            .push(widget::text(&s.url).size(12).width(Length::Fill))
            .width(Length::Fill)
            .into()
    };

    let row = widget::row::with_capacity(2)
        .push(widget::icon(
            widget::icon::from_name(icon_name).size(16).into(),
        ))
        .push(info)
        .align_y(Alignment::Center)
        .spacing(8);

    let styled = if selected {
        widget::container(row)
            .class(libcosmic::theme::Container::Primary)
            .width(Length::Fill)
            .padding([4, 8])
    } else {
        widget::container(row).width(Length::Fill).padding([4, 8])
    };

    widget::mouse_area(styled)
        .on_press(Message::SuggestionActivated(index))
        .into()
}

/// Quick-access row of search engine buttons at the bottom of the popup.
fn engine_row<'a>(active: &'a SearchEngine) -> Element<'a, Message> {
    let buttons: Vec<Element<'a, Message>> = ENGINES
        .iter()
        .map(|engine| {
            widget::button::text(engine.name())
                .on_press_maybe(
                    (engine != active).then_some(Message::SearchWithEngine(engine.clone())),
                )
                .into()
        })
        .collect();

    widget::container(widget::row::with_children(buttons).spacing(4))
        .padding([4, 8])
        .width(Length::Fill)
        .into()
}
