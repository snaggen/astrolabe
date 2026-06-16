use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;

use crate::app::Message;
use crate::config::{
    BrowserConfig, ClearRange, CookieProtectionLevel, PermissionDefault, PermissionDefaults,
    PermissionKind, StartupPage,
};

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
];

/// State for the "Clear browsing data" settings section.
pub struct DataClearing<'a> {
    /// Selected time range (applies to history only).
    pub range: ClearRange,
    /// Whether browsing history is included in the clear action.
    pub history_checked: bool,
    /// Whether cookies and site data are included in the clear action.
    pub cookies_checked: bool,
    /// Domains that currently hold cookies or web storage, for per-site clearing.
    pub site_domains: &'a [String],
}

/// State for the "Site permissions" settings section.
pub struct Permissions<'a> {
    /// Global default response per configurable permission category.
    pub defaults: &'a PermissionDefaults,
    /// Stored per-site grants as `(domain, feature_key, allow)`, sorted.
    pub grants: &'a [(String, String, bool)],
    /// Whether sites may use the camera and microphone.
    pub allow_camera_microphone: bool,
}

/// Settings drawer content: New Tab grid size, privacy settings, and filter lists.
pub fn view<'a>(
    config: &'a BrowserConfig,
    filter_list_url: &'a str,
    clearing: DataClearing<'a>,
    permissions: Permissions<'a>,
) -> Element<'a, Message> {
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
    let cookie_description = match config.cookie_protection {
        CookieProtectionLevel::Standard => {
            "Blocks common trackers and cross-site cookies using EasyPrivacy \
             and your custom filter lists. Recommended for most sites."
        }
        CookieProtectionLevel::Strict => {
            "Blocks more aggressively using EasyPrivacy, uBlock Origin filters, \
             and your custom filter lists. May break some sites."
        }
    };

    // ── Custom filter lists ────────────────────────────────────────────────────
    let mut filter_section = widget::column::with_capacity(8)
        .push(widget::text::title4("Custom filter lists"))
        .spacing(8);

    for (i, entry) in config.custom_filter_lists.iter().enumerate() {
        let toggle =
            widget::toggler(entry.enabled).on_toggle(move |_| Message::ToggleCustomFilterList(i));
        let label = widget::text(entry.label.as_str()).width(Length::Fill);
        let remove = widget::button::icon(widget::icon::from_name("list-remove-symbolic"))
            .on_press(Message::RemoveCustomFilterList(i));
        let row = widget::row::with_capacity(3)
            .push(toggle)
            .push(label)
            .push(remove)
            .align_y(Alignment::Center)
            .spacing(8);
        filter_section = filter_section.push(row);
    }

    let can_add = !filter_list_url.trim().is_empty();
    let add_row = widget::row::with_capacity(2)
        .push(
            widget::text_input("https://example.com/list.txt", filter_list_url)
                .on_input(Message::CustomFilterListUrlChanged)
                .on_submit(|_| Message::AddCustomFilterList(filter_list_url.to_owned()))
                .width(Length::Fill),
        )
        .push(widget::button::text("Add").on_press_maybe(
            can_add.then(|| Message::AddCustomFilterList(filter_list_url.to_owned())),
        ))
        .align_y(Alignment::Center)
        .spacing(8);
    filter_section = filter_section.push(add_row);

    column
        .push(widget::text::title4("Privacy"))
        .push(cookie_row)
        .push(widget::text::caption(cookie_description))
        .push(filter_section)
        .push(data_clearing_section(clearing))
        .push(permissions_section(permissions))
        .push(fonts_section(config))
        .into()
}

/// Selectable web-content font sizes, in pixels.
const FONT_SIZES: &[u16] = &[12, 13, 14, 15, 16, 18, 20, 24];

/// A labeled text input for a web-content font family. Empty means "default".
fn font_family_row<'a>(
    label: &'static str,
    value: &'a str,
    on_input: fn(String) -> Message,
) -> Element<'a, Message> {
    widget::row::with_capacity(2)
        .push(widget::text(label).width(Length::Fill))
        .push(
            widget::text_input("Default", value)
                .on_input(on_input)
                // Apply on Enter; per-keystroke reloads would be disruptive.
                .on_submit(|_| Message::ApplyWebFonts)
                .width(Length::Fixed(180.0)),
        )
        .align_y(Alignment::Center)
        .into()
}

/// A labeled dropdown for a web-content font size.
fn font_size_row<'a>(
    label: &'static str,
    value: u16,
    on_change: fn(u16) -> Message,
) -> Element<'a, Message> {
    let labels: Vec<String> = FONT_SIZES.iter().map(u16::to_string).collect();
    let selected = FONT_SIZES.iter().position(|&s| s == value);
    widget::row::with_capacity(2)
        .push(widget::text(label).width(Length::Fill))
        .push(widget::dropdown(labels, selected, move |i| {
            on_change(FONT_SIZES.get(i).copied().unwrap_or(value))
        }))
        .align_y(Alignment::Center)
        .into()
}

/// Build the "Fonts" section: default serif/sans-serif/monospace families and
/// base sizes for web content (like a browser's font settings).
fn fonts_section(config: &BrowserConfig) -> Element<'_, Message> {
    let spacing = libcosmic::theme::spacing();
    widget::column::with_capacity(7)
        .push(widget::text::title4("Fonts"))
        .push(widget::text::caption(
            "Default fonts for web pages. Leave a family blank to use the engine default.",
        ))
        .push(font_family_row(
            "Serif",
            &config.web_font_serif,
            Message::SetWebFontSerif,
        ))
        .push(font_family_row(
            "Sans-serif",
            &config.web_font_sans_serif,
            Message::SetWebFontSansSerif,
        ))
        .push(font_family_row(
            "Monospace",
            &config.web_font_monospace,
            Message::SetWebFontMonospace,
        ))
        .push(font_size_row(
            "Size",
            config.web_font_size,
            Message::SetWebFontSize,
        ))
        .push(font_size_row(
            "Monospace size",
            config.web_font_monospace_size,
            Message::SetWebFontMonospaceSize,
        ))
        .spacing(spacing.space_xxs)
        .into()
}

/// Build the "Clear browsing data" section: a time-range dropdown, category
/// toggles, a clear button, and a per-site list.
fn data_clearing_section(clearing: DataClearing<'_>) -> Element<'_, Message> {
    let spacing = libcosmic::theme::spacing();
    let range_selected = ClearRange::ALL.iter().position(|&r| r == clearing.range);
    let range_labels: Vec<&str> = ClearRange::ALL.iter().map(ClearRange::label).collect();
    let range_row = widget::row::with_capacity(2)
        .push(widget::text("Time range").width(Length::Fill))
        .push(widget::dropdown(range_labels, range_selected, |i| {
            Message::SetClearRange(ClearRange::ALL.get(i).copied().unwrap_or_default())
        }))
        .align_y(Alignment::Center);

    let history_toggle = widget::row::with_capacity(2)
        .push(widget::text("Browsing history").width(Length::Fill))
        .push(widget::toggler(clearing.history_checked).on_toggle(Message::ToggleClearHistory))
        .align_y(Alignment::Center);
    let cookies_toggle = widget::row::with_capacity(2)
        .push(widget::text("Cookies and site data").width(Length::Fill))
        .push(widget::toggler(clearing.cookies_checked).on_toggle(Message::ToggleClearCookies))
        .align_y(Alignment::Center);

    // Cookies and site data have no time filter in the engine; only history obeys
    // the selected range. Make that explicit so the control is not misleading.
    let range_note = widget::text::caption(
        "The time range applies to history only; cookies are cleared in full.",
    );

    let can_clear = clearing.history_checked || clearing.cookies_checked;
    // Clearing is irreversible, so it is a destructive action and is confirmed via
    // a dialog before anything is deleted (see `App::dialog`).
    let clear_button = widget::button::destructive("Clear now")
        .on_press_maybe(can_clear.then_some(Message::RequestClearData));

    let mut section = widget::column::with_capacity(7)
        .push(widget::text::title4("Clear browsing data"))
        .push(range_row)
        .push(history_toggle)
        .push(cookies_toggle)
        .push(range_note)
        .push(clear_button)
        .spacing(spacing.space_xxs);

    if !clearing.site_domains.is_empty() {
        section = section
            .push(widget::text::heading("Sites with stored data"))
            .push(widget::text::caption(
                "Remove cookies, site storage, and history for a single site.",
            ));
        for domain in clearing.site_domains {
            let remove = widget::tooltip(
                widget::button::icon(widget::icon::from_name("user-trash-symbolic"))
                    .on_press(Message::ClearSiteData(domain.clone())),
                widget::text::body("Clear data for this site"),
                widget::tooltip::Position::Left,
            );
            let row = widget::row::with_capacity(2)
                .push(widget::text(domain.as_str()).width(Length::Fill))
                .push(remove)
                .align_y(Alignment::Center)
                .spacing(spacing.space_xxs);
            section = section.push(row);
        }
    }

    section.into()
}

/// Build the "Site permissions" section: a global default per category and a
/// list of stored per-site exceptions that can be revoked.
fn permissions_section(permissions: Permissions<'_>) -> Element<'_, Message> {
    let spacing = libcosmic::theme::spacing();
    let default_labels: Vec<&str> = PermissionDefault::ALL
        .iter()
        .map(PermissionDefault::label)
        .collect();

    // Camera & microphone are a single engine-level switch: Servo exposes
    // getUserMedia without a per-site prompt, so this is an explicit opt-in.
    let camera_mic = widget::row::with_capacity(2)
        .push(widget::text("Allow sites to use camera and microphone").width(Length::Fill))
        .push(
            widget::toggler(permissions.allow_camera_microphone)
                .on_toggle(Message::SetAllowCameraMicrophone),
        )
        .align_y(Alignment::Center);

    let mut section = widget::column::with_capacity(8)
        .push(widget::text::title4("Site permissions"))
        .push(widget::text::caption(
            "Default response when a site asks for a permission.",
        ))
        .push(camera_mic)
        .push(widget::text::caption(
            "Camera and microphone are granted without a prompt; takes effect on the next page load.",
        ))
        .spacing(spacing.space_xxs);

    for kind in PermissionKind::ALL {
        let current = permissions.defaults.get(kind);
        let selected = PermissionDefault::ALL.iter().position(|&d| d == current);
        let row = widget::row::with_capacity(2)
            .push(widget::text(kind.label()).width(Length::Fill))
            .push(widget::dropdown(
                default_labels.clone(),
                selected,
                move |i| {
                    let value = PermissionDefault::ALL.get(i).copied().unwrap_or_default();
                    Message::SetPermissionDefault(kind, value)
                },
            ))
            .align_y(Alignment::Center);
        section = section.push(row);
    }

    if !permissions.grants.is_empty() {
        section = section.push(widget::text::heading("Site exceptions")).push(
            widget::button::standard("Reset all site permissions")
                .on_press(Message::ResetAllSitePermissions),
        );
        for (domain, key, allow) in permissions.grants {
            let label = cosmic_webview::permission::feature_label_from_key(key).unwrap_or(key);
            let decision = if *allow { "Allow" } else { "Block" };
            let revoke = widget::tooltip(
                widget::button::icon(widget::icon::from_name("user-trash-symbolic"))
                    .on_press(Message::RevokeSitePermission(domain.clone(), key.clone())),
                widget::text::body("Revoke this permission"),
                widget::tooltip::Position::Left,
            );
            let row = widget::row::with_capacity(3)
                .push(widget::text(format!("{domain} — {label}")).width(Length::Fill))
                .push(widget::text::caption(decision))
                .push(revoke)
                .align_y(Alignment::Center)
                .spacing(spacing.space_xxs);
            section = section.push(row);
        }
    }

    section.into()
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
