use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use cosmic_webview::{
    ContextMenu, ContextMenuAction, ContextMenuItem, DialogMsg, EditingActionEvent,
    EmbedderControl, InputEvent, WebViewWidget,
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
    BUILTIN_ENGINES, BrowserConfig, SearchEngine, StartupPage, extract_search_query,
};
use crate::db::{BookmarkEntry, HistoryEntry, HistorySort, Suggestion, SuggestionKind};
use crate::pages;
use crate::tabs::TabState;
use crate::url_bar;
use cosmic_config::CosmicConfigEntry as _;

/// Stable widget id for the URL bar text input, so keyboard shortcuts can move
/// focus to it (Ctrl+L). Must stay constant across `view()` calls.
static URL_BAR_ID: std::sync::LazyLock<widget::Id> =
    std::sync::LazyLock::new(|| widget::Id::new("url_bar_input"));

/// Page-zoom step applied per Ctrl+± press, and the clamp bounds.
const ZOOM_STEP: f32 = 0.1;
const ZOOM_MIN: f32 = 0.3;
const ZOOM_MAX: f32 = 5.0;

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

// ── Context page ─────────────────────────────────────────────────────────────

/// Which page is currently shown in the context drawer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ContextPage {
    #[default]
    History,
    Bookmarks,
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
    // ── Zoom ──────────────────────────────────────────────────────────────────
    /// Increase the active tab's page zoom (Ctrl++).
    ZoomIn,
    /// Decrease the active tab's page zoom (Ctrl+-).
    ZoomOut,
    /// Reset the active tab's page zoom to 100 % (Ctrl+0).
    ZoomReset,
    /// Move keyboard focus to the URL bar and select its contents (Ctrl+L).
    FocusUrlBar,
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
    fn add_tab(&mut self, url: Url) -> segmented_button::Entity {
        let (tab, _rx) = TabState::new(url, self.current_size, self.scale);
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

    /// Return the URL to open in new tabs based on the current config.
    fn startup_url(&self) -> Url {
        match &self.config.startup_page {
            StartupPage::Url(u) => Url::parse(u).unwrap_or_else(|_| {
                Url::parse("about:blank").expect("about:blank is always valid")
            }),
            // SearchEngine and NewTab both show about:blank until Fas 6/7
            StartupPage::SearchEngine | StartupPage::NewTab => {
                Url::parse("about:blank").expect("about:blank is always valid")
            }
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

        let config = cosmic_config::Config::new(Self::APP_ID, BrowserConfig::VERSION)
            .map(|c| {
                BrowserConfig::get_entry(&c).unwrap_or_else(|(errs, c)| {
                    for e in errs {
                        tracing::warn!("Config load error: {e}");
                    }
                    c
                })
            })
            .unwrap_or_default();

        let initial_size = PhysicalSize::new(1280u32, 800);
        let initial_url = flags.unwrap_or_else(|| match &config.startup_page {
            StartupPage::Url(u) => Url::parse(u).unwrap_or_else(|_| {
                Url::parse("about:blank").expect("about:blank is always valid")
            }),
            StartupPage::SearchEngine | StartupPage::NewTab => {
                Url::parse("about:blank").expect("about:blank is always valid")
            }
        });

        let (first_tab, servo_rx) = TabState::new(initial_url, initial_size, scale);

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
                        return Task::perform(
                            async move {
                                if let Err(e) = crate::db::log_visit(conn, url_str, title).await {
                                    tracing::warn!("History log failed: {e}");
                                }
                            },
                            |_| libcosmic::action::app(Message::VisitLogged),
                        );
                    }
                }
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
                let url = self.startup_url();
                let id = self.add_tab(url);
                self.tabs.activate(id);
                self.sync_tab_visibility();
                self.dismiss_context_menu();
                // Focus the URL bar so the user can type an address immediately.
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.url_bar_focused = true;
                }
                return widget::text_input::focus(URL_BAR_ID.clone());
            }

            Message::TabCloseActive => {
                self.close_tab(self.tabs.active());
                self.sync_tab_visibility();
                self.sync_active_title();
            }

            Message::TabNext => self.cycle_tab(true),
            Message::TabPrev => self.cycle_tab(false),

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
                return widget::text_input::focus(URL_BAR_ID.clone());
            }

            // ── Config ────────────────────────────────────────────────────────
            Message::ConfigChanged(config) => {
                self.config = config;
            }

            // ── Database ──────────────────────────────────────────────────────
            Message::DatabaseReady(conn) => {
                self.db = Some(conn.clone());
                return Task::perform(
                    async move { crate::db::load_bookmarked_urls(conn).await },
                    |result| match result {
                        Ok(urls) => libcosmic::action::app(Message::BookmarksInitialized(urls)),
                        Err(e) => {
                            tracing::warn!("Failed to load bookmarked URLs: {e}");
                            libcosmic::action::app(Message::BookmarksInitialized(HashSet::new()))
                        }
                    },
                );
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
        })
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        let bookmarks_btn =
            widget::button::icon(widget::icon::from_name("user-bookmarks-symbolic"))
                .on_press(Message::BookmarkOpen);
        let history_btn =
            widget::button::icon(widget::icon::from_name("document-open-recent-symbolic"))
                .on_press(Message::HistoryOpen);
        vec![bookmarks_btn.into(), history_btn.into()]
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

        let url_bar = widget::text_input("https://example.com", &tab.url_bar_text)
            .id(URL_BAR_ID.clone())
            .on_focus(Message::UrlBarFocused)
            .on_unfocus(Message::UrlBarUnfocused)
            .on_input(Message::UrlBarChanged)
            .on_submit(|_| Message::Navigate)
            .select_on_focus(true)
            .width(Length::Fill);

        let current_url = tab
            .servo
            .current_url()
            .map(|u| u.to_string())
            .unwrap_or_default();
        let star_icon = if self.bookmarked_urls.contains(&current_url) {
            "starred-symbolic"
        } else {
            "non-starred-symbolic"
        };
        let bookmark_btn = widget::button::icon(widget::icon::from_name(star_icon))
            .on_press(Message::BookmarkToggle);

        let nav_row = widget::row::with_capacity(5)
            .push(back)
            .push(forward)
            .push(reload_or_stop)
            .push(url_bar)
            .push(bookmark_btn);

        let menu_items = if self.context_items.is_empty() {
            build_default_menu(can_back, can_forward)
        } else {
            build_servo_menu(&self.context_items)
        };

        let webview = WebViewWidget::new(tab.servo.frame())
            .scale(self.scale)
            .cursor(tab.servo.cursor())
            .context_menu(menu_items)
            .on_input(Message::Input)
            .on_resize(Message::WebViewResized)
            .keyboard_enabled(!tab.url_bar_focused);

        let webview_area = widget::container(webview).height(Length::Fill);

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

        widget::column::with_capacity(2)
            .push(nav_area)
            .push(webview_area)
            .into()
    }
}
