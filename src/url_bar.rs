use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::config::SearchEngine;
use crate::db::{Suggestion, SuggestionKind};

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
const ENGINES: &[SearchEngine] = &[
    SearchEngine::DuckDuckGo,
    SearchEngine::Kagi,
    SearchEngine::Brave,
    SearchEngine::Ecosia,
    SearchEngine::Startpage,
    SearchEngine::Qwant,
];

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
    };

    let title: &str = if s.title.is_empty() { &s.url } else { &s.title };

    let info = widget::column::with_capacity(2)
        .push(widget::text(title).width(Length::Fill))
        .push(widget::text(&s.url).size(12).width(Length::Fill))
        .width(Length::Fill);

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
