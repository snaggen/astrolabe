use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::db::BookmarkEntry;

/// Bookmarks drawer content: filter input and entry list.
pub fn view<'a>(entries: &'a [BookmarkEntry], filter: &'a str) -> Element<'a, Message> {
    let filter_input = widget::text_input("Search bookmarks…", filter)
        .on_input(Message::BookmarkFilterChanged)
        .width(Length::Fill);

    let entry_list = entries.iter().fold(widget::list_column(), |list, entry| {
        list.add(entry_row(entry))
    });

    widget::column::with_capacity(2)
        .push(filter_input)
        .push(entry_list)
        .spacing(8)
        .width(Length::Fill)
        .into()
}

fn entry_row<'a>(entry: &'a BookmarkEntry) -> Element<'a, Message> {
    let title: &str = if entry.title.is_empty() {
        &entry.url
    } else {
        &entry.title
    };

    let info = widget::column::with_capacity(2)
        .push(widget::text(title).width(Length::Fill))
        .push(widget::text(&entry.url).size(12).width(Length::Fill))
        .width(Length::Fill);

    let del = widget::button::icon(widget::icon::from_name("edit-delete-symbolic"))
        .on_press(Message::BookmarkDeleteEntry(entry.id));

    widget::row::with_capacity(2)
        .push(widget::mouse_area(info).on_press(Message::BookmarkNavigate(entry.url.clone())))
        .push(del)
        .align_y(Alignment::Center)
        .into()
}
