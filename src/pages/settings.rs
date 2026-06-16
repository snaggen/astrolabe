use libcosmic::iced::{Alignment, Length};
use libcosmic::prelude::*;
use libcosmic::widget;
use libcosmic::widget::segmented_button;

use crate::app::Message;
use crate::config::{
    BUILTIN_ENGINES, BrowserConfig, ClearRange, CookieProtectionLevel, PermissionDefault,
    PermissionDefaults, PermissionKind, SearchEngine, StartupPage,
};

/// Selectable column counts for the New Tab grid.
const COLUMN_CHOICES: &[u16] = &[3, 4, 5, 6, 7, 8];
/// Selectable row counts for the New Tab grid.
const ROW_CHOICES: &[u16] = &[1, 2, 3, 4, 5, 6];
/// Page kinds, in dropdown order (index maps to a `StartupPage` variant).
const PAGE_KINDS: &[&str] = &["New Tab page", "Fixed URL", "Search engine"];
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

/// A labeled "which page to open" dropdown plus a conditional URL field, shared
/// by the startup-page and new-tab-page settings.
fn page_kind_controls<'a>(
    label: &'a str,
    page: &'a StartupPage,
    on_kind: fn(usize) -> Message,
    on_url: fn(String) -> Message,
) -> Element<'a, Message> {
    let selected = match page {
        StartupPage::NewTab => 0,
        StartupPage::Url(_) => 1,
        StartupPage::SearchEngine => 2,
    };
    let kind_row = widget::row::with_capacity(2)
        .push(widget::text(label).width(Length::Fill))
        .push(widget::dropdown(PAGE_KINDS, Some(selected), on_kind))
        .align_y(Alignment::Center);

    let mut col = widget::column::with_capacity(2)
        .push(kind_row)
        .spacing(libcosmic::theme::spacing().space_s);
    // The URL field only applies to the "Fixed URL" kind.
    if let StartupPage::Url(url) = page {
        col = col.push(
            widget::text_input("https://example.com", url)
                .on_input(on_url)
                .width(Length::Fill),
        );
    }
    col.into()
}

/// New Tab settings drawer content: grid size and what new tabs open.
///
/// Kept separate from the main settings page and reached from a gear button on
/// the New Tab page, since it only concerns that page.
pub fn new_tab_settings(config: &BrowserConfig) -> Element<'_, Message> {
    widget::column::with_capacity(3)
        .push(number_row(
            "Grid columns",
            COLUMN_CHOICES,
            config.new_tab_columns,
            Message::SetNewTabColumns,
        ))
        .push(number_row(
            "Grid rows",
            ROW_CHOICES,
            config.new_tab_rows,
            Message::SetNewTabRows,
        ))
        .push(page_kind_controls(
            "New tab opens",
            &config.new_tab_page,
            Message::SetNewTabPageKind,
            Message::SetNewTabUrl,
        ))
        .spacing(libcosmic::theme::spacing().space_s)
        .width(Length::Fill)
        .into()
}

/// A section of the settings page, shown one at a time via the side navigation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SettingsSection {
    /// Startup page and search engine.
    #[default]
    General,
    /// Tracking protection and custom filter lists.
    Privacy,
    /// Per-site permission defaults and exceptions.
    Permissions,
    /// Default web-content fonts.
    Fonts,
    /// Clear browsing data.
    ClearData,
}

impl SettingsSection {
    /// All sections, in navigation order.
    pub const ALL: &'static [SettingsSection] = &[
        SettingsSection::General,
        SettingsSection::Privacy,
        SettingsSection::Permissions,
        SettingsSection::Fonts,
        SettingsSection::ClearData,
    ];

    /// The label shown in the side navigation.
    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::General => "General",
            SettingsSection::Privacy => "Privacy",
            SettingsSection::Permissions => "Permissions",
            SettingsSection::Fonts => "Fonts",
            SettingsSection::ClearData => "Clear data",
        }
    }
}

/// Settings page: a side navigation of sections and the selected section's
/// content, laid out like cosmic-settings.
pub fn view<'a>(
    nav: &'a segmented_button::SingleSelectModel,
    config: &'a BrowserConfig,
    filter_list_url: &'a str,
    clearing: DataClearing<'a>,
    permissions: Permissions<'a>,
) -> Element<'a, Message> {
    let spacing = libcosmic::theme::spacing();
    let section = nav
        .active_data::<SettingsSection>()
        .copied()
        .unwrap_or_default();

    let nav_column = segmented_button::vertical(nav)
        .on_activate(Message::SettingsSectionSelected)
        .width(Length::Fixed(200.0));

    let content = match section {
        SettingsSection::General => general_section(config),
        SettingsSection::Privacy => privacy_section(config, filter_list_url),
        SettingsSection::Permissions => permissions_section(permissions),
        SettingsSection::Fonts => fonts_section(config),
        SettingsSection::ClearData => data_clearing_section(clearing),
    };
    let scroll = widget::scrollable(
        widget::container(content)
            .max_width(720.0)
            .padding(spacing.space_l),
    )
    .height(Length::Fill);

    widget::row::with_capacity(2)
        .push(nav_column)
        .push(scroll)
        .spacing(spacing.space_m)
        .padding(spacing.space_m)
        .height(Length::Fill)
        .into()
}

/// Build the "General" section: startup page and default search engine.
fn general_section(config: &BrowserConfig) -> Element<'_, Message> {
    let spacing = libcosmic::theme::spacing();
    widget::column::with_capacity(4)
        .push(widget::text::title4("Startup"))
        .push(page_kind_controls(
            "On startup show",
            &config.startup_page,
            Message::SetStartupPageKind,
            Message::SetStartupUrl,
        ))
        .push(widget::text::title4("Search"))
        .push(search_engine_controls(config))
        .spacing(spacing.space_s)
        .width(Length::Fill)
        .into()
}

/// Build the default-search-engine controls: a dropdown of built-in engines plus
/// a "Custom" option that reveals a URL-template field.
fn search_engine_controls(config: &BrowserConfig) -> Element<'_, Message> {
    let spacing = libcosmic::theme::spacing();
    let mut labels: Vec<&str> = BUILTIN_ENGINES.iter().map(SearchEngine::name).collect();
    labels.push("Custom");
    let selected = match &config.search_engine {
        SearchEngine::Custom(_) => BUILTIN_ENGINES.len(),
        engine => BUILTIN_ENGINES
            .iter()
            .position(|e| e == engine)
            .unwrap_or(0),
    };

    // Preserve any existing custom template when re-selecting "Custom".
    let existing_custom = match &config.search_engine {
        SearchEngine::Custom(t) => t.clone(),
        _ => String::new(),
    };
    let engine_row = widget::row::with_capacity(2)
        .push(widget::text("Search engine").width(Length::Fill))
        .push(widget::dropdown(
            labels,
            Some(selected),
            move |i| match BUILTIN_ENGINES.get(i) {
                Some(engine) => Message::SetSearchEngine(engine.clone()),
                None => Message::SetSearchEngine(SearchEngine::Custom(existing_custom.clone())),
            },
        ))
        .align_y(Alignment::Center);

    let mut col = widget::column::with_capacity(3)
        .push(engine_row)
        .spacing(spacing.space_xs);
    if let SearchEngine::Custom(template) = &config.search_engine {
        col = col
            .push(
                widget::text_input("https://example.com/search?q={query}", template)
                    .on_input(Message::SetCustomSearchUrl)
                    .width(Length::Fill),
            )
            .push(widget::text::caption(
                "Use {query} where the search terms should go.",
            ));
    }
    col.into()
}

/// Build the "Privacy" section: tracking protection level and custom filter lists.
fn privacy_section<'a>(
    config: &'a BrowserConfig,
    filter_list_url: &'a str,
) -> Element<'a, Message> {
    let spacing = libcosmic::theme::spacing();
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

    let mut filter_section = widget::column::with_capacity(8)
        .push(widget::text::title4("Custom filter lists"))
        .spacing(spacing.space_xxs);

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
            .spacing(spacing.space_xxs);
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
        .spacing(spacing.space_xxs);
    filter_section = filter_section.push(add_row);

    widget::column::with_capacity(4)
        .push(widget::text::title4("Privacy"))
        .push(cookie_row)
        .push(widget::text::caption(cookie_description))
        .push(filter_section)
        .spacing(spacing.space_s)
        .width(Length::Fill)
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
