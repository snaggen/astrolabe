use std::sync::LazyLock;

use astrolabe_webview::PermissionFeature;
use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::{Message, PermissionResponse};
use crate::config::{BUILTIN_ENGINES, CookieProtectionLevel, SearchEngine, ThemeChoice};
use crate::db::{Suggestion, SuggestionKind};

/// A permission decision for the active site, shown in the permissions popover.
pub struct PermissionEntry {
    /// Stable feature key, used to clear the decision.
    pub key: String,
    /// Human-readable feature label.
    pub label: &'static str,
    /// `true` if the feature is allowed, `false` if blocked.
    pub allowed: bool,
    /// `true` for a persistent (stored) decision, `false` for session-only.
    pub persistent: bool,
}

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
    Appearance,
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
    /// Per-site cookie-protection override for the active tab, if any.
    /// `None` means the global default applies.
    pub cookie_override: Option<CookieProtectionLevel>,
    /// The browser-wide default protection level (from settings).
    pub global_cookie_level: CookieProtectionLevel,
    /// Per-site theme override for the active tab, if any. `None` means the
    /// global preference applies.
    pub theme_override: Option<ThemeChoice>,
    /// The browser-wide theme preference for web content (from settings).
    pub global_theme: ThemeChoice,
    /// Permission decisions (allow/block, session/persistent) for the active
    /// site; the indicator is hidden when empty.
    pub permissions: Vec<PermissionEntry>,
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
        let input = widget::text_input("Search or enter address", self.text)
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
        let cookie = is_web_page.then(|| {
            cookie_indicator(
                self.cookie_override,
                self.global_cookie_level,
                self.open_popover,
            )
        });
        let appearance = is_web_page.then(|| {
            appearance_indicator(self.theme_override, self.global_theme, self.open_popover)
        });

        // The row keeps a fixed slot layout: absent indicators render as a
        // zero-size placeholder. This keeps the text input at a constant child
        // index so iced never reconciles its widget state against a sibling's
        // (which panics in `TextInput::layout`).
        let row = widget::row::with_capacity(7)
            .align_y(Alignment::Center)
            .push(slot(security_indicator(self.security, self.open_popover)))
            .push(slot(cookie))
            .push(slot(appearance))
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
        .class(libcosmic::theme::Container::Dialog(true))
        .into()
}

/// The cookie/tracking-protection indicator: a clickable shield icon with a
/// hover tooltip and a popover for choosing the per-site level.
fn cookie_indicator<'a>(
    cookie_override: Option<CookieProtectionLevel>,
    global: CookieProtectionLevel,
    open: Option<UrlBarPopover>,
) -> Element<'a, Message> {
    let effective = cookie_override.unwrap_or(global);
    let tip = if cookie_override.is_some() {
        format!("Tracking protection: {} (site override)", effective.name())
    } else {
        format!("Tracking protection: {}", effective.name())
    };

    let icon_name = match effective {
        CookieProtectionLevel::Standard => "security-medium-symbolic",
        CookieProtectionLevel::Strict => "security-high-symbolic",
    };
    let button = widget::button::icon(widget::icon::from_name(icon_name))
        .on_press(Message::ToggleUrlBarPopover(UrlBarPopover::Cookie));
    let anchor = widget::tooltip(button, widget::text(tip), widget::tooltip::Position::Bottom);

    let mut popover = widget::popover(anchor)
        .position(widget::popover::Position::Bottom)
        .on_close(Message::CloseUrlBarPopover);
    if open == Some(UrlBarPopover::Cookie) {
        popover = popover.popup(cookie_popup(cookie_override, global));
    }
    popover.into()
}

/// Dropdown choices for the per-site override: index 0 = use global default,
/// indices 1–2 map to Standard / Strict.
const OVERRIDE_OPTIONS: &[Option<CookieProtectionLevel>] = &[
    None,
    Some(CookieProtectionLevel::Standard),
    Some(CookieProtectionLevel::Strict),
];

fn override_option_label(
    opt: Option<CookieProtectionLevel>,
    global: CookieProtectionLevel,
) -> String {
    match opt {
        None => format!("Default ({})", global.name()),
        Some(level) => level.name().to_owned(),
    }
}

fn override_option_description(
    opt: Option<CookieProtectionLevel>,
    global: CookieProtectionLevel,
) -> &'static str {
    match opt.unwrap_or(global) {
        CookieProtectionLevel::Standard => {
            "Blocks common trackers and cross-site cookies using EasyPrivacy \
             and your custom filter lists. Recommended for most sites."
        }
        CookieProtectionLevel::Strict => {
            "Blocks more aggressively using EasyPrivacy, uBlock Origin filters, \
             and your custom filter lists. May break some sites."
        }
    }
}

/// Popover body: dropdown to pick the per-site protection level, with a short
/// description of what the selected level does. Choosing "Default" removes any
/// site-specific override and falls back to the browser-wide setting.
fn cookie_popup<'a>(
    cookie_override: Option<CookieProtectionLevel>,
    global: CookieProtectionLevel,
) -> Element<'a, Message> {
    let labels: Vec<String> = OVERRIDE_OPTIONS
        .iter()
        .map(|&opt| override_option_label(opt, global))
        .collect();
    let selected = OVERRIDE_OPTIONS.iter().position(|&o| o == cookie_override);

    let dropdown = widget::dropdown(labels, selected, |i| {
        let level = OVERRIDE_OPTIONS.get(i).copied().flatten();
        Message::SetCookieProtectionOverride(level)
    });

    let description = override_option_description(cookie_override, global);

    let column = widget::column::with_capacity(3)
        .spacing(10)
        .width(Length::Fixed(280.0))
        .push(widget::text::body("Tracking protection for this site"))
        .push(dropdown)
        .push(widget::text::caption(description));

    widget::container(column)
        .padding(12)
        .class(libcosmic::theme::Container::Dialog(true))
        .into()
}

/// The appearance indicator: a clickable icon showing which colour scheme this
/// site is told to use, with a popover for choosing it per site.
fn appearance_indicator<'a>(
    theme_override: Option<ThemeChoice>,
    global: ThemeChoice,
    open: Option<UrlBarPopover>,
) -> Element<'a, Message> {
    let effective = theme_override.unwrap_or(global);
    let tip = if theme_override.is_some() {
        format!("Appearance: {} (site override)", effective.label())
    } else {
        format!("Appearance: {}", effective.label())
    };

    let icon_name = match effective {
        ThemeChoice::System => "display-brightness-symbolic",
        ThemeChoice::Light => "weather-clear-symbolic",
        ThemeChoice::Dark => "weather-clear-night-symbolic",
    };
    let button = widget::button::icon(widget::icon::from_name(icon_name))
        .on_press(Message::ToggleUrlBarPopover(UrlBarPopover::Appearance));
    let anchor = widget::tooltip(button, widget::text(tip), widget::tooltip::Position::Bottom);

    let mut popover = widget::popover(anchor)
        .position(widget::popover::Position::Bottom)
        .on_close(Message::CloseUrlBarPopover);
    if open == Some(UrlBarPopover::Appearance) {
        popover = popover.popup(appearance_popup(theme_override, global));
    }
    popover.into()
}

/// Dropdown choices for the per-site theme: index 0 = use the global
/// preference, indices 1–3 map to the [`ThemeChoice`] variants.
const THEME_OPTIONS: &[Option<ThemeChoice>] = &[
    None,
    Some(ThemeChoice::System),
    Some(ThemeChoice::Light),
    Some(ThemeChoice::Dark),
];

/// Popover body: dropdown to pick what `prefers-color-scheme` reports on this
/// site. Choosing "Default" removes the override and follows the global
/// preference again.
fn appearance_popup<'a>(
    theme_override: Option<ThemeChoice>,
    global: ThemeChoice,
) -> Element<'a, Message> {
    let labels: Vec<String> = THEME_OPTIONS
        .iter()
        .map(|&opt| match opt {
            None => format!("Default ({})", global.label()),
            Some(choice) => choice.label().to_owned(),
        })
        .collect();
    let selected = THEME_OPTIONS.iter().position(|&o| o == theme_override);

    let dropdown = widget::dropdown(labels, selected, |i| {
        Message::SetSiteThemeOverride(THEME_OPTIONS.get(i).copied().flatten())
    });

    let column = widget::column::with_capacity(3)
        .spacing(10)
        .width(Length::Fixed(280.0))
        .push(widget::text::body("Appearance for this site"))
        .push(dropdown)
        .push(widget::text::caption(
            "Chooses what pages see in prefers-color-scheme. Sites that ignore \
             it keep their own colors.",
        ));

    widget::container(column)
        .padding(12)
        .class(libcosmic::theme::Container::Dialog(true))
        .into()
}

/// Human-readable name for a permission feature.
fn permission_label(feature: PermissionFeature) -> &'static str {
    astrolabe_webview::permission::feature_label(feature)
}

/// The permissions indicator: a key icon shown only when the site has a stored
/// or session permission decision. Clicking it lists them with a clear action.
fn permission_indicator<'a>(
    permissions: Vec<PermissionEntry>,
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

/// Popover body listing each permission decision with its state and a clear
/// button. Clearing a persistent block here lifts it, like revoking a grant.
fn permission_popup<'a>(permissions: Vec<PermissionEntry>) -> Element<'a, Message> {
    let mut column = widget::column::with_capacity(permissions.len() + 1)
        .spacing(8)
        .width(Length::Fixed(280.0))
        .push(widget::text::body("Permissions for this site"));

    for entry in permissions {
        let state = match (entry.allowed, entry.persistent) {
            (true, true) => "Allowed",
            (true, false) => "Allowed this session",
            (false, true) => "Blocked",
            (false, false) => "Blocked this session",
        };
        let label = widget::column::with_capacity(2)
            .width(Length::Fill)
            .push(widget::text(entry.label))
            .push(widget::text::caption(state));
        let clear = widget::button::text("Clear").on_press(Message::PermissionClear {
            key: entry.key,
            persistent: entry.persistent,
        });
        let row = widget::row::with_capacity(2)
            .align_y(Alignment::Center)
            .push(label)
            .push(clear);
        column = column.push(row);
    }

    widget::container(column)
        .padding(12)
        .class(libcosmic::theme::Container::Dialog(true))
        .into()
}

/// Inline banner that asks the user how to answer a permission request, with
/// session and persistent allow/block choices. Shown below the toolbar while a
/// request is pending.
pub fn permission_prompt<'a>(feature: PermissionFeature) -> Element<'a, Message> {
    let row = widget::row::with_capacity(5)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(
            widget::text(format!(
                "This site is requesting access to: {}",
                permission_label(feature)
            ))
            .width(Length::Fill),
        )
        .push(
            widget::button::standard("Block")
                .on_press(Message::PermissionRespond(PermissionResponse::Block)),
        )
        .push(
            widget::button::text("Deny once")
                .on_press(Message::PermissionRespond(PermissionResponse::DenySession)),
        )
        .push(
            widget::button::text("Allow once")
                .on_press(Message::PermissionRespond(PermissionResponse::AllowSession)),
        )
        .push(
            widget::button::suggested("Allow always")
                .on_press(Message::PermissionRespond(PermissionResponse::AllowAlways)),
        );

    widget::container(row)
        .padding(8)
        .width(Length::Fill)
        .class(libcosmic::theme::Container::Primary)
        .into()
}

/// A prompt shown when a remote developer-tools client wants to connect. The
/// devtools server is bound to loopback, so the connection is always local;
/// the prompt still lets the user decline an unexpected one.
pub fn devtools_prompt<'a>() -> Element<'a, Message> {
    let row = widget::row::with_capacity(3)
        .align_y(Alignment::Center)
        .spacing(8)
        .push(
            widget::text("A developer tools client wants to connect (local connection).")
                .width(Length::Fill),
        )
        .push(widget::button::standard("Deny").on_press(Message::DevtoolsRespond(false)))
        .push(widget::button::suggested("Allow").on_press(Message::DevtoolsRespond(true)));

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
        .class(libcosmic::theme::Container::Dialog(true))
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
            .push(widget::text::caption(s.url.as_str()).width(Length::Fill))
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
