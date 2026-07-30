use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use crate::mpris::{MediaUpdate, PlaybackStatus};
use cosmic_webview::{
    ContextMenu, ContextMenuAction, ContextMenuItem, DialogMsg, EditingActionEvent,
    EmbedderControl, FIND_BAR_ID, FindBar, FindBarMsg, FindBarState, FindResult, InputEvent,
    MediaSessionActionType, MediaSessionPlaybackState, PermissionFeature, WebViewWidget,
    permission,
};
use dpi::PhysicalSize;
use libcosmic::app::context_drawer;
use libcosmic::app::{Core, Task};
use libcosmic::executor;
use libcosmic::iced::{Event, Length, Subscription, keyboard, window};
use libcosmic::prelude::*;
use libcosmic::widget;
use libcosmic::widget::menu::{self, action::MenuAction};
use libcosmic::widget::segmented_button;
use turso::Connection;
use url::Url;

use crate::config::{
    BUILTIN_ENGINES, BrowserConfig, ClearRange, CookieProtectionLevel, FilterListEntry,
    PermissionDefault, PermissionKind, SearchEngine, StartupPage, extract_search_query,
};
use crate::db::{
    BookmarkEntry, HistoryEntry, HistorySort, SitePermission, Suggestion, SuggestionKind,
};
use crate::filter_engine::FilterEngines;
use crate::pages;
use crate::tabs::{TabKind, TabState};
use crate::url_bar::{self, SecurityStatus, UrlBarPopover};
use cosmic_config::CosmicConfigEntry as _;

/// Page-zoom step applied per Ctrl+± press, and the clamp bounds.
const ZOOM_STEP: f32 = 0.1;
const ZOOM_MIN: f32 = 0.3;
const ZOOM_MAX: f32 = 5.0;

/// History rows fetched for the grid; trimmed to the visible cell count in `view`.
const NEW_TAB_TOP_SITES: usize = 48;

/// Map a permission feature to its configurable global-default category, if it
/// has one. Features without a dedicated default always fall back to "ask".
///
/// Feature comparison, stable keys, and labels live in
/// [`cosmic_webview::permission`] so every embedder shares them.
fn permission_kind(feature: PermissionFeature) -> Option<crate::config::PermissionKind> {
    use crate::config::PermissionKind;
    match feature {
        PermissionFeature::Geolocation => Some(PermissionKind::Geolocation),
        PermissionFeature::Notifications => Some(PermissionKind::Notifications),
        PermissionFeature::Camera => Some(PermissionKind::Camera),
        PermissionFeature::Microphone => Some(PermissionKind::Microphone),
        PermissionFeature::PersistentStorage => Some(PermissionKind::PersistentStorage),
        _ => None,
    }
}

/// Build the Bluetooth device-selection dialog listing `devices`. Picking a row
/// emits [`Message::BluetoothPick`]; cancelling emits [`Message::BluetoothCancel`].
fn bluetooth_dialog(
    devices: &[cosmic_webview::BluetoothDeviceDescription],
) -> Element<'_, Message> {
    let spacing = libcosmic::theme::spacing();
    let mut dialog = widget::dialog()
        .title("Connect to a Bluetooth device")
        .secondary_action(widget::button::standard("Cancel").on_press(Message::BluetoothCancel));

    if devices.is_empty() {
        dialog = dialog.body("No devices found.");
    } else {
        let mut list = widget::column::with_capacity(devices.len()).spacing(spacing.space_xxs);
        for (index, device) in devices.iter().enumerate() {
            let name = if device.name.is_empty() {
                "Unknown device"
            } else {
                device.name.as_str()
            };
            // Show the address beneath the name: many BLE devices advertise no
            // friendly name, so the address is what distinguishes them.
            let content = widget::column::with_capacity(2)
                .push(widget::text::body(name))
                .push(widget::text::caption(device.address.as_str()));
            list = list.push(
                widget::button::custom(content)
                    .width(Length::Fill)
                    .on_press(Message::BluetoothPick(index)),
            );
        }
        dialog = dialog.control(list);
    }
    dialog.into()
}

/// The host of the tab's current URL, if any — the key for per-site grants.
fn current_domain(tab: &crate::tabs::TabState) -> Option<String> {
    tab.servo
        .current_url()
        .and_then(|u| u.host_str().map(str::to_owned))
}

/// Whether a URL belongs in the history database.
///
/// Only real web pages are logged: `about:`, `data:`, `blob:`, and Servo's
/// internal error pages are excluded so they never appear in history.
fn is_loggable_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// The user's answer to a permission prompt.
#[derive(Clone, Copy, Debug)]
pub enum PermissionResponse {
    /// Block persistently (stored).
    Block,
    /// Deny for this session only (in-memory).
    DenySession,
    /// Allow for this session only (in-memory).
    AllowSession,
    /// Allow persistently (stored).
    AllowAlways,
}

/// How a pending permission request should be resolved.
enum PermissionOutcome {
    /// Grant automatically (stored grant or global "allow" default).
    Allow,
    /// Deny automatically (stored block or global "block" default).
    Deny,
    /// No stored decision and the default is "ask": show the prompt.
    Prompt,
}

// ── Context menu ──────────────────────────────────────────────────────────────

/// Unified action for both static fallback items and Servo-provided items.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ContextAction {
    GoBack,
    GoForward,
    Reload,
    Copy,
    Paste,
    // Servo executes these internally via ContextMenu::select().
    Servo(ContextMenuAction),
}

// ContextMenuAction implements PartialEq but not Eq; no floats in the enum so
// reflexivity holds and Eq is safe to declare manually.
impl Eq for ContextAction {}

impl MenuAction for ContextAction {
    type Message = Message;
    fn message(&self) -> Message {
        match self {
            ContextAction::GoBack => Message::GoBack,
            ContextAction::GoForward => Message::GoForward,
            ContextAction::Reload => Message::Reload,
            ContextAction::Copy => Message::CopySelection,
            ContextAction::Paste => Message::Paste,
            ContextAction::Servo(a) => Message::ContextMenuSelected(*a),
        }
    }
}

/// Build the static fallback menu shown before Servo responds with its items.
fn build_default_menu(can_go_back: bool, can_go_forward: bool) -> Vec<menu::Tree<Message>> {
    let items: Vec<menu::Item<ContextAction, &'static str>> = vec![
        if can_go_back {
            menu::Item::Button("Back", None, ContextAction::GoBack)
        } else {
            menu::Item::ButtonDisabled("Back", None, ContextAction::GoBack)
        },
        if can_go_forward {
            menu::Item::Button("Forward", None, ContextAction::GoForward)
        } else {
            menu::Item::ButtonDisabled("Forward", None, ContextAction::GoForward)
        },
        menu::Item::Button("Reload", None, ContextAction::Reload),
        menu::Item::Divider,
        menu::Item::Button("Copy", None, ContextAction::Copy),
        menu::Item::Button("Paste", None, ContextAction::Paste),
    ];
    menu::items(&HashMap::new(), items)
}

/// Build a context-sensitive menu from items provided by Servo.
fn build_servo_menu(items: &[ContextMenuItem]) -> Vec<menu::Tree<Message>> {
    let menu_items: Vec<menu::Item<ContextAction, String>> = items
        .iter()
        .map(|item| match item {
            ContextMenuItem::Item {
                label,
                action,
                enabled,
            } => {
                if *enabled {
                    menu::Item::Button(label.clone(), None, ContextAction::Servo(*action))
                } else {
                    menu::Item::ButtonDisabled(label.clone(), None, ContextAction::Servo(*action))
                }
            }
            ContextMenuItem::Separator => menu::Item::Divider,
        })
        .collect();
    menu::items(&HashMap::new(), menu_items)
}

// ── Toolbar overflow menu ─────────────────────────────────────────────────────

/// Actions in the toolbar "hamburger" menu. Each maps to the same message the
/// old header buttons emitted, so the bookmarks/history/settings flows are
/// unchanged — only their entry point moved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppMenuAction {
    Bookmarks,
    History,
    Settings,
}

impl MenuAction for AppMenuAction {
    type Message = Message;
    fn message(&self) -> Message {
        match self {
            AppMenuAction::Bookmarks => Message::BookmarkOpen,
            AppMenuAction::History => Message::HistoryOpen,
            AppMenuAction::Settings => Message::SettingsOpen,
        }
    }
}

/// Build the toolbar overflow ("hamburger") menu: bookmarks, history, settings.
fn build_app_menu<'a>() -> Element<'a, Message> {
    let root = widget::button::custom(widget::icon::from_name("open-menu-symbolic").icon())
        .padding([4, 8])
        .class(libcosmic::theme::Button::MenuRoot);

    let icon = |name| Some(widget::icon::from_name(name).into());
    let items = menu::items(
        &HashMap::new(),
        vec![
            menu::Item::Button(
                "Bookmarks",
                icon("user-bookmarks-symbolic"),
                AppMenuAction::Bookmarks,
            ),
            menu::Item::Button(
                "History",
                icon("document-open-recent-symbolic"),
                AppMenuAction::History,
            ),
            menu::Item::Divider,
            menu::Item::Button(
                "Settings",
                icon("emblem-system-symbolic"),
                AppMenuAction::Settings,
            ),
        ],
    );

    menu::bar(vec![menu::Tree::with_children(
        libcosmic::widget::RcElementWrapper::new(Element::from(root)),
        items,
    )])
    .item_height(menu::ItemHeight::Dynamic(40))
    .item_width(menu::ItemWidth::Uniform(200))
    .spacing(4.0)
    .into()
}

/// Resolve a configured page to the Servo URL to load and whether the internal
/// New Tab page should be shown instead of the webview.
fn resolve_page(page: &StartupPage) -> (Url, bool) {
    let blank = || Url::parse("about:blank").expect("about:blank is always valid");
    match page {
        StartupPage::NewTab => (blank(), true),
        StartupPage::Url(u) => (Url::parse(u).unwrap_or_else(|_| blank()), false),
        // SearchEngine has no dedicated landing page yet; show a blank webview.
        StartupPage::SearchEngine => (blank(), false),
    }
}

/// Map a dark-mode flag to the Servo theme that drives `prefers-color-scheme`.
fn servo_theme_from(is_dark: bool) -> cosmic_webview::Theme {
    if is_dark {
        cosmic_webview::Theme::Dark
    } else {
        cosmic_webview::Theme::Light
    }
}

/// The Servo theme matching the active COSMIC desktop theme.
fn servo_theme() -> cosmic_webview::Theme {
    servo_theme_from(libcosmic::theme::active().cosmic().is_dark)
}

// ── Context page ─────────────────────────────────────────────────────────────

/// Which page is currently shown in the context drawer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ContextPage {
    #[default]
    History,
    Bookmarks,
    /// New Tab page settings (grid size, what new tabs open).
    NewTabSettings,
}

// ── Application messages ──────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum Message {
    /// Servo needs to spin its event loop. All tabs share one engine so one
    /// wake signal covers all pending WebView events.
    ServoWake,
    /// A mouse or keyboard event to forward to the active tab's Servo.
    Input(InputEvent),
    /// URL bar gained focus — suspend keyboard forwarding to Servo.
    UrlBarFocused,
    /// URL bar lost focus — resume keyboard forwarding to Servo.
    UrlBarUnfocused,
    /// User edited the URL bar text.
    UrlBarChanged(String),
    /// User pressed Enter in the URL bar.
    Navigate,
    GoBack,
    GoForward,
    Reload,
    /// Stop the current page load. Note: the Servo embedding API does not yet
    /// expose a real stop method, so this is currently a no-op.
    StopLoading,
    CopySelection,
    Paste,
    /// User selected an item from the active tab's Servo context menu.
    ContextMenuSelected(ContextMenuAction),
    /// Forwarded from ServoState's built-in dialog handler.
    Dialog(DialogMsg),
    /// WebView widget was laid out at a new physical size.
    WebViewResized(u32, u32),
    /// HiDPI scale factor changed.
    ScaleChanged(f32),
    /// A bare modifier key was pressed or released; forwarded to the active
    /// WebView to keep the engine's global modifier state in sync (never carries
    /// typed content — see `ServoState::send_modifier_key`).
    ModifierKey(keyboard::Event),
    /// User activated (clicked) a tab.
    TabActivated(segmented_button::Entity),
    /// A media action arrived over MPRIS (play/pause/next/…); forward it to the
    /// active tab's engine.
    MediaAction(MediaSessionActionType),
    /// The user allowed (`true`) or denied (`false`) a remote-devtools connection.
    DevtoolsRespond(bool),
    /// User closed a tab.
    TabClosed(segmented_button::Entity),
    /// User requested a new tab.
    TabNew,
    /// Close the currently active tab (Ctrl+W).
    TabCloseActive,
    /// Activate the next tab, wrapping around (Ctrl+Tab).
    TabNext,
    /// Activate the previous tab, wrapping around (Ctrl+Shift+Tab).
    TabPrev,
    /// Open a link in a new background tab (middle-click or Ctrl+click).
    OpenLinkInNewTab(String),
    // ── New Tab page ────────────────────────────────────────────────────────────
    /// Navigate the active tab to a URL (top-site click on the New Tab page).
    NavigateTo(String),
    /// Pin or unpin a site shown on the New Tab page.
    TogglePinnedSite {
        url: String,
        title: String,
    },
    /// A pin/unpin write finished; reload the New Tab page data.
    PinnedChanged,
    /// Most-visited sites loaded for the New Tab grid.
    TopSitesLoaded(Vec<HistoryEntry>),
    /// Pinned sites loaded for the New Tab grid.
    PinnedLoaded(Vec<BookmarkEntry>),
    /// A thumbnail or favicon finished writing; refresh the tile-image map.
    TileImagesChanged,
    /// Pointer entered a New Tab grid cell; reveals its pin button.
    TileHovered(String),
    /// Pointer left the hovered New Tab grid cell.
    TileUnhovered,
    // ── Settings ────────────────────────────────────────────────────────────────
    /// Open the settings tab (focusing the existing one if already open).
    SettingsOpen,
    /// Select a settings section in the side navigation.
    SettingsSectionSelected(segmented_button::Entity),
    /// Enable/disable a user script by file name.
    ToggleUserScript(String),
    /// Delete a user script file.
    RemoveUserScript(String),
    /// Open the userscripts directory in the file manager.
    OpenUserscriptsFolder,
    /// Re-scan the userscripts directory and re-apply user content.
    RefreshUserContent,
    /// Enable/disable the user stylesheet (user.css).
    ToggleUserStylesheet(bool),
    /// Open user.css in the default editor.
    OpenUserStylesheet,
    /// Set what the first tab shows at startup, by dropdown index.
    SetStartupPageKind(usize),
    /// Set the fixed URL opened at startup when the "Fixed URL" kind is selected.
    SetStartupUrl(String),
    /// Set the default search engine.
    SetSearchEngine(SearchEngine),
    /// Set the custom search engine URL template (must contain `{query}`).
    SetCustomSearchUrl(String),
    /// Set the New Tab grid column count.
    SetNewTabColumns(u16),
    /// Set the New Tab grid row count.
    SetNewTabRows(u16),
    /// Set what new tabs open, by dropdown index (New Tab / URL / Search engine).
    SetNewTabPageKind(usize),
    /// Set the fixed URL new tabs open when the "Fixed URL" kind is selected.
    SetNewTabUrl(String),
    /// Open the New Tab settings context drawer (grid size, what new tabs open).
    OpenNewTabSettings,
    /// Close the New Tab settings context drawer.
    CloseNewTabSettings,
    // ── Zoom ──────────────────────────────────────────────────────────────────
    /// Increase the active tab's page zoom (Ctrl++).
    ZoomIn,
    /// Decrease the active tab's page zoom (Ctrl+-).
    ZoomOut,
    /// Reset the active tab's page zoom to 100 % (Ctrl+0).
    ZoomReset,
    /// Move keyboard focus to the URL bar and select its contents (Ctrl+L).
    FocusUrlBar,
    // ── URL-bar indicators ──────────────────────────────────────────────────────
    /// Toggle a URL-bar indicator popover (open it, or close it if already open).
    ToggleUrlBarPopover(UrlBarPopover),
    /// Close any open URL-bar indicator popover (e.g. click outside).
    CloseUrlBarPopover,
    /// Set the global default cookie-protection level (settings drawer).
    SetCookieProtection(CookieProtectionLevel),
    /// Set the active tab's per-site cookie-protection override (URL-bar popover).
    /// `None` removes the override, restoring the global default.
    SetCookieProtectionOverride(Option<CookieProtectionLevel>),
    // ── Data clearing (settings drawer) ─────────────────────────────────────────
    /// Set the time range the "Clear browsing data" action applies to (history only).
    SetClearRange(ClearRange),
    /// Toggle whether browsing history is included when clearing data.
    ToggleClearHistory(bool),
    /// Toggle whether cookies and site data are included when clearing data.
    ToggleClearCookies(bool),
    /// Ask for confirmation before clearing data (opens the confirmation dialog).
    RequestClearData,
    /// Dismiss the clear-data confirmation dialog without clearing.
    CancelClearData,
    /// Clear the selected data categories for the selected time range (confirmed).
    ClearBrowsingData,
    /// Clear all data (cookies, site storage, history) for a single site.
    ClearSiteData(String),
    /// Answer the active tab's pending permission request (prompt banner).
    PermissionRespond(PermissionResponse),
    /// Clear a permission decision for the active site (permissions popover).
    /// `persistent` selects the stored decision; otherwise the session one.
    PermissionClear {
        key: String,
        persistent: bool,
    },
    /// Per-site permission grants loaded from the database at startup.
    SitePermissionsLoaded(Vec<SitePermission>),
    /// Set the global default for a permission category (settings drawer).
    SetPermissionDefault(PermissionKind, PermissionDefault),
    /// Revoke a single stored per-site permission grant (settings drawer).
    RevokeSitePermission(String, String),
    /// Revoke every stored per-site permission grant (settings drawer).
    ResetAllSitePermissions,
    /// Toggle whether sites may use the camera and microphone (settings drawer).
    SetAllowCameraMicrophone(bool),
    // ── Web fonts (settings drawer) ─────────────────────────────────────────────
    /// Set the default serif font family for web content.
    SetWebFontSerif(String),
    /// Set the default sans-serif font family for web content.
    SetWebFontSansSerif(String),
    /// Set the default monospace font family for web content.
    SetWebFontMonospace(String),
    /// Set the default web-content font size, in pixels.
    SetWebFontSize(u16),
    /// Set the default web-content monospace font size, in pixels.
    SetWebFontMonospaceSize(u16),
    /// Reload all tabs so changed web fonts apply to already-loaded pages.
    ApplyWebFonts,
    /// User picked a Bluetooth device by index in the selection dialog.
    BluetoothPick(usize),
    /// User dismissed the Bluetooth device-selection dialog.
    BluetoothCancel,
    // ── Awesomebar ────────────────────────────────────────────────────────────
    /// Suggestion list loaded from history + bookmarks for the current input.
    SuggestionsReady(Vec<Suggestion>),
    /// User pressed ArrowDown to move to the next suggestion.
    SuggestionNext,
    /// User pressed ArrowUp to move to the previous suggestion (or restore input).
    SuggestionPrev,
    /// User clicked a suggestion at the given index.
    SuggestionActivated(usize),
    /// User clicked one of the quick-search engine buttons.
    SearchWithEngine(SearchEngine),
    /// Live autocomplete results arrived from the search engine's suggestion API.
    EngineSuggestionsReady(Vec<Suggestion>),
    // ── Bookmarks ─────────────────────────────────────────────────────────────
    /// Open the bookmarks context drawer and load entries.
    BookmarkOpen,
    /// Bookmarks context drawer closed via its X button.
    BookmarkClose,
    /// Bookmark entries loaded from DB.
    BookmarkLoaded(Vec<BookmarkEntry>),
    /// User typed in the bookmarks filter field.
    BookmarkFilterChanged(String),
    /// User clicked a bookmark entry to navigate.
    BookmarkNavigate(String),
    /// Toggle bookmark status for the active tab's URL.
    BookmarkToggle,
    /// Bookmark was added or removed; carry the URL and new state.
    BookmarkToggled {
        url: String,
        is_bookmarked: bool,
    },
    /// User clicked the delete button on a bookmark entry.
    BookmarkDeleteEntry(i64),
    /// Bookmark deletion confirmed; carry the id and URL for local cleanup.
    BookmarkEntryDeleted {
        id: i64,
        url: String,
    },
    /// Initial set of bookmarked URLs loaded when the database opened.
    BookmarksInitialized(HashSet<String>),
    // ── Filter engine ────────────────────────────────────────────────────────
    /// Filter engines built from downloaded lists; ready to apply to tabs.
    FilterEnginesReady(std::sync::Arc<FilterEngines>),
    /// Filter engine build failed (logged, no blocking until next startup).
    FilterEnginesFailed(String),
    /// Per-domain override loaded from DB after a navigation.
    FilterOverrideLoaded(Option<CookieProtectionLevel>),
    /// Add a custom filter list URL.
    AddCustomFilterList(String),
    /// Remove a custom filter list by index.
    RemoveCustomFilterList(usize),
    /// Enable or disable a custom filter list by index.
    ToggleCustomFilterList(usize),
    /// Text in the "add custom filter list" input changed.
    CustomFilterListUrlChanged(String),
    // ── Config ────────────────────────────────────────────────────────────────
    /// Browser configuration was updated externally (e.g. via cosmic-settings).
    ConfigChanged(BrowserConfig),
    // ── Database ──────────────────────────────────────────────────────────────
    /// Database opened successfully and is ready to use.
    DatabaseReady(Connection),
    /// Database failed to open — app continues without history.
    DatabaseFailed(String),
    // ── History ───────────────────────────────────────────────────────────────
    /// Open the history context drawer and load entries.
    HistoryOpen,
    /// History entries loaded from DB.
    HistoryLoaded(Vec<HistoryEntry>),
    /// User typed in the history filter field.
    HistoryFilterChanged(String),
    /// User changed the history sort order.
    HistorySortChanged(HistorySort),
    /// User clicked a history entry to navigate.
    HistoryNavigate(String),
    /// User clicked the delete button on a history entry.
    HistoryDeleteEntry(i64),
    /// History entry deletion confirmed.
    HistoryEntryDeleted(i64),
    /// History context drawer closed via its X button.
    HistoryClose,
    /// Background history log write finished; no UI update needed.
    VisitLogged,
    // ── Find in page ──────────────────────────────────────────────────────────
    /// Open the find bar (Ctrl+F).
    FindOpen,
    /// Close the find bar (Escape or ✕ button).
    FindClose,
    /// User interacted with the find bar widget.
    FindBar(FindBarMsg),
    /// Result from a find or find-navigate operation.
    FindResult(FindResult),
    /// New 1-based current index from find_next (0 = no match or no session).
    FindNext(usize),
    /// New 1-based current index from find_prev (0 = no match or no session).
    FindPrev(usize),
    // ── Developer tools ───────────────────────────────────────────────────────
    /// Show info about the running devtools server (F12).
    DevToolsInfo,
    /// Enable or disable the devtools server (settings toggle). Takes effect on restart.
    SetDevToolsEnabled(bool),
    /// Update the devtools server port (settings text field). Takes effect on restart.
    SetDevToolsPort(String),
    /// Close the info dialog.
    InfoDialogClose,
}

// ── Application state ─────────────────────────────────────────────────────────

pub struct App {
    core: Core,
    /// Tab model; each entity has associated [`TabState`] (including its URL bar).
    tabs: segmented_button::SingleSelectModel,
    /// Shared waker receiver: all tabs fire the same channel since they share one engine.
    servo_rx: async_channel::Receiver<()>,
    /// MPRIS media integration; `None` if the D-Bus server could not start.
    mpris: Option<crate::mpris::MprisHandle>,
    /// (active tab, its media generation) last published to MPRIS, so we only
    /// publish on an actual change or a tab switch.
    last_media: Option<(segmented_button::Entity, u64)>,
    /// A pending remote-devtools connection awaiting the user's allow/deny.
    /// Engine-level (one devtools server per process), so not per-tab.
    pending_devtools: Option<cosmic_webview::DevtoolsRequest>,
    /// HiDPI scale for converting logical cursor coordinates to physical pixels.
    scale: f32,
    /// Physical size of the webview area, updated on each resize.
    current_size: PhysicalSize<u32>,
    /// Kept alive while a context menu is open; dropping sends an auto-dismiss to Servo.
    context_menu: Option<ContextMenu>,
    /// Items from the most recent Servo context menu; cleared after selection or dismiss.
    context_items: Vec<ContextMenuItem>,
    /// Persistent browser settings loaded from cosmic_config.
    config: BrowserConfig,
    /// Handle used to write settings back to cosmic_config; None if it failed to open.
    config_handle: Option<cosmic_config::Config>,
    /// Which page is currently shown in the context drawer.
    context_page: ContextPage,
    /// Side navigation for the settings page; its active entry selects which
    /// section the settings tab shows.
    settings_nav: segmented_button::SingleSelectModel,
    // ── Awesomebar ────────────────────────────────────────────────────────────
    /// Current suggestion list shown below the URL bar, empty when hidden.
    suggestions: Vec<Suggestion>,
    /// Index of the highlighted suggestion (None = user's own typed text).
    suggestion_index: Option<usize>,
    /// The text the user actually typed, saved before arrow-key navigation.
    suggestion_query: String,
    // ── Database & history ────────────────────────────────────────────────────
    /// Live database connection; None until the async open task completes.
    db: Option<Connection>,
    /// History entries currently shown in the context drawer.
    history_entries: Vec<HistoryEntry>,
    /// Active filter text for the history drawer.
    history_filter: String,
    /// Active sort order for the history drawer.
    history_sort: HistorySort,
    // ── Bookmarks ─────────────────────────────────────────────────────────────
    /// Bookmark entries currently shown in the context drawer.
    bookmarks: Vec<BookmarkEntry>,
    /// Active filter text for the bookmarks drawer.
    bookmark_filter: String,
    /// Set of bookmarked URLs for fast star-icon lookup.
    bookmarked_urls: HashSet<String>,
    // ── New Tab page ──────────────────────────────────────────────────────────
    /// Most-visited sites shown in the New Tab grid.
    top_sites: Vec<HistoryEntry>,
    /// Sites pinned to the New Tab grid.
    pinned_sites: Vec<BookmarkEntry>,
    /// Cached thumbnail and favicon paths per grid URL. Rebuilt when the grid
    /// data changes or a thumbnail/favicon finishes writing.
    tile_images: HashMap<String, crate::thumbnails::TileImage>,
    /// URL of the New Tab grid cell under the pointer, which reveals its pin
    /// button. `None` when no cell is hovered.
    hovered_tile: Option<String>,
    // ── URL-bar indicators ────────────────────────────────────────────────────
    /// Which URL-bar indicator popover is open, if any.
    url_bar_popover: Option<UrlBarPopover>,
    // ── Content filtering ────────────────────────────────────────────────────
    /// Compiled filter engines; None while downloading/building at startup.
    filter_engines: Option<std::sync::Arc<FilterEngines>>,
    /// Text being entered for a new custom filter list URL.
    custom_filter_list_url: String,
    /// User scripts found in the userscripts directory, with enabled state.
    /// Refreshed from disk when the User content settings section is shown.
    user_scripts: Vec<crate::user_content::ScriptEntry>,
    // ── Data clearing ──────────────────────────────────────────────────────────
    /// Time range the "Clear browsing data" action applies to (history only).
    clear_range: ClearRange,
    /// Whether the clear action includes browsing history.
    clear_history_checked: bool,
    /// Whether the clear action includes cookies and site data.
    clear_cookies_checked: bool,
    /// Domains that currently hold cookies or web storage, shown for per-site
    /// clearing. Refreshed when the settings drawer opens and after each clear.
    site_data_domains: Vec<String>,
    /// True while the "Clear browsing data" confirmation dialog is shown.
    clear_confirm_open: bool,
    // ── Permissions ────────────────────────────────────────────────────────────
    /// Persisted per-site permission grants, keyed by `(domain, feature_key)`,
    /// mapping to `true` for allow and `false` for block. Loaded at startup and
    /// kept in sync with the `site_permissions` table for synchronous lookups.
    site_permissions: HashMap<(String, String), bool>,
    /// Sorted view of `site_permissions` for the settings list: `(domain,
    /// feature_key, allow)`. Rebuilt whenever the grants change.
    permission_grants: Vec<(String, String, bool)>,
    /// Session-only permission decisions, keyed by `(domain, feature_key)`,
    /// `true` = allow, `false` = deny. Not persisted; cleared on restart.
    session_permissions: HashMap<(String, String), bool>,
    /// Message shown in the info dialog (DevTools status), or `None` when closed.
    info_dialog_message: Option<String>,
    /// Text-field mirror of `config.devtools_port` for the settings UI.
    devtools_port_str: String,
}

/// Maps a key press to a browser action, implementing Firefox-compatible
/// keyboard shortcuts. Returns `None` for keys with no binding.
///
/// Unmodified Escape / arrow keys drive the awesomebar suggestion popup; the
/// Ctrl/Alt combinations are the browser chrome shortcuts (new tab, zoom, etc.).
fn shortcut_to_message(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> Option<Message> {
    use keyboard::key::Named;

    let ctrl = modifiers.control();
    let alt = modifiers.alt();
    let shift = modifiers.shift();

    if ctrl && let keyboard::Key::Character(c) = key {
        match c.to_lowercase().as_str() {
            "t" => return Some(Message::TabNew),
            "w" => return Some(Message::TabCloseActive),
            "l" => return Some(Message::FocusUrlBar),
            "r" => return Some(Message::Reload),
            // "+" needs Shift on most layouts; "=" is the same physical key.
            "+" | "=" => return Some(Message::ZoomIn),
            "-" => return Some(Message::ZoomOut),
            "0" => return Some(Message::ZoomReset),
            "f" => return Some(Message::FindOpen),
            _ => {}
        }
    }

    if ctrl && let keyboard::Key::Named(Named::Tab) = key {
        return Some(if shift {
            Message::TabPrev
        } else {
            Message::TabNext
        });
    }

    if alt && let keyboard::Key::Named(named) = key {
        match named {
            Named::ArrowLeft => return Some(Message::GoBack),
            Named::ArrowRight => return Some(Message::GoForward),
            _ => {}
        }
    }

    if let keyboard::Key::Named(Named::F5) = key {
        return Some(Message::Reload);
    }

    if let keyboard::Key::Named(Named::F12) = key {
        return Some(Message::DevToolsInfo);
    }

    if let keyboard::Key::Named(Named::F3) = key {
        return Some(Message::FindBar(if shift {
            FindBarMsg::Prev
        } else {
            FindBarMsg::Next
        }));
    }

    // Unmodified keys drive the suggestion popup.
    if !ctrl
        && !alt
        && let keyboard::Key::Named(named) = key
    {
        return match named {
            // Escape closes the find bar first; falls through to suggestions on second press.
            Named::Escape => Some(Message::FindClose),
            Named::ArrowDown => Some(Message::SuggestionNext),
            Named::ArrowUp => Some(Message::SuggestionPrev),
            _ => None,
        };
    }

    None
}

/// Whether a key press is a browser-chrome shortcut handled by the window
/// rather than the page. The webview uses this to avoid forwarding the key to
/// Servo — otherwise a focused page field would also receive it (e.g. Ctrl+=
/// would type "=").
///
/// Only modified combinations and function keys qualify; unmodified suggestion
/// keys (arrows, Escape) must still reach the page when the webview has focus.
fn is_chrome_shortcut(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> bool {
    use keyboard::key::Named;
    let modified = modifiers.control() || modifiers.alt();
    let is_function_key = matches!(
        key,
        keyboard::Key::Named(Named::F3 | Named::F5 | Named::F12)
    );
    (modified || is_function_key) && shortcut_to_message(key, modifiers).is_some()
}

impl App {
    /// Parse the active tab's URL bar text, navigate, and unfocus the URL bar.
    ///
    /// Input that looks like a URL (has `://` or a dot-only domain) is treated
    /// as an address; everything else is forwarded to the configured search engine.
    /// Returns a Task that moves keyboard focus away from the URL bar to the page.
    fn try_navigate(&mut self) -> Task<Message> {
        // Read URL bar text with a shared reference (released before mut borrow below).
        let raw = match self.tabs.active_data::<TabState>() {
            Some(tab) => tab.url_bar_text.trim().to_owned(),
            None => return Task::none(),
        };

        let explicit_url = if raw.contains("://") {
            Url::parse(&raw).ok()
        } else if raw.contains('.') && !raw.contains(' ') {
            Url::parse(&format!("https://{raw}")).ok()
        } else {
            None
        };
        // Track whether this is a search so we can log it.
        let is_search = explicit_url.is_none();
        let url = explicit_url.unwrap_or_else(|| {
            let search = self.config.search_engine.search_url(&raw);
            Url::parse(&search).expect("search engine URL must be valid")
        });
        let domain = url.host_str().map(|h| h.to_owned());
        let url_str = url.to_string();

        // Compute global default filter before the mutable borrow.
        let global_filter = self
            .filter_engines
            .as_ref()
            .map(|e| e.filter_fn(self.config.cookie_protection));

        let Some(tab) = self.tabs.active_data_mut::<TabState>() else {
            return Task::none();
        };
        tab.url_bar_text = url_str;
        tab.url_bar_focused = false;
        tab.show_new_tab_page = false;
        // Per-site override is cleared until we hear back from the DB lookup.
        tab.cookie_protection_override = None;
        // Any unanswered request belongs to the old page. Session permission
        // decisions persist across navigations (they are keyed by domain).
        tab.pending_permission = None;
        tab.pending_bluetooth = None;
        // Apply the global default filter while we wait for the DB lookup.
        tab.servo.set_content_filter(global_filter);
        tab.servo.navigate(url);

        let focus_task = libcosmic::iced::widget::operation::focus_next();

        let mut tasks = vec![focus_task];

        if is_search && let Some(conn) = self.db.clone() {
            let raw2 = raw.clone();
            tasks.push(Task::perform(
                async move {
                    if let Err(e) = crate::db::log_search_query(conn, raw2).await {
                        tracing::warn!("Search query log failed: {e}");
                    }
                },
                |_| libcosmic::action::app(Message::VisitLogged),
            ));
        }

        // Look up any saved per-domain override and restore it.
        if let (Some(domain), Some(conn)) = (domain, self.db.clone()) {
            tasks.push(Task::perform(
                async move { crate::db::load_site_setting(conn, domain).await },
                |result| {
                    let level = result.unwrap_or_else(|e| {
                        tracing::warn!("Site setting lookup failed: {e}");
                        None
                    });
                    libcosmic::action::app(Message::FilterOverrideLoaded(level))
                },
            ));
        }

        Task::batch(tasks)
    }

    /// Dismiss the active context menu without selecting an item.
    fn dismiss_context_menu(&mut self) {
        self.context_menu = None;
        self.context_items.clear();
    }

    /// Sync per-tab readback with the active tab: hidden tabs skip
    /// compositing and pixel readback entirely. Call after any change to
    /// which tab is active.
    fn sync_tab_visibility(&mut self) {
        let ids: Vec<_> = self.tabs.iter().collect();
        for id in ids {
            let is_active = self.tabs.is_active(id);
            if let Some(tab) = self.tabs.data_mut::<TabState>(id) {
                tab.servo.set_visible(is_active);
                // Apply a deferred reload (e.g. web-font change) now that the
                // tab is the visible one.
                if is_active && tab.needs_reload {
                    tab.servo.reload();
                    tab.needs_reload = false;
                }
            }
        }
    }

    /// Create a new tab loading `url` and register it in the tab model.
    /// The caller is responsible for activating the returned entity.
    /// `show_new_tab_page` makes the tab open on the internal New Tab page.
    fn add_tab(&mut self, url: Url, show_new_tab_page: bool) -> segmented_button::Entity {
        let (mut tab, _rx) = TabState::new(url, self.current_size, self.scale);
        tab.show_new_tab_page = show_new_tab_page;
        tab.servo.set_theme(servo_theme());
        self.tabs.insert().closable().text("New Tab").data(tab).id()
    }

    /// Open the settings page in a tab, focusing the existing one if present.
    ///
    /// Settings live in a tab (not a drawer) so they get the full window, like
    /// cosmic-settings. The tab still owns an unused webview — `TabState` always
    /// creates one — but renders the settings page instead (see `view`).
    fn open_settings_tab(&mut self) {
        let existing = self
            .tabs
            .iter()
            .find(|&id| self.tabs.data::<TabState>(id).map(|t| t.kind) == Some(TabKind::Settings));

        let id = existing.unwrap_or_else(|| {
            let blank = Url::parse("about:blank").expect("about:blank is always valid");
            let (mut tab, _rx) = TabState::new(blank, self.current_size, self.scale);
            tab.kind = TabKind::Settings;
            tab.servo.set_theme(servo_theme());
            self.tabs
                .insert()
                .closable()
                .text("Settings")
                .icon(widget::icon::from_name("emblem-system-symbolic").icon())
                .data(tab)
                .id()
        });
        self.tabs.activate(id);
        self.sync_tab_visibility();
    }

    /// Publishes the active tab's media state to MPRIS, but only when it changed
    /// since the last publish (new metadata/playback state, or a different active
    /// tab). No-op when the MPRIS server is unavailable.
    fn publish_media(&mut self) {
        if self.mpris.is_none() {
            return;
        }
        let active = self.tabs.active();
        let (generation, update) = {
            let Some(tab) = self.tabs.active_data::<TabState>() else {
                return;
            };
            let generation = tab.servo.media_generation();
            if self.last_media == Some((active, generation)) {
                return;
            }
            let status = match tab.servo.media_playback_state() {
                Some(MediaSessionPlaybackState::Playing) => PlaybackStatus::Playing,
                Some(MediaSessionPlaybackState::Paused) => PlaybackStatus::Paused,
                _ => PlaybackStatus::Stopped,
            };
            let (title, artist, album) = match tab.servo.media_metadata() {
                Some(m) => (
                    (!m.title.is_empty()).then_some(m.title),
                    (!m.artist.is_empty()).then_some(m.artist),
                    (!m.album.is_empty()).then_some(m.album),
                ),
                None => (None, None, None),
            };
            (
                generation,
                MediaUpdate {
                    status,
                    title,
                    artist,
                    album,
                },
            )
        };
        self.last_media = Some((active, generation));
        if let Some(mpris) = &self.mpris {
            mpris.publish(update);
        }
    }

    /// Close `id`, activating a neighbor first. Does nothing if it is the last tab.
    fn close_tab(&mut self, id: segmented_button::Entity) {
        if self.tabs.len() <= 1 {
            return;
        }
        if self.tabs.is_active(id) {
            if let Some(pos) = self.tabs.position(id) {
                let next = if pos == 0 { pos + 1 } else { pos - 1 };
                self.tabs.activate_position(next);
            }
            self.dismiss_context_menu();
        }
        // Dropping the entity also drops the associated TabState.
        self.tabs.remove(id);
    }

    /// Activate the next (`forward`) or previous tab, wrapping around.
    /// No-op when there is only one tab.
    fn cycle_tab(&mut self, forward: bool) {
        let count = self.tabs.len() as u16;
        if count <= 1 {
            return;
        }
        let Some(pos) = self.tabs.position(self.tabs.active()) else {
            return;
        };
        let next = if forward {
            (pos + 1) % count
        } else {
            (pos + count - 1) % count
        };
        self.tabs.activate_position(next);
        self.sync_tab_visibility();
        self.dismiss_context_menu();
        self.sync_active_title();
    }

    /// Adjust the active tab's page zoom by `delta`, clamped to the supported
    /// range. Rounding keeps repeated steps from accumulating float drift.
    fn adjust_zoom(&mut self, delta: f32) {
        if let Some(tab) = self.tabs.active_data::<TabState>() {
            let target = (tab.servo.page_zoom() + delta).clamp(ZOOM_MIN, ZOOM_MAX);
            let rounded = (target * 100.0).round() / 100.0;
            tab.servo.set_page_zoom(rounded);
        }
    }

    /// Issue a DB query and send the results back as `BookmarkLoaded`.
    fn reload_bookmarks(&self) -> Task<Message> {
        let Some(conn) = self.db.clone() else {
            return Task::none();
        };
        let filter = self.bookmark_filter.clone();
        Task::perform(
            async move { crate::db::query_bookmarks(conn, filter).await },
            |result| match result {
                Ok(entries) => libcosmic::action::app(Message::BookmarkLoaded(entries)),
                Err(e) => {
                    tracing::warn!("Bookmark query failed: {e}");
                    libcosmic::action::app(Message::BookmarkLoaded(vec![]))
                }
            },
        )
    }

    /// Show an informational dialog with a single OK button.
    fn show_info_dialog(&mut self, message: String) -> Task<Message> {
        self.info_dialog_message = Some(message);
        Task::none()
    }

    /// Issue a DB query and send the results back as `HistoryLoaded`.
    fn reload_history(&self) -> Task<Message> {
        let Some(conn) = self.db.clone() else {
            return Task::none();
        };
        let filter = self.history_filter.clone();
        let sort = self.history_sort;
        Task::perform(
            async move { crate::db::query_history(conn, filter, sort, 50).await },
            |result| match result {
                Ok(entries) => libcosmic::action::app(Message::HistoryLoaded(entries)),
                Err(e) => {
                    tracing::warn!("History query failed: {e}");
                    libcosmic::action::app(Message::HistoryLoaded(vec![]))
                }
            },
        )
    }

    /// Refresh the list of domains that hold cookies or web storage, shown for
    /// per-site clearing in the settings drawer.
    fn refresh_site_data_domains(&mut self) {
        let mut domains: Vec<String> = cosmic_webview::list_site_data()
            .iter()
            .map(|site| site.name())
            .collect();
        domains.sort();
        domains.dedup();
        self.site_data_domains = domains;
    }

    /// Clear the selected data categories for the selected time range.
    ///
    /// Cookies and web storage have no time filter in Servo, so they are always
    /// cleared in full; only history honours [`App::clear_range`].
    fn clear_browsing_data(&mut self) -> Task<Message> {
        if self.clear_cookies_checked {
            cosmic_webview::clear_all_cookies();
            let domains: Vec<&str> = self.site_data_domains.iter().map(String::as_str).collect();
            cosmic_webview::clear_site_data(&domains);
            self.refresh_site_data_domains();
        }

        if !self.clear_history_checked {
            return Task::none();
        }
        let Some(conn) = self.db.clone() else {
            return Task::none();
        };
        let within = self.clear_range.within_seconds();
        let clear = Task::perform(
            async move {
                if let Err(e) = crate::db::clear_history(conn, within).await {
                    tracing::warn!("Failed to clear history: {e}");
                }
            },
            |_| libcosmic::action::app(Message::VisitLogged),
        );
        clear
            .chain(self.reload_history())
            .chain(self.reload_new_tab())
    }

    /// Clear cookies, web storage, and history for a single site.
    fn clear_site(&mut self, domain: String) -> Task<Message> {
        cosmic_webview::clear_site_data(&[domain.as_str()]);
        self.refresh_site_data_domains();
        let Some(conn) = self.db.clone() else {
            return Task::none();
        };
        let clear = Task::perform(
            async move {
                if let Err(e) = crate::db::clear_history_for_domain(conn, domain).await {
                    tracing::warn!("Failed to clear site history: {e}");
                }
            },
            |_| libcosmic::action::app(Message::VisitLogged),
        );
        clear
            .chain(self.reload_history())
            .chain(self.reload_new_tab())
    }

    /// Answer a permission request for tab `id`: auto-allow/deny from a stored
    /// grant or global default, or hold it for the prompt banner.
    fn resolve_permission(
        &mut self,
        id: segmented_button::Entity,
        request: cosmic_webview::PermissionRequest,
        domain: Option<String>,
    ) -> Option<Task<Message>> {
        let feature = request.feature();
        match self.permission_outcome(feature, domain.as_deref()) {
            PermissionOutcome::Allow => request.allow(),
            PermissionOutcome::Deny => request.deny(),
            PermissionOutcome::Prompt => {
                if let Some(tab) = self.tabs.data_mut::<TabState>(id) {
                    tab.pending_permission = Some(request);
                }
            }
        }
        None
    }

    /// Decide the outcome for `feature` on `domain`: a stored per-site grant wins,
    /// otherwise the global default (absent any default, prompt).
    fn permission_outcome(
        &self,
        feature: PermissionFeature,
        domain: Option<&str>,
    ) -> PermissionOutcome {
        let key = permission::feature_key(feature);
        if let Some(domain) = domain {
            let map_key = (domain.to_owned(), key.to_owned());
            // A persistent decision wins; otherwise an in-session one applies.
            if let Some(&allow) = self
                .site_permissions
                .get(&map_key)
                .or_else(|| self.session_permissions.get(&map_key))
            {
                return if allow {
                    PermissionOutcome::Allow
                } else {
                    PermissionOutcome::Deny
                };
            }
        }
        match permission_kind(feature).map(|kind| self.config.permission_defaults.get(kind)) {
            Some(PermissionDefault::Allow) => PermissionOutcome::Allow,
            Some(PermissionDefault::Block) => PermissionOutcome::Deny,
            _ => PermissionOutcome::Prompt,
        }
    }

    /// Remember a per-site permission decision in memory and persist it.
    fn persist_permission(
        &mut self,
        domain: String,
        feature: PermissionFeature,
        allow: bool,
    ) -> Task<Message> {
        let key = permission::feature_key(feature).to_owned();
        self.site_permissions
            .insert((domain.clone(), key.clone()), allow);
        self.rebuild_permission_grants();
        let Some(conn) = self.db.clone() else {
            return Task::none();
        };
        let decision = if allow { "allow" } else { "block" }.to_owned();
        Task::perform(
            async move {
                if let Err(e) = crate::db::save_site_permission(conn, domain, key, decision).await {
                    tracing::warn!("Failed to save site permission: {e}");
                }
            },
            |_| libcosmic::action::app(Message::VisitLogged),
        )
    }

    /// Rebuild the sorted display list of per-site permission grants.
    fn rebuild_permission_grants(&mut self) {
        let mut grants: Vec<(String, String, bool)> = self
            .site_permissions
            .iter()
            .map(|((domain, key), allow)| (domain.clone(), key.clone(), *allow))
            .collect();
        grants.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        self.permission_grants = grants;
    }

    /// Forget a per-site permission grant in memory and the database.
    fn forget_permission_key(&mut self, domain: String, key: String) -> Task<Message> {
        self.site_permissions.remove(&(domain.clone(), key.clone()));
        self.rebuild_permission_grants();
        let Some(conn) = self.db.clone() else {
            return Task::none();
        };
        Task::perform(
            async move {
                if let Err(e) = crate::db::delete_site_permission(conn, domain, key).await {
                    tracing::warn!("Failed to delete site permission: {e}");
                }
            },
            |_| libcosmic::action::app(Message::VisitLogged),
        )
    }

    /// Permission decisions (persistent and session) for the active tab's domain,
    /// shown in the URL-bar permissions popover. Sorted by label.
    fn permission_entries(&self) -> Vec<url_bar::PermissionEntry> {
        let Some(domain) = self.tabs.active_data::<TabState>().and_then(current_domain) else {
            return Vec::new();
        };
        let label = |key: &str| permission::feature_label_from_key(key).unwrap_or("Permission");
        let mut entries = Vec::new();
        for ((d, key), &allowed) in &self.site_permissions {
            if *d == domain {
                entries.push(url_bar::PermissionEntry {
                    key: key.clone(),
                    label: label(key),
                    allowed,
                    persistent: true,
                });
            }
        }
        for ((d, key), &allowed) in &self.session_permissions {
            if *d == domain {
                entries.push(url_bar::PermissionEntry {
                    key: key.clone(),
                    label: label(key),
                    allowed,
                    persistent: false,
                });
            }
        }
        entries.sort_by(|a, b| a.label.cmp(b.label));
        entries
    }

    /// Reload both data sources for the New Tab grid (top sites + pinned).
    fn reload_new_tab(&self) -> Task<Message> {
        let Some(conn) = self.db.clone() else {
            return Task::none();
        };
        let top = Task::perform(
            {
                let conn = conn.clone();
                async move {
                    crate::db::query_history(
                        conn,
                        String::new(),
                        HistorySort::MostVisited,
                        NEW_TAB_TOP_SITES,
                    )
                    .await
                }
            },
            |result| match result {
                Ok(entries) => libcosmic::action::app(Message::TopSitesLoaded(entries)),
                Err(e) => {
                    tracing::warn!("Top sites query failed: {e}");
                    libcosmic::action::app(Message::TopSitesLoaded(vec![]))
                }
            },
        );
        let pinned = Task::perform(
            async move { crate::db::query_pinned(conn).await },
            |result| match result {
                Ok(entries) => libcosmic::action::app(Message::PinnedLoaded(entries)),
                Err(e) => {
                    tracing::warn!("Pinned query failed: {e}");
                    libcosmic::action::app(Message::PinnedLoaded(vec![]))
                }
            },
        );
        Task::batch(vec![top, pinned])
    }

    /// Recompute the best cached image for every grid URL: a thumbnail if one
    /// exists on disk, otherwise a favicon, otherwise nothing (generic icon).
    fn rebuild_tile_images(&mut self) {
        use crate::thumbnails::{TileImage, favicon_path, thumbnail_path};

        let urls = self
            .pinned_sites
            .iter()
            .map(|b| b.url.clone())
            .chain(self.top_sites.iter().map(|h| h.url.clone()));

        let mut images = HashMap::new();
        for url in urls {
            let exists = |p: PathBuf| p.exists().then_some(p);
            let tile = TileImage {
                thumbnail: exists(thumbnail_path(&url)),
                favicon: exists(favicon_path(&url)),
            };
            if !tile.is_empty() {
                images.insert(url, tile);
            }
        }
        self.tile_images = images;
    }

    /// Capture a thumbnail and fetch a favicon for the active tab once its
    /// current page has finished loading. No-op until the page is complete, for
    /// non-HTTP(S) URLs, or when this URL was already captured.
    fn maybe_capture_active_thumbnail(&mut self) -> Task<Message> {
        use cosmic_webview::LoadStatus;

        let Some(tab) = self.tabs.active_data_mut::<TabState>() else {
            return Task::none();
        };
        if tab.servo.load_status() != LoadStatus::Complete || tab.servo.frame_generation() == 0 {
            return Task::none();
        }
        let Some(url) = tab.servo.current_url() else {
            return Task::none();
        };
        if !matches!(url.scheme(), "http" | "https") {
            return Task::none();
        }
        let url = url.to_string();
        if tab.thumb_captured_url.as_deref() == Some(&url) {
            return Task::none();
        }
        tab.thumb_captured_url = Some(url.clone());

        let mut tasks = Vec::new();

        // The GPU readback must run on this thread; the downscale + encode are
        // offloaded to the executor.
        if let Some(frame) = tab.servo.screenshot() {
            let url = url.clone();
            tasks.push(Task::perform(
                async move { crate::thumbnails::save_thumbnail(&url, &frame) },
                |result| {
                    if let Err(e) = &result {
                        tracing::warn!("Thumbnail save failed: {e}");
                    }
                    libcosmic::action::app(Message::TileImagesChanged)
                },
            ));
        }

        // Refresh the favicon only when it is missing or has aged out.
        let favicon = crate::thumbnails::favicon_path(&url);
        if crate::thumbnails::is_stale(&favicon, crate::thumbnails::MAX_AGE) {
            tasks.push(Task::perform(
                async move { crate::thumbnails::fetch_favicon(url).await },
                |result| {
                    if let Err(e) = &result {
                        tracing::debug!("Favicon fetch failed: {e}");
                    }
                    libcosmic::action::app(Message::TileImagesChanged)
                },
            ));
        }

        Task::batch(tasks)
    }

    /// Proactively fetch favicons for grid sites that have no thumbnail and no
    /// fresh cached favicon, so the New Tab page shows favicons for sites not
    /// visited since the cache was built. Deduplicated by origin.
    fn fetch_missing_favicons(&self) -> Task<Message> {
        let urls = self
            .pinned_sites
            .iter()
            .map(|b| b.url.clone())
            .chain(self.top_sites.iter().map(|h| h.url.clone()));

        let mut seen = HashSet::new();
        let mut tasks = Vec::new();
        for url in urls {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                continue;
            }
            // A thumbnail already wins in `rebuild_tile_images`; skip the fetch.
            if crate::thumbnails::thumbnail_path(&url).exists() {
                continue;
            }
            let favicon = crate::thumbnails::favicon_path(&url);
            // One fetch per origin, and only when missing or aged out.
            if !seen.insert(favicon.clone())
                || !crate::thumbnails::is_stale(&favicon, crate::thumbnails::MAX_AGE)
            {
                continue;
            }
            tasks.push(Task::perform(
                async move { crate::thumbnails::fetch_favicon(url).await },
                |result| {
                    if let Err(e) = &result {
                        tracing::debug!("Favicon fetch failed: {e}");
                    }
                    libcosmic::action::app(Message::TileImagesChanged)
                },
            ));
        }
        Task::batch(tasks)
    }

    /// Write a settings change through `cosmic_config`.
    ///
    /// The closure runs the generated per-field setter, which both persists the
    /// value and updates `self.config` in place. Failures are logged, not fatal.
    fn write_config(
        &mut self,
        set: impl FnOnce(
            &mut BrowserConfig,
            &cosmic_config::Config,
        ) -> Result<bool, cosmic_config::Error>,
    ) {
        let Some(handle) = self.config_handle.clone() else {
            tracing::warn!("Cannot save settings: config handle unavailable");
            return;
        };
        if let Err(e) = set(&mut self.config, &handle) {
            tracing::warn!("Settings write failed: {e}");
        }
    }

    /// Push the current web-font settings into the engine (next page load).
    fn apply_web_fonts(&self) {
        cosmic_webview::set_web_fonts(self.config.web_fonts());
    }

    /// Push the current user scripts and stylesheet into the engine. Applies on
    /// the next page load, so reload open tabs to see the change immediately.
    fn apply_user_content(&self) {
        cosmic_webview::set_user_content(crate::user_content::load(&self.config));
    }

    /// Re-scan the userscripts directory into `self.user_scripts`.
    fn refresh_user_scripts(&mut self) {
        self.user_scripts = crate::user_content::list_scripts(&self.config);
    }

    /// Apply an engine-level change (e.g. fonts) to existing pages: reload the
    /// active tab now and flag the rest to reload lazily when next shown, so we
    /// never reload many background tabs at once.
    fn reload_active_flag_others(&mut self) {
        let ids: Vec<_> = self.tabs.iter().collect();
        for id in ids {
            let is_active = self.tabs.is_active(id);
            if let Some(tab) = self.tabs.data_mut::<TabState>(id) {
                if is_active {
                    tab.servo.reload();
                    tab.needs_reload = false;
                } else {
                    tab.needs_reload = true;
                }
            }
        }
    }

    /// Returns the effective cookie-protection level for a tab.
    fn effective_level(&self, tab: &crate::tabs::TabState) -> CookieProtectionLevel {
        tab.cookie_protection_override
            .unwrap_or(self.config.cookie_protection)
    }

    /// Install the correct content filter on a tab based on its effective level.
    fn apply_filter_to_tab(&self, tab: &crate::tabs::TabState) {
        let Some(engines) = &self.filter_engines else {
            return;
        };
        let level = self.effective_level(tab);
        tab.servo.set_content_filter(Some(engines.filter_fn(level)));
    }

    /// Start an async task that downloads filter lists and builds the engines.
    ///
    /// Custom filter lists from config are included in both Standard and Strict engines.
    fn build_filter_engines(&self) -> Task<Message> {
        let custom_lists: Vec<FilterListEntry> = self
            .config
            .custom_filter_lists
            .iter()
            .filter(|e| e.enabled)
            .cloned()
            .collect();
        Task::perform(
            async move {
                let cache_dir = crate::filter_lists::cache_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("/tmp/cosmic-web-filters"));
                let mut standard_texts = Vec::new();
                let mut strict_extra_texts = Vec::new();
                let mut custom_texts = Vec::new();

                // Download all built-in lists concurrently.
                let standard_futs: Vec<_> =
                    crate::filter_lists::STANDARD_LISTS
                        .iter()
                        .map(|(id, url)| {
                            let cache_dir = cache_dir.clone();
                            async move {
                                crate::filter_lists::load_or_refresh(id, url, &cache_dir).await
                            }
                        })
                        .collect();
                let strict_futs: Vec<_> =
                    crate::filter_lists::STRICT_EXTRA_LISTS
                        .iter()
                        .map(|(id, url)| {
                            let cache_dir = cache_dir.clone();
                            async move {
                                crate::filter_lists::load_or_refresh(id, url, &cache_dir).await
                            }
                        })
                        .collect();
                let custom_futs: Vec<_> =
                    custom_lists
                        .iter()
                        .map(|entry| {
                            let id = entry.label.replace(' ', "-").to_lowercase();
                            let url = entry.url.clone();
                            let cache_dir = cache_dir.clone();
                            async move {
                                crate::filter_lists::load_or_refresh(&id, &url, &cache_dir).await
                            }
                        })
                        .collect();

                for fut in standard_futs {
                    standard_texts.push(fut.await);
                }
                for fut in strict_futs {
                    strict_extra_texts.push(fut.await);
                }
                for fut in custom_futs {
                    custom_texts.push(fut.await);
                }

                // Build is CPU-intensive; run on a blocking thread to not stall
                // the async executor. Owned Vecs are moved in; &str slices are
                // created inside to satisfy the 'static bound.
                let engines = tokio::task::spawn_blocking(move || {
                    let standard_refs: Vec<&str> =
                        standard_texts.iter().map(String::as_str).collect();
                    let strict_refs: Vec<&str> =
                        strict_extra_texts.iter().map(String::as_str).collect();
                    let custom_refs: Vec<&str> = custom_texts.iter().map(String::as_str).collect();
                    crate::filter_engine::FilterEngines::build(
                        &standard_refs,
                        &strict_refs,
                        &custom_refs,
                    )
                })
                .await;
                match engines {
                    Ok(e) => Ok(std::sync::Arc::new(e)),
                    Err(e) => Err(e.to_string()),
                }
            },
            |result| match result {
                Ok(engines) => libcosmic::action::app(Message::FilterEnginesReady(engines)),
                Err(e) => libcosmic::action::app(Message::FilterEnginesFailed(e)),
            },
        )
    }

    /// Apply the active filter to every open tab.
    fn apply_filter_to_all_tabs(&self) {
        let ids: Vec<_> = self.tabs.iter().collect();
        for id in ids {
            if let Some(tab) = self.tabs.data::<crate::tabs::TabState>(id) {
                self.apply_filter_to_tab(tab);
            }
        }
    }

    /// Update the window title to match the currently active tab.
    fn sync_active_title(&mut self) {
        let title = self
            .tabs
            .active_data::<TabState>()
            .and_then(|tab| tab.servo.title())
            .unwrap_or_default();
        self.set_header_title(title);
    }
}

// ── Servo subscription helper ─────────────────────────────────────────────────

/// Newtype so `async_channel::Receiver<()>` satisfies `Hash` for `Subscription::run_with`.
#[derive(Clone)]
struct ServoRx(async_channel::Receiver<()>);

impl Hash for ServoRx {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::any::TypeId::of::<Self>().hash(state);
    }
}

/// Newtype so the MPRIS action receiver satisfies `Hash` for `Subscription::run_with`.
#[derive(Clone)]
struct MprisRx(async_channel::Receiver<MediaSessionActionType>);

impl Hash for MprisRx {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::any::TypeId::of::<Self>().hash(state);
    }
}

// ── Application trait implementation ─────────────────────────────────────────

impl libcosmic::Application for App {
    type Executor = executor::Default;
    type Flags = Option<Url>;
    type Message = Message;

    const APP_ID: &'static str = "io.github.snaggen.CosmicWeb";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, flags: Option<Url>) -> (Self, Task<Message>) {
        let scale = std::env::var("COSMIC_SCALE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(1.0);

        let config_handle = cosmic_config::Config::new(Self::APP_ID, BrowserConfig::VERSION).ok();
        let config = config_handle
            .as_ref()
            .map(|c| {
                BrowserConfig::get_entry(c).unwrap_or_else(|(errs, c)| {
                    for e in errs {
                        tracing::warn!("Config load error: {e}");
                    }
                    c
                })
            })
            .unwrap_or_default();

        cosmic_webview::set_profile_dir(crate::db::profile_dir());
        // Apply the camera/microphone opt-in before the engine starts so it is in
        // effect from the first page load.
        cosmic_webview::set_camera_microphone_enabled(config.allow_camera_microphone);
        // Apply web-content fonts before the engine starts.
        cosmic_webview::set_web_fonts(config.web_fonts());
        // Enable the remote devtools server if configured (must be before first tab).
        if config.devtools_enabled {
            cosmic_webview::set_devtools_enabled(config.devtools_port);
        }
        // Apply user scripts / stylesheet before the engine starts so the first
        // page load already has them.
        cosmic_webview::set_user_content(crate::user_content::load(&config));
        let user_scripts = crate::user_content::list_scripts(&config);

        let initial_size = PhysicalSize::new(1280u32, 800);
        // A command-line URL overrides the configured startup page.
        let (initial_url, show_new_tab_page) = match flags {
            Some(url) => (url, false),
            None => resolve_page(&config.startup_page),
        };

        let (mut first_tab, servo_rx) = TabState::new(initial_url, initial_size, scale);
        first_tab.show_new_tab_page = show_new_tab_page;
        first_tab.servo.set_theme(servo_theme());

        let mut tabs = segmented_button::SingleSelectModel::default();
        let first_id = tabs
            .insert()
            .closable()
            .text("New Tab")
            .data(first_tab)
            .id();
        tabs.activate(first_id);

        // Build the settings side navigation, one entry per section.
        let mut settings_nav = segmented_button::SingleSelectModel::default();
        for &section in pages::settings::SettingsSection::ALL {
            settings_nav.insert().text(section.label()).data(section);
        }
        let first_section = settings_nav.iter().next();
        if let Some(first) = first_section {
            settings_nav.activate(first);
        }

        let devtools_port_str = config.devtools_port.to_string();

        let app = App {
            core,
            config,
            config_handle,
            context_page: ContextPage::default(),
            settings_nav,
            suggestions: Vec::new(),
            suggestion_index: None,
            suggestion_query: String::new(),
            tabs,
            servo_rx,
            mpris: crate::mpris::spawn(),
            last_media: None,
            pending_devtools: None,
            scale,
            current_size: initial_size,
            context_menu: None,
            context_items: Vec::new(),
            db: None,
            history_entries: Vec::new(),
            history_filter: String::new(),
            history_sort: HistorySort::default(),
            bookmarks: Vec::new(),
            bookmark_filter: String::new(),
            bookmarked_urls: HashSet::new(),
            top_sites: Vec::new(),
            pinned_sites: Vec::new(),
            tile_images: HashMap::new(),
            hovered_tile: None,
            url_bar_popover: None,
            filter_engines: None,
            custom_filter_list_url: String::new(),
            user_scripts,
            clear_range: ClearRange::default(),
            clear_history_checked: true,
            clear_cookies_checked: true,
            site_data_domains: Vec::new(),
            clear_confirm_open: false,
            site_permissions: HashMap::new(),
            permission_grants: Vec::new(),
            session_permissions: HashMap::new(),
            info_dialog_message: None,
            devtools_port_str,
        };

        let db_task = Task::perform(
            async { crate::db::open(crate::db::db_path()).await },
            |result| match result {
                Ok(conn) => libcosmic::action::app(Message::DatabaseReady(conn)),
                Err(e) => libcosmic::action::app(Message::DatabaseFailed(e.to_string())),
            },
        );
        let filter_task = app.build_filter_engines();
        (app, Task::batch(vec![db_task, filter_task]))
    }

    fn on_app_exit(&mut self) -> Option<Message> {
        // Drop all tabs first so every Servo clone is released, then shut down
        // the shared engine. The final drop triggers ServoInner::Drop which
        // flushes cookie_jar.json to disk.
        self.tabs = segmented_button::SingleSelectModel::default();
        cosmic_webview::shutdown_engine();
        None
    }

    fn subscription(&self) -> Subscription<Message> {
        use libcosmic::iced::event;
        use libcosmic::iced::futures::StreamExt as _;

        // Drive the shared Servo event loop. While the foreground tab is animating,
        // clock spins at the display refresh rate via `window::frames`: the offscreen
        // rendering context has no vsync, so relying on Servo's EventLoopWaker would
        // spin as fast as the CPU allows (hundreds of fps). When idle, spin on the
        // waker channel so async work (network, timers, input results) stays
        // responsive. The two are mutually exclusive — one spin processes all pending
        // engine work, so a coalesced/dropped waker signal loses nothing.
        //
        // Spin is driven here (app level), not inside WebViewWidget, on purpose. A
        // widget-driven spin (like servoshell's winit loop) was evaluated and rejected:
        // (1) iced requests a redraw on every message (needs_redraw = !messages
        // .is_empty()), so a widget that spins and notifies would feed a
        // message→redraw→spin loop; (2) the Servo engine is process-global and shared
        // by all tabs, but only the active tab's widget renders — a tab that shows no
        // WebViewWidget (e.g. Settings) would then stop spinning the engine and stall
        // background loads. App-level spin avoids both.
        let animating = self
            .tabs
            .active_data::<TabState>()
            .is_some_and(|tab| tab.servo.animating());
        let spin_sub = if animating {
            libcosmic::iced::window::frames().map(|_| Message::ServoWake)
        } else {
            Subscription::run_with(ServoRx(self.servo_rx.clone()), |s| {
                s.0.clone().map(|_| Message::ServoWake)
            })
        };

        let window_sub = event::listen_with(|event, _status, _id| match event {
            Event::Window(window::Event::Rescaled(factor)) => Some(Message::ScaleChanged(factor)),
            Event::Keyboard(ke) => match &ke {
                // A chrome shortcut takes precedence; otherwise forward the event
                // so a bare modifier key keeps the engine's modifier state in sync
                // at the window level (a release is seen even while a non-webview
                // page is shown). ModifierKey ignores non-modifier keys.
                keyboard::Event::KeyPressed { key, modifiers, .. } => {
                    shortcut_to_message(key, *modifiers)
                        .or_else(|| Some(Message::ModifierKey(ke.clone())))
                }
                keyboard::Event::KeyReleased { .. } => Some(Message::ModifierKey(ke.clone())),
                keyboard::Event::ModifiersChanged(_) => None,
            },
            _ => None,
        });

        let config_sub = self
            .watch_config::<BrowserConfig>(Self::APP_ID)
            .map(|update| {
                if !update.errors.is_empty() {
                    tracing::warn!("Config load errors: {:?}", update.errors);
                }
                Message::ConfigChanged(update.config)
            });

        // Forward MPRIS media actions (from the applet or hardware keys) into the
        // app so the active tab's engine receives them.
        let mut subs = vec![spin_sub, window_sub, config_sub];
        if let Some(mpris) = &self.mpris {
            let sub = Subscription::run_with(MprisRx(mpris.action_rx.clone()), |s| {
                s.0.clone().map(Message::MediaAction)
            });
            subs.push(sub);
        }

        Subscription::batch(subs)
    }

    /// Push the new light/dark theme into every tab's engine so web content's
    /// `prefers-color-scheme` follows the desktop without a reload.
    fn system_theme_update(
        &mut self,
        _keys: &[&'static str],
        new_theme: &libcosmic::cosmic_theme::Theme,
    ) -> Task<Message> {
        let theme = servo_theme_from(new_theme.is_dark);
        let ids: Vec<_> = self.tabs.iter().collect();
        for id in ids {
            if let Some(tab) = self.tabs.data::<TabState>(id) {
                tab.servo.set_theme(theme);
            }
        }
        Task::none()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ServoWake => {
                // Spin the shared engine once — covers all pending WebView events.
                if let Some(tab) = self.tabs.active_data::<TabState>() {
                    tab.servo.spin();
                }

                // A remote-devtools client may be requesting a connection. This is
                // engine-level (one server per process), so poll it once per wake.
                if let Some(request) = cosmic_webview::take_devtools_request() {
                    self.pending_devtools = Some(request);
                }

                // WebViews the engine opened for us (window.open, target=_blank),
                // collected across all tabs and adopted after the loop so we don't
                // mutate the tab model while iterating it.
                let mut adopted: Vec<cosmic_webview::ServoState> = Vec::new();

                // Set when an engine favicon was cached this wake, to refresh tiles.
                let mut favicon_saved = false;

                // Tabs whose page called window.close(); removed after the loop
                // so we don't mutate the tab model while iterating it.
                let mut closed_ids: Vec<segmented_button::Entity> = Vec::new();

                // Process state changes for every tab.
                let ids: Vec<_> = self.tabs.iter().collect();
                for id in ids {
                    let is_active = self.tabs.is_active(id);

                    // Collect volatile state while holding the mutable borrow.
                    let (title, dialog, crash, permission, favicon, favicon_url) = {
                        let Some(tab) = self.tabs.data_mut::<TabState>(id) else {
                            continue;
                        };

                        // Context menus only matter for the active (visible) tab.
                        let dialog = if is_active {
                            tab.servo.take_dialog()
                        } else {
                            let _ = tab.servo.take_dialog();
                            None
                        };

                        // Keep URL bar in sync unless the user is typing.
                        if !tab.url_bar_focused
                            && let Some(url) = tab.servo.current_url()
                        {
                            tab.url_bar_text = url.to_string();
                        }

                        // A page may ask for a permission (geolocation, camera, …).
                        // Capture it with the requesting domain; the decision needs
                        // `self`, so it is resolved after this borrow ends.
                        let permission = tab.servo.take_permission_request().map(|request| {
                            let domain = tab
                                .servo
                                .current_url()
                                .and_then(|u| u.host_str().map(str::to_owned));
                            (request, domain)
                        });

                        // A page may ask the user to pick a Bluetooth device; hold
                        // the request until the picker dialog is answered.
                        if let Some(request) = tab.servo.take_bluetooth_request() {
                            tab.pending_bluetooth = Some(request);
                        }

                        // A page may open popups/new windows via window.open() or a
                        // target="_blank" link; adopt each as a new tab below.
                        while let Some(new_servo) = tab.servo.take_pending_new_tab() {
                            adopted.push(new_servo);
                        }

                        // A page may close its own tab via window.close().
                        if tab.servo.take_closed() {
                            closed_ids.push(id);
                        }

                        let title = tab.servo.title();

                        // Favicon reported by the engine (respects <link rel=icon>,
                        // SVG, data: URIs); captured with its URL to cache per origin.
                        let favicon = tab.servo.take_favicon();
                        let favicon_url = tab.servo.current_url().map(|u| u.to_string());

                        // No frame copying here: the engine fills the shared
                        // frame buffer directly and the widget uploads it on
                        // the next draw. See cosmic-webview's FrameSink.

                        let crash = tab.servo.take_crash();
                        (title, dialog, crash, permission, favicon, favicon_url)
                    };

                    if let Some((request, domain)) = permission
                        && let Some(task) = self.resolve_permission(id, request, domain)
                    {
                        return task;
                    }

                    if let Some(title) = title {
                        self.tabs.text_set(id, title.clone());
                        if is_active {
                            self.set_header_title(title);
                        }
                    }

                    if let Some((reason, _)) = crash {
                        tracing::error!("WebView crashed: {reason}");
                    }

                    // Show the favicon at the left of the tab, and cache it per
                    // origin. Saving synchronously makes the file fresh before the
                    // HTTP fallback in maybe_capture_active_thumbnail checks it, so
                    // the engine icon suppresses the redundant network fetch.
                    if let Some(rgba) = favicon {
                        if let Some(url) = &favicon_url {
                            if let Err(e) = crate::thumbnails::save_favicon(url, &rgba) {
                                tracing::debug!("Favicon save failed: {e}");
                            }
                            favicon_saved = true;
                        }
                        let (w, h) = (rgba.width(), rgba.height());
                        let handle = widget::icon::from_raster_pixels(w, h, rgba.into_raw());
                        self.tabs.icon_set(id, widget::icon::icon(handle));
                    }

                    match dialog {
                        Some(EmbedderControl::ContextMenu(menu)) => {
                            self.context_items = menu.items().to_vec();
                            self.context_menu = Some(menu);
                        }
                        Some(control) => {
                            if let Some(tab) = self.tabs.data_mut::<TabState>(id)
                                && let Some(task) = tab.servo.handle_dialog_control(control)
                            {
                                return task
                                    .map(|msg| libcosmic::action::app(Message::Dialog(msg)));
                            }
                        }
                        None => {}
                    }
                }

                // Adopt engine-created WebViews as foreground tabs, matching
                // servoshell: the most recent popup becomes the active tab.
                if !adopted.is_empty() {
                    let mut new_id = None;
                    for servo in adopted {
                        servo.set_theme(servo_theme());
                        let url_bar_text = servo
                            .current_url()
                            .map(|u| u.to_string())
                            .unwrap_or_default();
                        let tab = TabState::adopt(servo, url_bar_text);
                        let id = self.tabs.insert().closable().text("New Tab").data(tab).id();
                        if let Some(tab) = self.tabs.data::<TabState>(id) {
                            self.apply_filter_to_tab(tab);
                        }
                        new_id = Some(id);
                    }
                    if let Some(id) = new_id {
                        self.tabs.activate(id);
                        self.sync_tab_visibility();
                    }
                }

                // Remove tabs whose page requested close (window.close()).
                // `close_tab` keeps the last tab, so a page can never quit the app.
                if !closed_ids.is_empty() {
                    for id in closed_ids {
                        self.close_tab(id);
                    }
                    self.sync_tab_visibility();
                    self.sync_active_title();
                }

                // A freshly cached engine favicon may belong to a New Tab grid site.
                if favicon_saved {
                    self.rebuild_tile_images();
                }

                // Mirror the active tab's media state to the desktop (MPRIS).
                self.publish_media();

                let mut tasks = Vec::new();

                // Log the active tab's page once it has finished loading. Gating
                // on Complete (not mid-load) and on real http(s) URLs keeps
                // about: pages and partially-loaded pages out of history. Only
                // fires when the URL changes so repeated wakes don't spam the DB.
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && tab.servo.load_status() == cosmic_webview::LoadStatus::Complete
                    && let Some(url) = tab.servo.current_url()
                    && is_loggable_url(&url)
                {
                    let url_str = url.to_string();
                    if tab.logged_url.as_deref() != Some(&url_str)
                        && let Some(conn) = self.db.clone()
                    {
                        let title = tab.servo.title().unwrap_or_default();
                        tab.logged_url = Some(url_str.clone());
                        tasks.push(Task::perform(
                            async move {
                                if let Err(e) = crate::db::log_visit(conn, url_str, title).await {
                                    tracing::warn!("History log failed: {e}");
                                }
                            },
                            |_| libcosmic::action::app(Message::VisitLogged),
                        ));
                    }
                }

                // Capture a thumbnail + favicon once the active page is loaded.
                tasks.push(self.maybe_capture_active_thumbnail());

                return Task::batch(tasks);
            }

            Message::Input(event) => {
                // Do NOT clear suggestion_query here: mouse-move events over the
                // webview underneath the popup would close the popup on hover.
                // The popup closes on navigation, Escape, or suggestion click.
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.servo.send_input(event);
                }
            }

            Message::UrlBarFocused => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.url_bar_focused = true;
                    let text = tab.url_bar_text.trim().to_owned();
                    if !text.is_empty() {
                        self.suggestion_query = text.clone();
                        if let Some(conn) = self.db.clone() {
                            let engine = self.config.search_engine.clone();
                            let text2 = text.clone();
                            let db_task = Task::perform(
                                async move { crate::db::query_suggestions(conn, text, 6).await },
                                |result| match result {
                                    Ok(s) => libcosmic::action::app(Message::SuggestionsReady(s)),
                                    Err(e) => {
                                        tracing::warn!("Suggestions query failed: {e}");
                                        libcosmic::action::app(Message::SuggestionsReady(vec![]))
                                    }
                                },
                            );
                            let http_task = Task::perform(
                                async move { crate::url_bar::fetch_suggestions(engine, text2).await },
                                |s| libcosmic::action::app(Message::EngineSuggestionsReady(s)),
                            );
                            return Task::batch(vec![db_task, http_task]);
                        }
                    }
                }
            }

            Message::UrlBarUnfocused => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.url_bar_focused = false;
                    // Do NOT clear suggestions here: the unfocus fires on mouse-press
                    // before a suggestion click is processed, so clearing now would
                    // swallow the click. Suggestions are cleared on navigate or Escape.
                }
            }

            Message::UrlBarChanged(text) => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.url_bar_text = text.clone();
                }
                self.suggestion_index = None;
                self.suggestion_query = text.clone();
                if text.trim().is_empty() {
                    self.suggestions.clear();
                    return Task::none();
                }
                if let Some(conn) = self.db.clone() {
                    let engine = self.config.search_engine.clone();
                    let text2 = text.clone();
                    let db_task = Task::perform(
                        async move { crate::db::query_suggestions(conn, text, 6).await },
                        |result| match result {
                            Ok(s) => libcosmic::action::app(Message::SuggestionsReady(s)),
                            Err(e) => {
                                tracing::warn!("Suggestions query failed: {e}");
                                libcosmic::action::app(Message::SuggestionsReady(vec![]))
                            }
                        },
                    );
                    let http_task = Task::perform(
                        async move { crate::url_bar::fetch_suggestions(engine, text2).await },
                        |s| libcosmic::action::app(Message::EngineSuggestionsReady(s)),
                    );
                    return Task::batch(vec![db_task, http_task]);
                }
            }

            Message::Navigate => {
                // If a suggestion is selected, put its text in the URL bar first
                // so try_navigate() can handle both full URLs and search terms.
                if let Some(i) = self.suggestion_index
                    && let Some(s) = self.suggestions.get(i)
                {
                    let text = s.url.clone();
                    if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                        tab.url_bar_text = text;
                    }
                }
                self.suggestions.clear();
                self.suggestion_index = None;
                self.suggestion_query.clear();
                return self.try_navigate();
            }

            Message::GoBack => {
                self.dismiss_context_menu();
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.servo.go_back();
                }
            }

            Message::GoForward => {
                self.dismiss_context_menu();
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.servo.go_forward();
                }
            }

            Message::Reload => {
                self.dismiss_context_menu();
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.servo.reload();
                }
            }

            Message::StopLoading => {
                // The Servo embedding API does not yet expose a stop method.
            }

            Message::CopySelection => {
                self.dismiss_context_menu();
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.servo
                        .send_input(InputEvent::EditingAction(EditingActionEvent::Copy));
                }
            }

            Message::Paste => {
                self.dismiss_context_menu();
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.servo
                        .send_input(InputEvent::EditingAction(EditingActionEvent::Paste));
                }
            }

            Message::ContextMenuSelected(action) => {
                if let Some(menu) = self.context_menu.take() {
                    menu.select(action);
                }
                self.context_items.clear();
            }

            Message::Dialog(msg) => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.servo.update_dialog(msg);
                }
            }

            // ── Awesomebar ────────────────────────────────────────────────────
            Message::SuggestionsReady(mut suggestions) => {
                // Collect query texts already shown as SavedQuery entries.
                let saved_queries: HashSet<String> = suggestions
                    .iter()
                    .filter(|s| s.kind == SuggestionKind::SavedQuery)
                    .map(|s| s.url.clone())
                    .collect();

                // Drop History entries whose URL is a known search engine results
                // page for a query already covered by a SavedQuery, to avoid
                // showing both "kalle tjo" and the raw DDG/Qwant URL for the same search.
                // Check all built-in engines plus the current engine (covers Custom).
                if !saved_queries.is_empty() {
                    let mut engines: Vec<SearchEngine> = BUILTIN_ENGINES.to_vec();
                    if matches!(self.config.search_engine, SearchEngine::Custom(_)) {
                        engines.push(self.config.search_engine.clone());
                    }
                    suggestions.retain(|s| {
                        if s.kind != SuggestionKind::History {
                            return true;
                        }
                        match extract_search_query(&s.url, &engines) {
                            Some(q) => !saved_queries.contains(&q),
                            None => true,
                        }
                    });
                }

                self.suggestions = suggestions;
                self.suggestion_index = None;
            }

            Message::EngineSuggestionsReady(engine_results) => {
                let seen: HashSet<String> =
                    self.suggestions.iter().map(|s| s.url.clone()).collect();
                for s in engine_results {
                    if !seen.contains(&s.url) {
                        self.suggestions.push(s);
                    }
                }
            }

            Message::SuggestionNext => {
                let focused = self
                    .tabs
                    .active_data::<TabState>()
                    .map(|t| t.url_bar_focused)
                    .unwrap_or(false);
                if !focused || self.suggestions.is_empty() {
                    return Task::none();
                }
                let new_idx = match self.suggestion_index {
                    None => {
                        if let Some(tab) = self.tabs.active_data::<TabState>() {
                            self.suggestion_query = tab.url_bar_text.clone();
                        }
                        Some(0)
                    }
                    Some(i) if i + 1 < self.suggestions.len() => Some(i + 1),
                    Some(i) => Some(i),
                };
                self.suggestion_index = new_idx;
                if let Some(i) = new_idx
                    && let Some(tab) = self.tabs.active_data_mut::<TabState>()
                {
                    tab.url_bar_text = self.suggestions[i].url.clone();
                }
            }

            Message::SuggestionPrev => {
                let focused = self
                    .tabs
                    .active_data::<TabState>()
                    .map(|t| t.url_bar_focused)
                    .unwrap_or(false);
                if !focused || self.suggestions.is_empty() {
                    return Task::none();
                }
                match self.suggestion_index {
                    None | Some(0) => {
                        self.suggestion_index = None;
                        let query = self.suggestion_query.clone();
                        if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                            tab.url_bar_text = query;
                        }
                    }
                    Some(i) => {
                        self.suggestion_index = Some(i - 1);
                        let url = self.suggestions[i - 1].url.clone();
                        if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                            tab.url_bar_text = url;
                        }
                    }
                }
            }

            Message::SuggestionActivated(i) => {
                // Copy the suggestion text into the URL bar, then let
                // try_navigate handle both full URLs and plain search terms.
                if let Some(s) = self.suggestions.get(i) {
                    let text = s.url.clone();
                    if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                        tab.url_bar_text = text;
                    }
                }
                self.suggestions.clear();
                self.suggestion_index = None;
                self.suggestion_query.clear();
                return self.try_navigate();
            }

            Message::SearchWithEngine(engine) => {
                let query = if !self.suggestion_query.is_empty() {
                    self.suggestion_query.clone()
                } else {
                    self.tabs
                        .active_data::<TabState>()
                        .map(|t| t.url_bar_text.clone())
                        .unwrap_or_default()
                };
                let url_str = engine.search_url(&query);
                if let Ok(url) = Url::parse(&url_str)
                    && let Some(tab) = self.tabs.active_data_mut::<TabState>()
                {
                    tab.url_bar_text = url_str;
                    tab.show_new_tab_page = false;
                    tab.servo.navigate(url);
                }
                self.suggestions.clear();
                self.suggestion_index = None;
                self.suggestion_query.clear();
                if let Some(conn) = self.db.clone() {
                    return Task::perform(
                        async move {
                            if let Err(e) = crate::db::log_search_query(conn, query).await {
                                tracing::warn!("Search query log failed: {e}");
                            }
                        },
                        |_| libcosmic::action::app(Message::VisitLogged),
                    );
                }
            }

            Message::WebViewResized(w, h) => {
                self.current_size = PhysicalSize::new(w, h);
                // All tabs share the same display area.
                let ids: Vec<_> = self.tabs.iter().collect();
                for id in ids {
                    if let Some(tab) = self.tabs.data_mut::<TabState>(id) {
                        tab.servo.resize(PhysicalSize::new(w, h));
                    }
                }
            }

            Message::ScaleChanged(factor) => {
                self.scale = factor;
                let ids: Vec<_> = self.tabs.iter().collect();
                for id in ids {
                    if let Some(tab) = self.tabs.data_mut::<TabState>(id) {
                        tab.servo.set_scale_factor(factor);
                    }
                }
            }

            Message::ModifierKey(event) => {
                // Keep the engine's global modifier state correct even when the
                // release lands while a non-webview page is active. Only bare
                // modifier keys are forwarded (send_modifier_key ignores the rest),
                // so this never leaks typed content.
                if let Some(tab) = self.tabs.active_data::<TabState>() {
                    tab.servo.send_modifier_key(&event);
                }
            }

            Message::TabActivated(id) => {
                self.tabs.activate(id);
                self.sync_tab_visibility();
                self.dismiss_context_menu();
                self.sync_active_title();
            }

            Message::MediaAction(action) => {
                if let Some(tab) = self.tabs.active_data::<TabState>() {
                    tab.servo.send_media_action(action);
                }
            }

            Message::DevtoolsRespond(allow) => {
                if let Some(request) = self.pending_devtools.take() {
                    if allow {
                        request.allow();
                    } else {
                        request.deny();
                    }
                }
            }

            Message::TabClosed(id) => {
                self.close_tab(id);
                self.sync_tab_visibility();
                self.sync_active_title();
            }

            Message::TabNew => {
                let (url, show_new_tab_page) = resolve_page(&self.config.new_tab_page);
                let id = self.add_tab(url, show_new_tab_page);
                self.tabs.activate(id);
                self.sync_tab_visibility();
                self.dismiss_context_menu();
                // Focus the URL bar so the user can type an address immediately.
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.url_bar_focused = true;
                }
                return Task::batch(vec![
                    widget::text_input::focus(url_bar::URL_BAR_ID.clone()),
                    self.reload_new_tab(),
                ]);
            }

            Message::TabCloseActive => {
                self.close_tab(self.tabs.active());
                self.sync_tab_visibility();
                self.sync_active_title();
            }

            Message::TabNext => self.cycle_tab(true),
            Message::TabPrev => self.cycle_tab(false),

            Message::OpenLinkInNewTab(url) => match Url::parse(&url) {
                Ok(url) => {
                    // Open in the background, like Firefox — do not switch tabs.
                    self.add_tab(url, false);
                    self.sync_tab_visibility();
                }
                Err(e) => tracing::warn!("Ignoring middle-click link {url:?}: {e}"),
            },

            // ── New Tab page ──────────────────────────────────────────────────
            Message::NavigateTo(url) => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Ok(parsed) = Url::parse(&url)
                {
                    tab.url_bar_text = url;
                    tab.show_new_tab_page = false;
                    tab.servo.navigate(parsed);
                }
            }

            Message::TogglePinnedSite { url, title } => {
                if let Some(conn) = self.db.clone() {
                    let is_pinned = self.pinned_sites.iter().any(|b| b.url == url);
                    return Task::perform(
                        async move {
                            if is_pinned {
                                crate::db::remove_pinned(conn, url).await
                            } else {
                                crate::db::add_pinned(conn, url, title).await
                            }
                        },
                        |result| {
                            if let Err(e) = &result {
                                tracing::warn!("Toggle pinned site failed: {e}");
                            }
                            libcosmic::action::app(Message::PinnedChanged)
                        },
                    );
                }
            }

            Message::PinnedChanged => return self.reload_new_tab(),

            Message::TopSitesLoaded(entries) => {
                self.top_sites = entries;
                self.rebuild_tile_images();
                return self.fetch_missing_favicons();
            }

            Message::PinnedLoaded(entries) => {
                self.pinned_sites = entries;
                self.rebuild_tile_images();
                return self.fetch_missing_favicons();
            }

            Message::TileImagesChanged => {
                self.rebuild_tile_images();
            }

            Message::TileHovered(url) => self.hovered_tile = Some(url),
            Message::TileUnhovered => self.hovered_tile = None,

            // ── Settings ──────────────────────────────────────────────────────
            Message::SettingsOpen => {
                self.refresh_site_data_domains();
                self.open_settings_tab();
            }

            Message::SettingsSectionSelected(entity) => {
                self.settings_nav.activate(entity);
                // Re-scan so the User content section reflects files added on disk.
                self.refresh_user_scripts();
            }

            Message::ToggleUserScript(name) => {
                self.write_config(|c, h| {
                    let mut disabled = c.disabled_userscripts.clone();
                    if let Some(pos) = disabled.iter().position(|n| n == &name) {
                        disabled.remove(pos);
                    } else {
                        disabled.push(name.clone());
                    }
                    c.set_disabled_userscripts(h, disabled)
                });
                self.refresh_user_scripts();
                self.apply_user_content();
            }

            Message::RemoveUserScript(name) => {
                crate::user_content::remove_script(&name);
                // Drop any stale disabled entry for the removed file.
                self.write_config(|c, h| {
                    let mut disabled = c.disabled_userscripts.clone();
                    disabled.retain(|n| n != &name);
                    c.set_disabled_userscripts(h, disabled)
                });
                self.refresh_user_scripts();
                self.apply_user_content();
            }

            Message::OpenUserscriptsFolder => {
                crate::user_content::open_scripts_dir();
            }

            Message::RefreshUserContent => {
                self.refresh_user_scripts();
                self.apply_user_content();
            }

            Message::ToggleUserStylesheet(on) => {
                self.write_config(|c, h| c.set_user_stylesheet_enabled(h, on));
                self.apply_user_content();
            }

            Message::OpenUserStylesheet => {
                crate::user_content::open_stylesheet();
            }

            Message::SetStartupPageKind(index) => {
                let page = match index {
                    // Preserve any existing URL when re-selecting "Fixed URL".
                    1 => StartupPage::Url(match &self.config.startup_page {
                        StartupPage::Url(u) => u.clone(),
                        _ => String::from("https://"),
                    }),
                    2 => StartupPage::SearchEngine,
                    _ => StartupPage::NewTab,
                };
                self.write_config(|c, h| c.set_startup_page(h, page));
            }

            Message::SetStartupUrl(url) => {
                self.write_config(|c, h| c.set_startup_page(h, StartupPage::Url(url)));
            }

            Message::SetSearchEngine(engine) => {
                self.write_config(|c, h| c.set_search_engine(h, engine));
            }

            Message::SetCustomSearchUrl(url) => {
                self.write_config(|c, h| c.set_search_engine(h, SearchEngine::Custom(url)));
            }

            Message::SetNewTabColumns(n) => {
                self.write_config(|c, h| c.set_new_tab_columns(h, n));
            }

            Message::SetNewTabRows(n) => {
                self.write_config(|c, h| c.set_new_tab_rows(h, n));
            }

            Message::SetNewTabPageKind(index) => {
                let page = match index {
                    // Preserve any existing URL when re-selecting "Fixed URL".
                    1 => StartupPage::Url(match &self.config.new_tab_page {
                        StartupPage::Url(u) => u.clone(),
                        _ => String::from("https://"),
                    }),
                    2 => StartupPage::SearchEngine,
                    _ => StartupPage::NewTab,
                };
                self.write_config(|c, h| c.set_new_tab_page(h, page));
            }

            Message::SetNewTabUrl(url) => {
                self.write_config(|c, h| c.set_new_tab_page(h, StartupPage::Url(url)));
            }
            Message::OpenNewTabSettings => {
                self.context_page = ContextPage::NewTabSettings;
                self.core_mut().set_show_context(true);
            }
            Message::CloseNewTabSettings => {
                self.core_mut().set_show_context(false);
            }

            // ── Zoom ──────────────────────────────────────────────────────────
            Message::ZoomIn => self.adjust_zoom(ZOOM_STEP),
            Message::ZoomOut => self.adjust_zoom(-ZOOM_STEP),
            Message::ZoomReset => {
                if let Some(tab) = self.tabs.active_data::<TabState>() {
                    tab.servo.set_page_zoom(1.0);
                }
            }

            Message::FocusUrlBar => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.url_bar_focused = true;
                }
                return widget::text_input::focus(url_bar::URL_BAR_ID.clone());
            }

            // ── URL-bar indicators ──────────────────────────────────────────────
            Message::ToggleUrlBarPopover(which) => {
                self.url_bar_popover = if self.url_bar_popover == Some(which) {
                    None
                } else {
                    Some(which)
                };
            }
            Message::CloseUrlBarPopover => self.url_bar_popover = None,
            Message::SetCookieProtection(level) => {
                self.write_config(|c, h| c.set_cookie_protection(h, level));
                // Rebuild and re-apply filter for all tabs.
                return self.build_filter_engines();
            }
            Message::SetCookieProtectionOverride(override_level) => {
                self.url_bar_popover = None;
                let domain = self
                    .tabs
                    .active_data::<TabState>()
                    .and_then(|t| t.servo.current_url())
                    .and_then(|u| u.host_str().map(|h| h.to_owned()));

                // Effective level: override if set, otherwise global default.
                let effective = override_level.unwrap_or(self.config.cookie_protection);
                let filter = self.filter_engines.as_ref().map(|e| e.filter_fn(effective));
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.cookie_protection_override = override_level;
                    tab.servo.set_content_filter(filter);
                }

                // Persist to DB: save if override is set, delete if reset.
                if let (Some(domain), Some(conn)) = (domain, self.db.clone()) {
                    return Task::perform(
                        async move {
                            let result = match override_level {
                                Some(level) => {
                                    crate::db::save_site_setting(conn, domain, level).await
                                }
                                None => crate::db::delete_site_setting(conn, domain).await,
                            };
                            if let Err(e) = result {
                                tracing::warn!("Failed to update site setting: {e}");
                            }
                        },
                        |_| libcosmic::action::app(Message::VisitLogged),
                    );
                }
            }
            // ── Data clearing ───────────────────────────────────────────────────
            Message::SetClearRange(range) => self.clear_range = range,
            Message::ToggleClearHistory(on) => self.clear_history_checked = on,
            Message::ToggleClearCookies(on) => self.clear_cookies_checked = on,
            Message::RequestClearData => self.clear_confirm_open = true,
            Message::CancelClearData => self.clear_confirm_open = false,
            Message::ClearBrowsingData => {
                self.clear_confirm_open = false;
                return self.clear_browsing_data();
            }
            Message::ClearSiteData(domain) => return self.clear_site(domain),

            Message::PermissionRespond(response) => {
                // Answer the engine, then record the choice (capture domain +
                // feature inside the borrow, act afterwards).
                let mut answered = None;
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Some(request) = tab.pending_permission.take()
                {
                    let feature = request.feature();
                    match response {
                        PermissionResponse::Block | PermissionResponse::DenySession => {
                            request.deny()
                        }
                        PermissionResponse::AllowSession | PermissionResponse::AllowAlways => {
                            request.allow()
                        }
                    }
                    answered = current_domain(tab).map(|d| (d, feature));
                }
                let Some((domain, feature)) = answered else {
                    return Task::none();
                };
                let key = permission::feature_key(feature).to_owned();
                match response {
                    // Persistent choices are stored; session choices stay in memory.
                    PermissionResponse::AllowAlways => {
                        return self.persist_permission(domain, feature, true);
                    }
                    PermissionResponse::Block => {
                        return self.persist_permission(domain, feature, false);
                    }
                    PermissionResponse::AllowSession => {
                        self.session_permissions.insert((domain, key), true);
                    }
                    PermissionResponse::DenySession => {
                        self.session_permissions.insert((domain, key), false);
                    }
                }
            }
            Message::PermissionClear { key, persistent } => {
                self.url_bar_popover = None;
                let domain = self.tabs.active_data::<TabState>().and_then(current_domain);
                if let Some(domain) = domain {
                    if persistent {
                        return self.forget_permission_key(domain, key);
                    }
                    self.session_permissions.remove(&(domain, key));
                }
            }
            Message::SitePermissionsLoaded(entries) => {
                self.site_permissions = entries
                    .into_iter()
                    .map(|e| ((e.domain, e.feature), e.decision == "allow"))
                    .collect();
                self.rebuild_permission_grants();
            }
            Message::SetPermissionDefault(kind, value) => {
                self.write_config(|c, h| {
                    let mut defaults = c.permission_defaults;
                    defaults.set(kind, value);
                    c.set_permission_defaults(h, defaults)
                });
            }
            Message::RevokeSitePermission(domain, key) => {
                return self.forget_permission_key(domain, key);
            }
            Message::SetAllowCameraMicrophone(on) => {
                self.write_config(|c, h| c.set_allow_camera_microphone(h, on));
                // Update the running engine; takes effect on the next page load.
                cosmic_webview::set_camera_microphone_enabled(on);
            }
            Message::SetWebFontSerif(family) => {
                self.write_config(|c, h| c.set_web_font_serif(h, family));
                self.apply_web_fonts();
            }
            Message::SetWebFontSansSerif(family) => {
                self.write_config(|c, h| c.set_web_font_sans_serif(h, family));
                self.apply_web_fonts();
            }
            Message::SetWebFontMonospace(family) => {
                self.write_config(|c, h| c.set_web_font_monospace(h, family));
                self.apply_web_fonts();
            }
            Message::SetWebFontSize(size) => {
                self.write_config(|c, h| c.set_web_font_size(h, size));
                self.apply_web_fonts();
                self.reload_active_flag_others();
            }
            Message::SetWebFontMonospaceSize(size) => {
                self.write_config(|c, h| c.set_web_font_monospace_size(h, size));
                self.apply_web_fonts();
                self.reload_active_flag_others();
            }
            Message::ApplyWebFonts => self.reload_active_flag_others(),
            Message::BluetoothPick(index) => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Some(request) = tab.pending_bluetooth.take()
                {
                    // Clone the chosen device so the borrow ends before the
                    // consuming `pick_device` call.
                    match request.devices().get(index).cloned() {
                        Some(device) => {
                            let _ = request.pick_device(&device);
                        }
                        None => {
                            let _ = request.cancel();
                        }
                    }
                }
            }
            Message::BluetoothCancel => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Some(request) = tab.pending_bluetooth.take()
                {
                    let _ = request.cancel();
                }
            }
            Message::ResetAllSitePermissions => {
                self.site_permissions.clear();
                self.permission_grants.clear();
                if let Some(conn) = self.db.clone() {
                    return Task::perform(
                        async move {
                            if let Err(e) = crate::db::clear_all_site_permissions(conn).await {
                                tracing::warn!("Failed to clear site permissions: {e}");
                            }
                        },
                        |_| libcosmic::action::app(Message::VisitLogged),
                    );
                }
            }

            // ── Filter engine ─────────────────────────────────────────────────
            Message::FilterEnginesReady(engines) => {
                self.filter_engines = Some(engines);
                self.apply_filter_to_all_tabs();
            }
            Message::FilterEnginesFailed(reason) => {
                tracing::warn!("Content filter engine build failed: {reason}");
            }
            Message::FilterOverrideLoaded(level) => {
                let effective = level.unwrap_or(self.config.cookie_protection);
                let filter = self.filter_engines.as_ref().map(|e| e.filter_fn(effective));
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.cookie_protection_override = level;
                    if let Some(f) = filter {
                        tab.servo.set_content_filter(Some(f));
                    }
                }
            }
            Message::CustomFilterListUrlChanged(url) => {
                self.custom_filter_list_url = url;
            }
            Message::AddCustomFilterList(url) => {
                let url = url.trim().to_owned();
                if url.is_empty() {
                    return Task::none();
                }
                let label = url
                    .split('/')
                    .next_back()
                    .unwrap_or("custom")
                    .trim_end_matches(".txt")
                    .to_owned();
                let entry = FilterListEntry {
                    label,
                    url,
                    enabled: true,
                };
                self.custom_filter_list_url.clear();
                self.write_config(|c, h| {
                    let mut new_lists = c.custom_filter_lists.clone();
                    new_lists.push(entry);
                    c.set_custom_filter_lists(h, new_lists)
                });
                return self.build_filter_engines();
            }
            Message::RemoveCustomFilterList(index) => {
                self.write_config(|c, h| {
                    let mut new_lists = c.custom_filter_lists.clone();
                    if index < new_lists.len() {
                        new_lists.remove(index);
                    }
                    c.set_custom_filter_lists(h, new_lists)
                });
                return self.build_filter_engines();
            }
            Message::ToggleCustomFilterList(index) => {
                self.write_config(|c, h| {
                    let mut new_lists = c.custom_filter_lists.clone();
                    if let Some(entry) = new_lists.get_mut(index) {
                        entry.enabled = !entry.enabled;
                    }
                    c.set_custom_filter_lists(h, new_lists)
                });
                return self.build_filter_engines();
            }
            // ── Config ────────────────────────────────────────────────────────
            Message::ConfigChanged(config) => {
                self.config = config;
            }

            // ── Database ──────────────────────────────────────────────────────
            Message::DatabaseReady(conn) => {
                self.db = Some(conn.clone());
                let init_bookmarks = Task::perform(
                    {
                        let conn = conn.clone();
                        async move { crate::db::load_bookmarked_urls(conn).await }
                    },
                    |result| match result {
                        Ok(urls) => libcosmic::action::app(Message::BookmarksInitialized(urls)),
                        Err(e) => {
                            tracing::warn!("Failed to load bookmarked URLs: {e}");
                            libcosmic::action::app(Message::BookmarksInitialized(HashSet::new()))
                        }
                    },
                );
                let init_permissions = Task::perform(
                    async move { crate::db::query_site_permissions(conn).await },
                    |result| match result {
                        Ok(entries) => {
                            libcosmic::action::app(Message::SitePermissionsLoaded(entries))
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load site permissions: {e}");
                            libcosmic::action::app(Message::SitePermissionsLoaded(vec![]))
                        }
                    },
                );
                return Task::batch(vec![
                    init_bookmarks,
                    init_permissions,
                    self.reload_new_tab(),
                ]);
            }

            Message::DatabaseFailed(reason) => {
                tracing::error!("Database failed to open: {reason}");
            }

            // ── Bookmarks ─────────────────────────────────────────────────────
            Message::BookmarkOpen => {
                if self.core().window.show_context && self.context_page == ContextPage::Bookmarks {
                    self.core_mut().set_show_context(false);
                } else {
                    self.context_page = ContextPage::Bookmarks;
                    self.core_mut().set_show_context(true);
                    return self.reload_bookmarks();
                }
            }

            Message::BookmarkClose => {
                self.core_mut().set_show_context(false);
            }

            Message::BookmarkLoaded(entries) => {
                self.bookmarks = entries;
            }

            Message::BookmarkFilterChanged(filter) => {
                self.bookmark_filter = filter;
                return self.reload_bookmarks();
            }

            Message::BookmarkNavigate(url) => {
                self.core_mut().set_show_context(false);
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Ok(parsed) = Url::parse(&url)
                {
                    tab.url_bar_text = url;
                    tab.show_new_tab_page = false;
                    tab.servo.navigate(parsed);
                }
            }

            Message::BookmarkToggle => {
                if let Some(tab) = self.tabs.active_data::<TabState>()
                    && let Some(url) = tab.servo.current_url()
                    && let Some(conn) = self.db.clone()
                {
                    let url_str = url.to_string();
                    let title = tab.servo.title().unwrap_or_default();
                    if self.bookmarked_urls.contains(&url_str) {
                        return Task::perform(
                            async move {
                                crate::db::remove_bookmark(conn, url_str.clone())
                                    .await
                                    .map(|_| url_str)
                            },
                            |result| match result {
                                Ok(url) => libcosmic::action::app(Message::BookmarkToggled {
                                    url,
                                    is_bookmarked: false,
                                }),
                                Err(e) => {
                                    tracing::warn!("Remove bookmark failed: {e}");
                                    libcosmic::action::app(Message::VisitLogged)
                                }
                            },
                        );
                    } else {
                        return Task::perform(
                            async move {
                                crate::db::add_bookmark(conn, url_str.clone(), title)
                                    .await
                                    .map(|_| url_str)
                            },
                            |result| match result {
                                Ok(url) => libcosmic::action::app(Message::BookmarkToggled {
                                    url,
                                    is_bookmarked: true,
                                }),
                                Err(e) => {
                                    tracing::warn!("Add bookmark failed: {e}");
                                    libcosmic::action::app(Message::VisitLogged)
                                }
                            },
                        );
                    }
                }
            }

            Message::BookmarkToggled { url, is_bookmarked } => {
                if is_bookmarked {
                    self.bookmarked_urls.insert(url);
                } else {
                    self.bookmarked_urls.remove(&url);
                }
                if self.core().window.show_context && self.context_page == ContextPage::Bookmarks {
                    return self.reload_bookmarks();
                }
            }

            Message::BookmarkDeleteEntry(id) => {
                let url = self
                    .bookmarks
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.url.clone())
                    .unwrap_or_default();
                if let Some(conn) = self.db.clone() {
                    return Task::perform(
                        async move { crate::db::delete_bookmark_entry(conn, id).await },
                        move |result| {
                            if let Err(e) = &result {
                                tracing::warn!("Delete bookmark failed: {e}");
                            }
                            libcosmic::action::app(Message::BookmarkEntryDeleted {
                                id,
                                url: url.clone(),
                            })
                        },
                    );
                }
            }

            Message::BookmarkEntryDeleted { id, url } => {
                self.bookmarks.retain(|e| e.id != id);
                self.bookmarked_urls.remove(&url);
            }

            Message::BookmarksInitialized(urls) => {
                self.bookmarked_urls = urls;
            }

            // ── History ───────────────────────────────────────────────────────
            Message::HistoryOpen => {
                if self.core().window.show_context && self.context_page == ContextPage::History {
                    self.core_mut().set_show_context(false);
                } else {
                    self.context_page = ContextPage::History;
                    self.core_mut().set_show_context(true);
                    return self.reload_history();
                }
            }

            Message::HistoryLoaded(entries) => {
                self.history_entries = entries;
            }

            Message::HistoryFilterChanged(filter) => {
                self.history_filter = filter;
                return self.reload_history();
            }

            Message::HistorySortChanged(sort) => {
                self.history_sort = sort;
                return self.reload_history();
            }

            Message::HistoryNavigate(url) => {
                self.core_mut().set_show_context(false);
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Ok(parsed) = Url::parse(&url)
                {
                    tab.url_bar_text = url;
                    tab.show_new_tab_page = false;
                    tab.servo.navigate(parsed);
                }
            }

            Message::HistoryDeleteEntry(id) => {
                if let Some(conn) = self.db.clone() {
                    return Task::perform(
                        async move { crate::db::delete_history_entry(conn, id).await },
                        move |_| libcosmic::action::app(Message::HistoryEntryDeleted(id)),
                    );
                }
            }

            Message::HistoryEntryDeleted(id) => {
                self.history_entries.retain(|e| e.id != id);
            }

            Message::HistoryClose => {
                self.core_mut().set_show_context(false);
            }

            Message::VisitLogged => {}

            // ── Find in page ──────────────────────────────────────────────────
            Message::FindOpen => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    if tab.find_bar.is_none() {
                        tab.find_bar = Some(FindBarState::default());
                    }
                    return widget::text_input::focus(FIND_BAR_ID.clone());
                }
            }

            // Escape key: close find bar if open, otherwise close suggestions.
            Message::FindClose => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && tab.find_bar.is_some()
                {
                    tab.servo.find_clear();
                    tab.find_bar = None;
                    return Task::none();
                }
                // No find bar open — fall through to suggestion dismissal.
                if self.suggestion_index.is_some() {
                    let query = self.suggestion_query.clone();
                    if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                        tab.url_bar_text = query;
                    }
                }
                self.suggestions.clear();
                self.suggestion_index = None;
                self.suggestion_query.clear();
            }

            Message::FindBar(msg) => {
                let Some(tab) = self.tabs.active_data_mut::<TabState>() else {
                    return Task::none();
                };
                let Some(state) = tab.find_bar.as_mut() else {
                    return Task::none();
                };
                match msg {
                    FindBarMsg::QueryChanged(q) => {
                        state.query = q.clone();
                        let cs = state.case_sensitive;
                        let wrap = state.wrap;
                        return tab
                            .servo
                            .find(&q, cs, wrap)
                            .map(|r| libcosmic::action::app(Message::FindResult(r)));
                    }
                    FindBarMsg::Next => {
                        let wrap = state.wrap;
                        return tab
                            .servo
                            .find_next(wrap)
                            .map(|n| libcosmic::action::app(Message::FindNext(n)));
                    }
                    FindBarMsg::Prev => {
                        let wrap = state.wrap;
                        return tab
                            .servo
                            .find_prev(wrap)
                            .map(|n| libcosmic::action::app(Message::FindPrev(n)));
                    }
                    FindBarMsg::Close => {
                        tab.servo.find_clear();
                        tab.find_bar = None;
                    }
                    FindBarMsg::ToggleWrap => {
                        state.wrap = !state.wrap;
                        let q = state.query.clone();
                        let cs = state.case_sensitive;
                        let wrap = state.wrap;
                        return tab
                            .servo
                            .find(&q, cs, wrap)
                            .map(|r| libcosmic::action::app(Message::FindResult(r)));
                    }
                    FindBarMsg::ToggleCaseSensitive => {
                        state.case_sensitive = !state.case_sensitive;
                        let q = state.query.clone();
                        let cs = state.case_sensitive;
                        let wrap = state.wrap;
                        return tab
                            .servo
                            .find(&q, cs, wrap)
                            .map(|r| libcosmic::action::app(Message::FindResult(r)));
                    }
                }
            }

            Message::FindResult(result) => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Some(state) = tab.find_bar.as_mut()
                {
                    state.result = result;
                }
            }

            // JS returns the new 1-based index, or 0 when there are no matches.
            Message::FindNext(n) | Message::FindPrev(n) => {
                if n > 0
                    && let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Some(state) = tab.find_bar.as_mut()
                {
                    state.result.current = n;
                }
            }

            Message::InfoDialogClose => {
                self.info_dialog_message = None;
            }

            // ── Developer tools ────────────────────────────────────────────────
            Message::DevToolsInfo => {
                // Devtools are enabled at startup via BrowserConfig; show a
                // dialog so the user knows how to connect.
                if self.config.devtools_enabled {
                    let port = self.config.devtools_port;
                    return self.show_info_dialog(format!(
                        "DevTools server running on port {port}.\n\
                         Connect from Firefox: DevTools → Remote Debugging → \
                         Connect to localhost:{port}"
                    ));
                }
                return self.show_info_dialog(
                    "DevTools are disabled.\n\
                     Enable them in Settings → Developer Tools and restart."
                        .to_owned(),
                );
            }

            Message::SetDevToolsEnabled(on) => {
                self.write_config(|c, h| c.set_devtools_enabled(h, on));
            }

            Message::SetDevToolsPort(s) => {
                self.devtools_port_str = s.clone();
                if let Ok(port) = s.parse::<u16>() {
                    self.write_config(|c, h| c.set_devtools_port(h, port));
                }
            }
        }

        Task::none()
    }

    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<'_, Message>> {
        if !self.core().window.show_context {
            return None;
        }
        Some(match self.context_page {
            ContextPage::History => context_drawer::context_drawer(
                pages::history::view(
                    &self.history_entries,
                    &self.history_filter,
                    self.history_sort,
                ),
                Message::HistoryClose,
            )
            .title("History"),
            ContextPage::Bookmarks => context_drawer::context_drawer(
                pages::bookmarks::view(&self.bookmarks, &self.bookmark_filter),
                Message::BookmarkClose,
            )
            .title("Bookmarks"),
            ContextPage::NewTabSettings => context_drawer::context_drawer(
                pages::settings::new_tab_settings(&self.config),
                Message::CloseNewTabSettings,
            )
            .title("New Tab"),
        })
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        // Bookmarks, history, and settings moved into the toolbar overflow menu
        // (see `build_app_menu`) to free up header space for the tab bar.
        vec![]
    }

    fn header_center(&self) -> Vec<Element<'_, Message>> {
        let tab_bar = widget::tab_bar::horizontal(&self.tabs)
            .show_close_icon_on_hover(true)
            .on_activate(Message::TabActivated)
            .on_close(Message::TabClosed)
            // Gap between the favicon and the tab title (defaults to 0).
            .button_spacing(libcosmic::theme::spacing().space_xxs)
            .width(Length::Fill);

        let new_tab_btn = widget::button::icon(widget::icon::from_name("tab-new-symbolic"))
            .on_press(Message::TabNew);

        vec![tab_bar.into(), new_tab_btn.into()]
    }

    fn dialog(&self) -> Option<Element<'_, Message>> {
        // Info dialog (e.g. DevTools status).
        if let Some(msg) = &self.info_dialog_message {
            return Some(
                widget::dialog()
                    .title("Information")
                    .body(msg.as_str())
                    .primary_action(
                        widget::button::standard("OK").on_press(Message::InfoDialogClose),
                    )
                    .into(),
            );
        }

        // The clear-data confirmation takes priority over any page dialog.
        if self.clear_confirm_open {
            let dialog = widget::dialog()
                .title("Clear browsing data?")
                .body(
                    "This permanently deletes the selected browsing data. \
                     This can't be undone.",
                )
                .primary_action(
                    widget::button::destructive("Clear").on_press(Message::ClearBrowsingData),
                )
                .secondary_action(
                    widget::button::standard("Cancel").on_press(Message::CancelClearData),
                );
            return Some(dialog.into());
        }

        // A page may ask the user to choose a Bluetooth device.
        if let Some(tab) = self.tabs.active_data::<TabState>()
            && let Some(request) = &tab.pending_bluetooth
        {
            return Some(bluetooth_dialog(request.devices()));
        }

        self.tabs
            .active_data::<TabState>()?
            .servo
            .dialog_view()
            .map(|e| e.map(Message::Dialog))
    }

    fn view(&self) -> Element<'_, Message> {
        use cosmic_webview::LoadStatus;

        let Some(tab) = self.tabs.active_data::<TabState>() else {
            return widget::text("No active tab").into();
        };

        // A settings tab renders the settings page full-window, without the
        // address bar (it has no web page to navigate).
        if tab.kind == TabKind::Settings {
            return widget::container(pages::settings::view(
                &self.settings_nav,
                &self.config,
                &self.custom_filter_list_url,
                pages::settings::DataClearing {
                    range: self.clear_range,
                    history_checked: self.clear_history_checked,
                    cookies_checked: self.clear_cookies_checked,
                    site_domains: &self.site_data_domains,
                },
                pages::settings::Permissions {
                    defaults: &self.config.permission_defaults,
                    grants: &self.permission_grants,
                    allow_camera_microphone: self.config.allow_camera_microphone,
                },
                &self.devtools_port_str,
                &self.user_scripts,
            ))
            .height(Length::Fill)
            .into();
        }

        let (can_back, can_forward, is_loading) = (
            tab.servo.can_go_back(),
            tab.servo.can_go_forward(),
            tab.servo.load_status() != LoadStatus::Complete,
        );

        let back = widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
            .on_press_maybe(can_back.then_some(Message::GoBack));

        let forward = widget::button::icon(widget::icon::from_name("go-next-symbolic"))
            .on_press_maybe(can_forward.then_some(Message::GoForward));

        let reload_or_stop = if is_loading {
            widget::button::icon(widget::icon::from_name("process-stop-symbolic"))
                .on_press(Message::StopLoading)
        } else {
            widget::button::icon(widget::icon::from_name("view-refresh-symbolic"))
                .on_press(Message::Reload)
        };

        let current_url = tab
            .servo
            .current_url()
            .map(|u| u.to_string())
            .unwrap_or_default();

        let address_bar = url_bar::AddressBar {
            text: &tab.url_bar_text,
            security: SecurityStatus::from_url(&current_url),
            cookie_override: tab.cookie_protection_override,
            global_cookie_level: self.config.cookie_protection,
            permissions: self.permission_entries(),
            is_bookmarked: self.bookmarked_urls.contains(&current_url),
            zoom: tab.servo.page_zoom(),
            open_popover: self.url_bar_popover,
        }
        .view();

        let nav_row = widget::row::with_capacity(6)
            .align_y(libcosmic::iced::Alignment::Center)
            .push(back)
            .push(forward)
            .push(reload_or_stop)
            .push(address_bar)
            .push_maybe(is_loading.then(|| {
                widget::progress_bar::indeterminate_circular()
                    .size(24.0)
                    .bar_height(3.0)
            }))
            .push(build_app_menu());

        // Show the internal New Tab page instead of the webview when this tab
        // opened on it and has not navigated yet.
        let content_area: Element<'_, Message> = if tab.show_new_tab_page {
            widget::container(pages::new_tab::view(
                &self.top_sites,
                &self.pinned_sites,
                &self.tile_images,
                self.config.new_tab_columns,
                self.config.new_tab_rows,
                self.hovered_tile.as_deref(),
            ))
            .height(Length::Fill)
            .into()
        } else {
            let menu_items = if self.context_items.is_empty() {
                build_default_menu(can_back, can_forward)
            } else {
                build_servo_menu(&self.context_items)
            };

            let webview = WebViewWidget::new(tab.servo.frame())
                .scale(self.scale)
                .cursor(tab.servo.cursor())
                .hovered_link(tab.servo.hovered_link())
                .context_menu(menu_items)
                .on_input(Message::Input)
                .on_open_link(Message::OpenLinkInNewTab)
                .on_resize(Message::WebViewResized)
                .keyboard_enabled(!tab.url_bar_focused)
                .shortcut_filter(is_chrome_shortcut);

            widget::container(webview).height(Length::Fill).into()
        };

        // Always keep the popover in the widget tree so the text input's widget
        // ID stays stable and focus is not reset when suggestions appear.
        // A stable ID is required; the default is widget::Id::unique() which
        // changes every render and would reset focus state.
        let nav_area: Element<'_, Message> = {
            let p = widget::popover(nav_row)
                .id(widget::Id::new("url_bar_suggestions"))
                .position(widget::popover::Position::Bottom);
            // Show the popup whenever the user has typed something — the
            // engine-row is always useful even when there are no local matches.
            if !self.suggestion_query.is_empty() {
                p.popup(url_bar::suggestion_list(
                    &self.suggestions,
                    self.suggestion_index,
                    &self.config.search_engine,
                ))
                .into()
            } else {
                p.into()
            }
        };

        let mut root = widget::column::with_capacity(4).push(nav_area);
        // Show a permission prompt banner below the toolbar while one is pending.
        if let Some(request) = &tab.pending_permission {
            root = root.push(url_bar::permission_prompt(request.feature()));
        }
        // Show a remote-devtools connection prompt (engine-level, any tab).
        if self.pending_devtools.is_some() {
            root = root.push(url_bar::devtools_prompt());
        }
        // Show the find bar just above the content area when active.
        if let Some(find_state) = &tab.find_bar {
            root = root.push(FindBar::view(find_state).map(Message::FindBar));
        }
        root.push(content_area).into()
    }
}
