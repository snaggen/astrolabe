use libcosmic::iced::alignment::Horizontal;
use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::db::BookmarkEntry;

/// Bookmarks drawer content: filter input and entry list.
pub fn view<'a>(entries: &'a [BookmarkEntry], filter: &'a str) -> Element<'a, Message> {
    let spacing = libcosmic::theme::spacing();
    let filter_input = widget::text_input("Search bookmarks…", filter)
        .on_input(Message::BookmarkFilterChanged)
        .width(Length::Fill);

    let content: Element<'a, Message> = if entries.is_empty() {
        let message = if filter.trim().is_empty() {
            "No bookmarks yet"
        } else {
            "No matching bookmarks"
        };
        empty_state(message)
    } else {
        entries
            .iter()
            .fold(widget::list_column(), |list, entry| {
                list.add(entry_row(entry))
            })
            .into()
    };

    widget::column::with_capacity(2)
        .push(filter_input)
        .push(content)
        .spacing(spacing.space_xxs)
        .width(Length::Fill)
        .into()
}

/// A centered placeholder shown when the list is empty.
fn empty_state(message: &str) -> Element<'_, Message> {
    widget::container(widget::text::body(message))
        .width(Length::Fill)
        .align_x(Horizontal::Center)
        .padding(libcosmic::theme::spacing().space_l)
        .into()
}

fn entry_row(entry: &BookmarkEntry) -> Element<'_, Message> {
    let spacing = libcosmic::theme::spacing();
    let title: &str = if entry.title.is_empty() {
        &entry.url
    } else {
        &entry.title
    };

    let info = widget::column::with_capacity(2)
        .push(widget::text(title).width(Length::Fill))
        .push(widget::text::caption(entry.url.as_str()).width(Length::Fill))
        .width(Length::Fill);

    let del = widget::tooltip(
        widget::button::icon(widget::icon::from_name("edit-delete-symbolic"))
            .on_press(Message::BookmarkDeleteEntry(entry.id)),
        widget::text::body("Remove bookmark"),
        widget::tooltip::Position::Left,
    );

    widget::row::with_capacity(2)
        .push(widget::mouse_area(info).on_press(Message::BookmarkNavigate(entry.url.clone())))
        .push(del)
        .align_y(Alignment::Center)
        .spacing(spacing.space_xs)
        .into()
}
