use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::db::{HistoryEntry, HistorySort};

/// History drawer content: filter input, sort controls, and entry list.
pub fn view<'a>(
    entries: &'a [HistoryEntry],
    filter: &'a str,
    sort: HistorySort,
) -> Element<'a, Message> {
    let filter_input = widget::text_input("Search history…", filter)
        .on_input(Message::HistoryFilterChanged)
        .width(Length::Fill);

    let sort_row = widget::row::with_capacity(2)
        .push(
            widget::button::text("Recent").on_press_maybe(
                (sort != HistorySort::RecentFirst)
                    .then_some(Message::HistorySortChanged(HistorySort::RecentFirst)),
            ),
        )
        .push(
            widget::button::text("Most visited").on_press_maybe(
                (sort != HistorySort::MostVisited)
                    .then_some(Message::HistorySortChanged(HistorySort::MostVisited)),
            ),
        )
        .spacing(8);

    let entry_list = entries.iter().fold(widget::list_column(), |list, entry| {
        list.add(entry_row(entry))
    });

    widget::column::with_capacity(3)
        .push(filter_input)
        .push(sort_row)
        .push(entry_list)
        .spacing(8)
        .width(Length::Fill)
        .into()
}

fn entry_row<'a>(entry: &'a HistoryEntry) -> Element<'a, Message> {
    let title: &str = if entry.title.is_empty() {
        &entry.url
    } else {
        &entry.title
    };

    let subtitle = format!("{} — {} visits", entry.url, entry.visit_count);
    let info = widget::column::with_capacity(2)
        .push(widget::text(title).width(Length::Fill))
        .push(widget::text::caption(subtitle).width(Length::Fill))
        .width(Length::Fill);

    let del = widget::button::icon(widget::icon::from_name("edit-delete-symbolic"))
        .on_press(Message::HistoryDeleteEntry(entry.id));

    widget::row::with_capacity(2)
        .push(widget::mouse_area(info).on_press(Message::HistoryNavigate(entry.url.clone())))
        .push(del)
        .align_y(Alignment::Center)
        .into()
}
