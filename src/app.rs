use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use cosmic_webview::{
    ContextMenu, ContextMenuAction, ContextMenuItem, DialogMsg, EditingActionEvent,
    EmbedderControl, InputEvent, PermissionFeature, WebViewWidget,
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
    BUILTIN_ENGINES, BrowserConfig, CookieProtectionLevel, SearchEngine, StartupPage,
    extract_search_query,
};
use crate::db::{BookmarkEntry, HistoryEntry, HistorySort, Suggestion, SuggestionKind};
use crate::pages;
use crate::tabs::TabState;
use crate::url_bar::{self, SecurityStatus, UrlBarPopover};
use cosmic_config::CosmicConfigEntry as _;

/// Page-zoom step applied per Ctrl+± press, and the clamp bounds.
const ZOOM_STEP: f32 = 0.1;
const ZOOM_MIN: f32 = 0.3;
const ZOOM_MAX: f32 = 5.0;

/// History rows fetched for the grid; trimmed to the visible cell count in `view`.
const NEW_TAB_TOP_SITES: usize = 48;

/// Compare two permission features by variant. `PermissionFeature` is `Copy`
/// but does not implement `PartialEq`, so we match on the discriminant.
fn same_feature(a: PermissionFeature, b: PermissionFeature) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
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

// ── Context page ─────────────────────────────────────────────────────────────

/// Which page is currently shown in the context drawer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ContextPage {
    #[default]
    History,
    Bookmarks,
    Settings,
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
    /// User activated (clicked) a tab.
    TabActivated(segmented_button::Entity),
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
    // ── Settings ────────────────────────────────────────────────────────────────
    /// Toggle the settings context drawer.
    SettingsOpen,
    /// Settings context drawer closed via its X button.
    SettingsClose,
    /// Set the New Tab grid column count.
    SetNewTabColumns(u16),
    /// Set the New Tab grid row count.
    SetNewTabRows(u16),
    /// Set what new tabs open, by dropdown index (New Tab / URL / Search engine).
    SetNewTabPageKind(usize),
    /// Set the fixed URL new tabs open when the "Fixed URL" kind is selected.
    SetNewTabUrl(String),
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
    SetCookieProtectionOverride(CookieProtectionLevel),
    /// Allow the active tab's pending permission request.
    PermissionAllow,
    /// Deny the active tab's pending permission request.
    PermissionDeny,
    /// Revoke a previously granted permission (permissions popover).
    PermissionRevoke(PermissionFeature),
    // ── Awesomebar ────────────────────────────────────────────────────────────
    /// Suggestion list loaded from history + bookmarks for the current input.
    SuggestionsReady(Vec<Suggestion>),
    /// User pressed ArrowDown to move to the next suggestion.
    SuggestionNext,
    /// User pressed ArrowUp to move to the previous suggestion (or restore input).
    SuggestionPrev,
    /// User clicked a suggestion at the given index.
    SuggestionActivated(usize),
    /// Close the suggestion list (Escape, webview click, or explicit dismiss).
    SuggestionsClose,
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
}

// ── Application state ─────────────────────────────────────────────────────────

pub struct App {
    core: Core,
    /// Tab model; each entity has associated [`TabState`] (including its URL bar).
    tabs: segmented_button::SingleSelectModel,
    /// Shared waker receiver: all tabs fire the same channel since they share one engine.
    servo_rx: async_channel::Receiver<()>,
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
    /// Best cached image (thumbnail, else favicon) per grid URL. Rebuilt when
    /// the grid data changes or a thumbnail/favicon finishes writing.
    tile_images: HashMap<String, PathBuf>,
    // ── URL-bar indicators ────────────────────────────────────────────────────
    /// Which URL-bar indicator popover is open, if any.
    url_bar_popover: Option<UrlBarPopover>,
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

    // Unmodified keys drive the suggestion popup.
    if !ctrl
        && !alt
        && let keyboard::Key::Named(named) = key
    {
        return match named {
            Named::Escape => Some(Message::SuggestionsClose),
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
/// Only modified combinations and F5 qualify; the unmodified suggestion keys
/// (arrows, Escape) must still reach the page when the webview has focus.
fn is_chrome_shortcut(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> bool {
    use keyboard::key::Named;
    let modified = modifiers.control() || modifiers.alt();
    let is_f5 = matches!(key, keyboard::Key::Named(Named::F5));
    (modified || is_f5) && shortcut_to_message(key, modifiers).is_some()
}

impl App {
    /// Parse the active tab's URL bar text, navigate, and unfocus the URL bar.
    ///
    /// Input that looks like a URL (has `://` or a dot-only domain) is treated
    /// as an address; everything else is forwarded to the configured search engine.
    /// Returns a Task that moves keyboard focus away from the URL bar to the page.
    fn try_navigate(&mut self) -> Task<Message> {
        let Some(tab) = self.tabs.active_data_mut::<TabState>() else {
            return Task::none();
        };
        let raw = tab.url_bar_text.trim().to_owned();
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
        tab.url_bar_text = url.to_string();
        tab.url_bar_focused = false;
        tab.show_new_tab_page = false;
        // A per-site cookie override only applies to the site it was set on.
        tab.cookie_protection_override = None;
        // Granted permissions and any unanswered request belong to the old page.
        tab.granted_permissions.clear();
        tab.pending_permission = None;
        tab.servo.navigate(url);

        let focus_task = libcosmic::iced::widget::operation::focus_next();

        if is_search && let Some(conn) = self.db.clone() {
            let log_task = Task::perform(
                async move {
                    if let Err(e) = crate::db::log_search_query(conn, raw).await {
                        tracing::warn!("Search query log failed: {e}");
                    }
                },
                |_| libcosmic::action::app(Message::VisitLogged),
            );
            return Task::batch(vec![focus_task, log_task]);
        }

        focus_task
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
            if let Some(tab) = self.tabs.data::<TabState>(id) {
                tab.servo.set_visible(is_active);
            }
        }
    }

    /// Create a new tab loading `url` and register it in the tab model.
    /// The caller is responsible for activating the returned entity.
    /// `show_new_tab_page` makes the tab open on the internal New Tab page.
    fn add_tab(&mut self, url: Url, show_new_tab_page: bool) -> segmented_button::Entity {
        let (mut tab, _rx) = TabState::new(url, self.current_size, self.scale);
        tab.show_new_tab_page = show_new_tab_page;
        self.tabs.insert().closable().text("New Tab").data(tab).id()
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
        let urls = self
            .pinned_sites
            .iter()
            .map(|b| b.url.clone())
            .chain(self.top_sites.iter().map(|h| h.url.clone()));

        let mut images = HashMap::new();
        for url in urls {
            let thumb = crate::thumbnails::thumbnail_path(&url);
            if thumb.exists() {
                images.insert(url, thumb);
                continue;
            }
            let favicon = crate::thumbnails::favicon_path(&url);
            if favicon.exists() {
                images.insert(url, favicon);
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

        let initial_size = PhysicalSize::new(1280u32, 800);
        // A command-line URL overrides the configured startup page.
        let (initial_url, show_new_tab_page) = match flags {
            Some(url) => (url, false),
            None => resolve_page(&config.startup_page),
        };

        let (mut first_tab, servo_rx) = TabState::new(initial_url, initial_size, scale);
        first_tab.show_new_tab_page = show_new_tab_page;

        let mut tabs = segmented_button::SingleSelectModel::default();
        let first_id = tabs
            .insert()
            .closable()
            .text("New Tab")
            .data(first_tab)
            .id();
        tabs.activate(first_id);

        let app = App {
            core,
            config,
            config_handle,
            context_page: ContextPage::default(),
            suggestions: Vec::new(),
            suggestion_index: None,
            suggestion_query: String::new(),
            tabs,
            servo_rx,
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
            url_bar_popover: None,
        };

        let db_task = Task::perform(
            async { crate::db::open(crate::db::db_path()).await },
            |result| match result {
                Ok(conn) => libcosmic::action::app(Message::DatabaseReady(conn)),
                Err(e) => libcosmic::action::app(Message::DatabaseFailed(e.to_string())),
            },
        );
        (app, db_task)
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

        // All tabs share one Servo engine and therefore one waker channel.
        let servo_sub = Subscription::run_with(ServoRx(self.servo_rx.clone()), |s| {
            s.0.clone().map(|_| Message::ServoWake)
        });

        let window_sub = event::listen_with(|event, _status, _id| match event {
            Event::Window(window::Event::Rescaled(factor)) => Some(Message::ScaleChanged(factor)),
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                shortcut_to_message(&key, modifiers)
            }
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

        Subscription::batch([servo_sub, window_sub, config_sub])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ServoWake => {
                // Spin the shared engine once — covers all pending WebView events.
                if let Some(tab) = self.tabs.active_data::<TabState>() {
                    tab.servo.spin();
                }

                // Process state changes for every tab.
                let ids: Vec<_> = self.tabs.iter().collect();
                for id in ids {
                    let is_active = self.tabs.is_active(id);

                    // Collect volatile state while holding the mutable borrow.
                    let (title, dialog, crash) = {
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

                        // A page may ask for a permission (geolocation, camera, …);
                        // hold it until the user answers via the prompt banner.
                        if let Some(request) = tab.servo.take_permission_request() {
                            tab.pending_permission = Some(request);
                        }

                        let title = tab.servo.title();

                        // No frame copying here: the engine fills the shared
                        // frame buffer directly and the widget uploads it on
                        // the next draw. See cosmic-webview's FrameSink.

                        let crash = tab.servo.take_crash();
                        (title, dialog, crash)
                    };

                    if let Some(title) = title {
                        self.tabs.text_set(id, title.clone());
                        if is_active {
                            self.set_header_title(title);
                        }
                    }

                    if let Some((reason, _)) = crash {
                        tracing::error!("WebView crashed: {reason}");
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

                let mut tasks = Vec::new();

                // Log URL changes for the active tab. Only fires when the URL
                // actually changes so repeated wake signals don't spam the DB.
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Some(url) = tab.servo.current_url()
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

            Message::SuggestionsClose => {
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

            Message::TabActivated(id) => {
                self.tabs.activate(id);
                self.sync_tab_visibility();
                self.dismiss_context_menu();
                self.sync_active_title();
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

            // ── Settings ──────────────────────────────────────────────────────
            Message::SettingsOpen => {
                if self.core().window.show_context && self.context_page == ContextPage::Settings {
                    self.core_mut().set_show_context(false);
                } else {
                    self.context_page = ContextPage::Settings;
                    self.core_mut().set_show_context(true);
                }
            }

            Message::SettingsClose => {
                self.core_mut().set_show_context(false);
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
            }
            Message::SetCookieProtectionOverride(level) => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.cookie_protection_override = Some(level);
                }
                self.url_bar_popover = None;
            }
            Message::PermissionAllow => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Some(request) = tab.pending_permission.take()
                {
                    let feature = request.feature();
                    if !tab
                        .granted_permissions
                        .iter()
                        .any(|g| same_feature(*g, feature))
                    {
                        tab.granted_permissions.push(feature);
                    }
                    request.allow();
                }
            }
            Message::PermissionDeny => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>()
                    && let Some(request) = tab.pending_permission.take()
                {
                    request.deny();
                }
            }
            Message::PermissionRevoke(feature) => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    // We can only drop our own record; Servo exposes no way to
                    // rescind a grant it already made.
                    tab.granted_permissions
                        .retain(|g| !same_feature(*g, feature));
                }
                self.url_bar_popover = None;
            }

            // ── Config ────────────────────────────────────────────────────────
            Message::ConfigChanged(config) => {
                self.config = config;
            }

            // ── Database ──────────────────────────────────────────────────────
            Message::DatabaseReady(conn) => {
                self.db = Some(conn.clone());
                let init_bookmarks = Task::perform(
                    async move { crate::db::load_bookmarked_urls(conn).await },
                    |result| match result {
                        Ok(urls) => libcosmic::action::app(Message::BookmarksInitialized(urls)),
                        Err(e) => {
                            tracing::warn!("Failed to load bookmarked URLs: {e}");
                            libcosmic::action::app(Message::BookmarksInitialized(HashSet::new()))
                        }
                    },
                );
                return Task::batch(vec![init_bookmarks, self.reload_new_tab()]);
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
            ContextPage::Settings => context_drawer::context_drawer(
                pages::settings::view(&self.config),
                Message::SettingsClose,
            )
            .title("Settings"),
        })
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        let bookmarks_btn =
            widget::button::icon(widget::icon::from_name("user-bookmarks-symbolic"))
                .on_press(Message::BookmarkOpen);
        let history_btn =
            widget::button::icon(widget::icon::from_name("document-open-recent-symbolic"))
                .on_press(Message::HistoryOpen);
        let settings_btn = widget::button::icon(widget::icon::from_name("emblem-system-symbolic"))
            .on_press(Message::SettingsOpen);
        vec![
            bookmarks_btn.into(),
            history_btn.into(),
            settings_btn.into(),
        ]
    }

    fn header_center(&self) -> Vec<Element<'_, Message>> {
        let tab_bar = widget::tab_bar::horizontal(&self.tabs)
            .show_close_icon_on_hover(true)
            .on_activate(Message::TabActivated)
            .on_close(Message::TabClosed)
            .width(Length::Fill);

        let new_tab_btn = widget::button::icon(widget::icon::from_name("tab-new-symbolic"))
            .on_press(Message::TabNew);

        vec![tab_bar.into(), new_tab_btn.into()]
    }

    fn dialog(&self) -> Option<Element<'_, Message>> {
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
            cookie_level: tab
                .cookie_protection_override
                .unwrap_or(self.config.cookie_protection),
            permissions: &tab.granted_permissions,
            is_bookmarked: self.bookmarked_urls.contains(&current_url),
            zoom: tab.servo.page_zoom(),
            open_popover: self.url_bar_popover,
        }
        .view();

        let nav_row = widget::row::with_capacity(4)
            .push(back)
            .push(forward)
            .push(reload_or_stop)
            .push(address_bar);

        // Show the internal New Tab page instead of the webview when this tab
        // opened on it and has not navigated yet.
        let content_area: Element<'_, Message> = if tab.show_new_tab_page {
            widget::container(pages::new_tab::view(
                &self.top_sites,
                &self.pinned_sites,
                &self.tile_images,
                self.config.new_tab_columns,
                self.config.new_tab_rows,
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

        let mut root = widget::column::with_capacity(3).push(nav_area);
        // Show a permission prompt banner below the toolbar while one is pending.
        if let Some(request) = &tab.pending_permission {
            root = root.push(url_bar::permission_prompt(request.feature()));
        }
        root.push(content_area).into()
    }
}
