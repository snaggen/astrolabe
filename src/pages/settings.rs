use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::config::{BrowserConfig, StartupPage};

/// Selectable column counts for the New Tab grid.
const COLUMN_CHOICES: &[u16] = &[3, 4, 5, 6, 7, 8];
/// Selectable row counts for the New Tab grid.
const ROW_CHOICES: &[u16] = &[1, 2, 3, 4, 5, 6];
/// New-tab-page kinds, in dropdown order (index maps to a `StartupPage` variant).
const NEW_TAB_KINDS: &[&str] = &["New Tab page", "Fixed URL", "Search engine"];

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

    column.into()
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
