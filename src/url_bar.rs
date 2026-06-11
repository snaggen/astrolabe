use std::sync::LazyLock;

use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::config::{BUILTIN_ENGINES, SearchEngine};
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
    /// Whether the active page is bookmarked (controls the star icon).
    pub is_bookmarked: bool,
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

        let mut row = widget::row::with_capacity(3).align_y(Alignment::Center);
        if let Some(indicator) = security_indicator(self.security, self.open_popover) {
            row = row.push(indicator);
        }
        row = row.push(input).push(bookmark_button(self.is_bookmarked));

        widget::container(row)
            .padding([0, 4])
            .width(Length::Fill)
            .class(libcosmic::theme::Container::Card)
            .into()
    }
}

/// The security indicator: a clickable icon with a hover tooltip and a popover
/// describing the connection state. Returns `None` for internal pages where no
/// security state applies.
fn security_indicator<'a>(
    security: SecurityStatus,
    open: Option<UrlBarPopover>,
) -> Option<Element<'a, Message>> {
    let (icon_name, tip) = match security {
        SecurityStatus::Secure => ("channel-secure-symbolic", "Anslutningen är krypterad"),
        SecurityStatus::Insecure => (
            "channel-insecure-symbolic",
            "Anslutningen är inte krypterad",
        ),
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
        SecurityStatus::Secure => "Anslutningen till den här sidan är krypterad.",
        SecurityStatus::Insecure => {
            "Anslutningen till den här sidan är inte krypterad. \
             Information du skickar kan vara synlig för andra."
        }
        SecurityStatus::Unknown => "Ingen säkerhetsinformation är tillgänglig för den här sidan.",
    };
    widget::container(widget::text(body))
        .padding(12)
        .width(Length::Fixed(280.0))
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
