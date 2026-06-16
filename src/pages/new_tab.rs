use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use libcosmic::iced::alignment::{Horizontal, Vertical};
use libcosmic::iced::widget::Stack;
use libcosmic::iced::{Alignment, ContentFit, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::db::{BookmarkEntry, HistoryEntry};
use crate::thumbnails::TileImage;

/// Fixed width of a single grid cell, in logical pixels.
const CELL_WIDTH: f32 = 140.0;
/// Height of a cell's thumbnail image, in logical pixels.
const THUMB_HEIGHT: f32 = 90.0;
/// Fixed height of a cell's label region, in logical pixels (room for ~2 lines).
const LABEL_HEIGHT: f32 = 34.0;
/// Size of the favicon overlaid on a thumbnail, in logical pixels.
const FAVICON_OVERLAY: f32 = 36.0;
/// Size of a favicon shown as the whole tile (no thumbnail), in logical pixels.
const FAVICON_TILE: f32 = 48.0;

/// New Tab page: a search box that focuses the URL bar and a grid of top sites.
///
/// Pinned sites are shown first, then the most-visited history entries fill the
/// remaining `columns * rows` cells. Cells already pinned are not repeated.
/// `images` maps a URL to its cached tile images (thumbnail and/or favicon).
/// `hovered` is the URL of the cell under the pointer, which reveals its pin button.
pub fn view<'a>(
    top_sites: &'a [HistoryEntry],
    pinned: &'a [BookmarkEntry],
    images: &'a HashMap<String, TileImage>,
    columns: u16,
    rows: u16,
    hovered: Option<&'a str>,
) -> Element<'a, Message> {
    // A gear button (top-left) opens the New Tab settings drawer.
    let settings_btn = widget::tooltip(
        widget::button::icon(widget::icon::from_name("emblem-system-symbolic"))
            .on_press(Message::OpenNewTabSettings),
        widget::text::body("New Tab settings"),
        widget::tooltip::Position::Right,
    );
    let top_row = widget::row::with_capacity(2)
        .push(settings_btn)
        .push(widget::space::horizontal())
        .width(Length::Fill);

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

    let grid = grid_view(top_sites, pinned, images, columns, rows, hovered);

    widget::column::with_capacity(3)
        .push(top_row)
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
    images: &'a HashMap<String, TileImage>,
    columns: u16,
    rows: u16,
    hovered: Option<&'a str>,
) -> Element<'a, Message> {
    // Same rounding the theme uses for buttons, so tiles match the rest of the UI.
    let radius = libcosmic::theme::active().cosmic().corner_radii.radius_s;
    let gap = libcosmic::theme::spacing().space_s;

    let capacity = columns as usize * rows as usize;
    let pinned_urls: HashSet<&str> = pinned.iter().map(|b| b.url.as_str()).collect();
    let tile_of = |url: &str| images.get(url);
    let is_hovered = |url: &str| hovered == Some(url);

    let mut cells: Vec<Element<'a, Message>> = Vec::with_capacity(capacity);
    for b in pinned.iter().take(capacity) {
        cells.push(site_cell(
            &b.url,
            &b.title,
            true,
            tile_of(&b.url),
            radius,
            is_hovered(&b.url),
        ));
    }
    for h in top_sites {
        if cells.len() >= capacity {
            break;
        }
        if !pinned_urls.contains(h.url.as_str()) {
            cells.push(site_cell(
                &h.url,
                &h.title,
                false,
                tile_of(&h.url),
                radius,
                is_hovered(&h.url),
            ));
        }
    }

    // Chunk the flat cell list into rows of `columns` each.
    let mut grid = widget::column::with_capacity(rows as usize).spacing(gap);
    let mut row = widget::row::with_capacity(columns as usize).spacing(gap);
    let mut in_row = 0u16;
    for cell in cells {
        row = row.push(cell);
        in_row += 1;
        if in_row == columns {
            grid = grid.push(row);
            row = widget::row::with_capacity(columns as usize).spacing(gap);
            in_row = 0;
        }
    }
    if in_row > 0 {
        grid = grid.push(row);
    }
    grid.into()
}

/// A single grid cell, laid out as a fixed-size card: a tile image (thumbnail,
/// favicon, or generic icon) with a favicon overlay (top-right) and a pin/unpin
/// button (top-left, revealed on hover), and a fixed-height title region below.
fn site_cell<'a>(
    url: &str,
    title: &str,
    pinned: bool,
    tile: Option<&TileImage>,
    radius: [f32; 4],
    hovered: bool,
) -> Element<'a, Message> {
    let label = if title.trim().is_empty() { url } else { title }.to_string();
    let spacing = libcosmic::theme::spacing();

    let thumbnail = tile.and_then(|t| t.thumbnail.as_deref());
    let favicon = tile.and_then(|t| t.favicon.as_deref());

    // Layer the overlays over the base tile image.
    let mut tile_stack = Stack::new().push(tile_base(thumbnail, favicon, radius));
    // The favicon overlay only makes sense over a thumbnail; otherwise the
    // favicon is already the tile itself.
    if let (Some(_), Some(path)) = (thumbnail, favicon) {
        tile_stack = tile_stack.push(favicon_overlay(path));
    }
    if hovered {
        tile_stack = tile_stack.push(pin_overlay(url, &label, pinned));
    }

    let label_box = widget::container(widget::text::caption(label))
        .width(Length::Fixed(CELL_WIDTH))
        .height(Length::Fixed(LABEL_HEIGHT))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Top)
        .clip(true);

    let body = widget::column::with_capacity(2)
        .push(tile_stack)
        .push(label_box)
        .spacing(spacing.space_xxs)
        .align_x(Alignment::Center)
        .width(Length::Fixed(CELL_WIDTH));

    let card = widget::container(body)
        .padding(spacing.space_xs)
        .class(libcosmic::theme::Container::Card);

    // Hover/click cover the whole card. Clicking the pin button captures the
    // press, so navigation only fires when the rest of the card is clicked.
    widget::mouse_area(card)
        .on_press(Message::NavigateTo(url.to_string()))
        .on_enter(Message::TileHovered(url.to_string()))
        .on_exit(Message::TileUnhovered)
        .into()
}

/// Build the fixed-size base tile image: a rounded thumbnail, a centered favicon,
/// or a generic site icon when neither is cached.
fn tile_base<'a>(
    thumbnail: Option<&Path>,
    favicon: Option<&Path>,
    radius: [f32; 4],
) -> Element<'a, Message> {
    match (thumbnail, favicon) {
        (Some(thumb), _) => widget::image(widget::image::Handle::from_path(thumb))
            .width(Length::Fixed(CELL_WIDTH))
            .height(Length::Fixed(THUMB_HEIGHT))
            .content_fit(ContentFit::Cover)
            .border_radius(radius)
            .into(),
        (None, Some(path)) => tile_box(
            widget::image(widget::image::Handle::from_path(path))
                .width(Length::Fixed(FAVICON_TILE))
                .height(Length::Fixed(FAVICON_TILE))
                .into(),
        ),
        (None, None) => tile_box(
            widget::icon::from_name("applications-internet-symbolic")
                .icon()
                .size(FAVICON_TILE as u16)
                .into(),
        ),
    }
}

/// Wrap `content` centered in a fixed-size tile box with a subtle background.
fn tile_box<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    widget::container(content)
        .width(Length::Fixed(CELL_WIDTH))
        .height(Length::Fixed(THUMB_HEIGHT))
        .align_x(Horizontal::Center)
        .align_y(Vertical::Center)
        .class(libcosmic::theme::Container::Secondary)
        .into()
}

/// A favicon pinned to the top-right corner of the tile, to identify the site.
fn favicon_overlay<'a>(path: &Path) -> Element<'a, Message> {
    let radius = libcosmic::theme::active().cosmic().corner_radii.radius_xs;
    let icon = widget::image(widget::image::Handle::from_path(path))
        .width(Length::Fixed(FAVICON_OVERLAY))
        .height(Length::Fixed(FAVICON_OVERLAY))
        .border_radius(radius);
    widget::container(icon)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Right)
        .align_y(Vertical::Top)
        .padding(libcosmic::theme::spacing().space_xxs)
        .into()
}

/// A dark, semi-opaque scrim button style for the unpinned pin button, keeping
/// the icon legible over any thumbnail. The background is a fixed dark overlay
/// (not a theme token) because it sits on top of arbitrary page screenshots; the
/// rounding still follows the theme.
fn pin_scrim_style() -> libcosmic::theme::Button {
    use libcosmic::iced::{Background, Color, Vector};
    use libcosmic::widget::button::Style;

    fn style(alpha: f32, theme: &libcosmic::Theme) -> Style {
        Style {
            shadow_offset: Vector::new(0.0, 0.0),
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, alpha))),
            overlay: None,
            border_radius: theme.cosmic().corner_radii.radius_s.into(),
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
            outline_width: 0.0,
            outline_color: Color::TRANSPARENT,
            icon_color: Some(Color::from_rgb(0.93, 0.93, 0.93)),
            text_color: Some(Color::from_rgb(0.93, 0.93, 0.93)),
        }
    }
    libcosmic::theme::Button::Custom {
        active: Box::new(|_focused, theme| style(0.65, theme)),
        hovered: Box::new(|_focused, theme| style(0.82, theme)),
        pressed: Box::new(|_focused, theme| style(0.90, theme)),
        disabled: Box::new(|theme| style(0.50, theme)),
    }
}

/// A pin/unpin button in the top-left corner of the tile, revealed on hover.
///
/// A filled style gives the icon a solid background so it stays legible over any
/// thumbnail: a dark scrim when unpinned, the accent (suggested) style when the
/// site is already pinned.
fn pin_overlay<'a>(url: &str, label: &str, pinned: bool) -> Element<'a, Message> {
    let style = if pinned {
        libcosmic::theme::Button::Suggested
    } else {
        pin_scrim_style()
    };
    let tooltip = if pinned { "Unpin site" } else { "Pin site" };
    let button = widget::tooltip(
        widget::button::icon(widget::icon::from_name("pin-symbolic"))
            .class(style)
            .on_press(Message::TogglePinnedSite {
                url: url.to_string(),
                title: label.to_string(),
            }),
        widget::text::body(tooltip),
        widget::tooltip::Position::Right,
    );
    widget::container(button)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Horizontal::Left)
        .align_y(Vertical::Top)
        .padding(libcosmic::theme::spacing().space_xxs)
        .into()
}
