use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::config::{BrowserConfig, CookieProtectionLevel, StartupPage};

/// Selectable column counts for the New Tab grid.
const COLUMN_CHOICES: &[u16] = &[3, 4, 5, 6, 7, 8];
/// Selectable row counts for the New Tab grid.
const ROW_CHOICES: &[u16] = &[1, 2, 3, 4, 5, 6];
/// New-tab-page kinds, in dropdown order (index maps to a `StartupPage` variant).
const NEW_TAB_KINDS: &[&str] = &["New Tab page", "Fixed URL", "Search engine"];
/// Cookie-protection levels, in dropdown order (index maps to the enum).
const COOKIE_LEVELS: &[CookieProtectionLevel] = &[
    CookieProtectionLevel::Standard,
    CookieProtectionLevel::Strict,
    CookieProtectionLevel::Custom,
];

/// Settings drawer content: New Tab grid size and what new tabs open.
pub fn view(config: &BrowserConfig) -> Element<'_, Message> {
    let columns = number_row(
        "Grid columns",
        COLUMN_CHOICES,
        config.new_tab_columns,
        Message::SetNewTabColumns,
    );
    let rows = number_row(
        "Grid rows",
        ROW_CHOICES,
        config.new_tab_rows,
        Message::SetNewTabRows,
    );

    let kind_selected = match config.new_tab_page {
        StartupPage::NewTab => 0,
        StartupPage::Url(_) => 1,
        StartupPage::SearchEngine => 2,
    };
    let new_tab_kind = widget::row::with_capacity(2)
        .push(widget::text("New tab opens").width(Length::Fill))
        .push(widget::dropdown(
            NEW_TAB_KINDS,
            Some(kind_selected),
            Message::SetNewTabPageKind,
        ))
        .align_y(Alignment::Center);

    let mut column = widget::column::with_capacity(4)
        .push(widget::text::title4("New Tab page"))
        .push(columns)
        .push(rows)
        .push(new_tab_kind)
        .spacing(12)
        .width(Length::Fill);

    // The URL field only applies to the "Fixed URL" kind.
    if let StartupPage::Url(url) = &config.new_tab_page {
        column = column.push(
            widget::text_input("https://example.com", url)
                .on_input(Message::SetNewTabUrl)
                .width(Length::Fill),
        );
    }

    let cookie_selected = COOKIE_LEVELS
        .iter()
        .position(|&l| l == config.cookie_protection);
    let cookie_labels: Vec<&str> = COOKIE_LEVELS
        .iter()
        .map(CookieProtectionLevel::name)
        .collect();
    let cookie_row = widget::row::with_capacity(2)
        .push(widget::text("Tracking protection").width(Length::Fill))
        .push(widget::dropdown(cookie_labels, cookie_selected, |i| {
            Message::SetCookieProtection(COOKIE_LEVELS.get(i).copied().unwrap_or_default())
        }))
        .align_y(Alignment::Center);

    column
        .push(widget::text::title4("Privacy"))
        .push(cookie_row)
        .into()
}

/// A labeled dropdown of integer choices that emits `on_change(value)`.
fn number_row<'a>(
    label: &'a str,
    choices: &'static [u16],
    current: u16,
    on_change: fn(u16) -> Message,
) -> Element<'a, Message> {
    let labels: Vec<String> = choices.iter().map(u16::to_string).collect();
    let selected = choices.iter().position(|&c| c == current);
    let dropdown = widget::dropdown(labels, selected, move |i| {
        on_change(choices.get(i).copied().unwrap_or(current))
    });

    widget::row::with_capacity(2)
        .push(widget::text(label).width(Length::Fill))
        .push(dropdown)
        .align_y(Alignment::Center)
        .into()
}
