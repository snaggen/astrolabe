use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::db::{BookmarkEntry, HistoryEntry};

/// Fixed width of a single grid cell, in logical pixels.
const CELL_WIDTH: f32 = 140.0;
/// Height of a cell's thumbnail image, in logical pixels.
const THUMB_HEIGHT: f32 = 90.0;

/// New Tab page: a search box that focuses the URL bar and a grid of top sites.
///
/// Pinned sites are shown first, then the most-visited history entries fill the
/// remaining `columns * rows` cells. Cells already pinned are not repeated.
/// `images` maps a URL to its best cached tile image (thumbnail or favicon).
pub fn view<'a>(
    top_sites: &'a [HistoryEntry],
    pinned: &'a [BookmarkEntry],
    images: &'a HashMap<String, PathBuf>,
    columns: u16,
    rows: u16,
) -> Element<'a, Message> {
    let search = widget::button::custom(
        widget::row::with_capacity(2)
            .push(
                widget::icon::from_name("system-search-symbolic")
                    .icon()
                    .size(16),
            )
            .push(widget::text("Search the web…"))
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .on_press(Message::FocusUrlBar)
    .width(Length::Fixed(420.0));

    let grid = grid_view(top_sites, pinned, images, columns, rows);

    widget::column::with_capacity(2)
        .push(search)
        .push(grid)
        .spacing(24)
        .align_x(Alignment::Center)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(32)
        .into()
}

/// Build the cell grid: pinned sites first, then most-visited history entries.
fn grid_view<'a>(
    top_sites: &'a [HistoryEntry],
    pinned: &'a [BookmarkEntry],
    images: &'a HashMap<String, PathBuf>,
    columns: u16,
    rows: u16,
) -> Element<'a, Message> {
    let capacity = columns as usize * rows as usize;
    let pinned_urls: HashSet<&str> = pinned.iter().map(|b| b.url.as_str()).collect();
    let image_of = |url: &str| images.get(url).map(PathBuf::as_path);

    let mut cells: Vec<Element<'a, Message>> = Vec::with_capacity(capacity);
    for b in pinned.iter().take(capacity) {
        cells.push(site_cell(&b.url, &b.title, true, image_of(&b.url)));
    }
    for h in top_sites {
        if cells.len() >= capacity {
            break;
        }
        if !pinned_urls.contains(h.url.as_str()) {
            cells.push(site_cell(&h.url, &h.title, false, image_of(&h.url)));
        }
    }

    // Chunk the flat cell list into rows of `columns` each.
    let mut grid = widget::column::with_capacity(rows as usize).spacing(16);
    let mut row = widget::row::with_capacity(columns as usize).spacing(16);
    let mut in_row = 0u16;
    for cell in cells {
        row = row.push(cell);
        in_row += 1;
        if in_row == columns {
            grid = grid.push(row);
            row = widget::row::with_capacity(columns as usize).spacing(16);
            in_row = 0;
        }
    }
    if in_row > 0 {
        grid = grid.push(row);
    }
    grid.into()
}

/// A single grid cell: tile image (thumbnail, favicon, or generic icon), title,
/// and a pin/unpin toggle.
fn site_cell<'a>(
    url: &str,
    title: &str,
    pinned: bool,
    image: Option<&Path>,
) -> Element<'a, Message> {
    let label = if title.trim().is_empty() { url } else { title }.to_string();

    // Prefer a cached thumbnail/favicon; fall back to a generic site icon.
    let thumb: Element<'a, Message> = match image {
        Some(path) => widget::image(widget::image::Handle::from_path(path))
            .width(Length::Fixed(CELL_WIDTH))
            .height(Length::Fixed(THUMB_HEIGHT))
            .into(),
        None => widget::icon::from_name("applications-internet-symbolic")
            .icon()
            .size(48)
            .into(),
    };

    let body = widget::column::with_capacity(2)
        .push(thumb)
        .push(widget::text::caption(label.clone()))
        .spacing(6)
        .align_x(Alignment::Center)
        .width(Length::Fixed(CELL_WIDTH));
    let open = widget::mouse_area(body).on_press(Message::NavigateTo(url.to_string()));

    let pin_icon = if pinned {
        "list-remove-symbolic"
    } else {
        "list-add-symbolic"
    };
    let pin_btn = widget::button::icon(widget::icon::from_name(pin_icon)).on_press(
        Message::TogglePinnedSite {
            url: url.to_string(),
            title: label,
        },
    );

    widget::column::with_capacity(2)
        .push(open)
        .push(pin_btn)
        .spacing(4)
        .align_x(Alignment::Center)
        .into()
}
