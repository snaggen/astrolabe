use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use cosmic_webview::{DialogMsg, WebViewWidget};
use dpi::PhysicalSize;
use libcosmic::app::{Core, Task};
use libcosmic::executor;
use libcosmic::iced::{Event, Length, Subscription, window};
use libcosmic::prelude::*;
use libcosmic::widget;
use libcosmic::widget::menu::{self, action::MenuAction};
use libcosmic::widget::segmented_button;
use servo::{
    ContextMenu, ContextMenuAction, ContextMenuItem, EditingActionEvent, EmbedderControl,
    InputEvent,
};
use url::Url;

use crate::tabs::TabState;

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
}

impl App {
    /// Parse the active tab's URL bar text and navigate.
    fn try_navigate(&mut self) {
        let Some(tab) = self.tabs.active_data_mut::<TabState>() else {
            return;
        };
        let raw = tab.url_bar_text.trim().to_owned();
        let with_scheme = if raw.contains("://") {
            raw
        } else {
            format!("https://{raw}")
        };
        if let Ok(url) = Url::parse(&with_scheme) {
            tab.url_bar_text = url.to_string();
            tab.servo.navigate(url);
        }
    }

    /// Dismiss the active context menu without selecting an item.
    fn dismiss_context_menu(&mut self) {
        self.context_menu = None;
        self.context_items.clear();
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
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "org.cosmic.WebBrowser";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _flags: ()) -> (Self, Task<Message>) {
        let scale = std::env::var("COSMIC_SCALE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(1.0);

        let initial_size = PhysicalSize::new(1280u32, 800);
        let initial_url = Url::parse("about:blank").expect("about:blank is always valid");

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
            tabs,
            servo_rx,
            scale,
            current_size: initial_size,
            context_menu: None,
            context_items: Vec::new(),
        };
        (app, Task::none())
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
            _ => None,
        });

        Subscription::batch([servo_sub, window_sub])
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

                        if is_active && let Some(img) = tab.servo.current_frame().as_ref() {
                            let (w, h) = img.dimensions();
                            tab.frame.update(w, h, img.as_raw());
                        }

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
            }

            Message::Input(event) => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.servo.send_input(event);
                }
            }

            Message::UrlBarFocused => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.url_bar_focused = true;
                }
            }

            Message::UrlBarUnfocused => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.url_bar_focused = false;
                }
            }

            Message::UrlBarChanged(text) => {
                if let Some(tab) = self.tabs.active_data_mut::<TabState>() {
                    tab.url_bar_text = text;
                }
            }

            Message::Navigate => {
                self.try_navigate();
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
                self.dismiss_context_menu();
                self.sync_active_title();
            }

            Message::TabClosed(id) => {
                self.close_tab(id);
                self.sync_active_title();
            }

            Message::TabNew => {
                let url = Url::parse("about:blank").expect("about:blank is always valid");
                let id = self.add_tab(url);
                self.tabs.activate(id);
                self.dismiss_context_menu();
            }
        }

        Task::none()
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
        use servo::LoadStatus;

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
            .on_focus(Message::UrlBarFocused)
            .on_unfocus(Message::UrlBarUnfocused)
            .on_input(Message::UrlBarChanged)
            .on_submit(|_| Message::Navigate)
            .select_on_focus(true)
            .width(Length::Fill);

        let nav_row = widget::row::with_capacity(4)
            .push(back)
            .push(forward)
            .push(reload_or_stop)
            .push(url_bar);

        let webview = WebViewWidget::new(&tab.frame)
            .scale(self.scale)
            .on_input(Message::Input)
            .on_resize(Message::WebViewResized)
            .keyboard_enabled(!tab.url_bar_focused);

        let menu_items = if self.context_items.is_empty() {
            build_default_menu(can_back, can_forward)
        } else {
            build_servo_menu(&self.context_items)
        };

        widget::column::with_capacity(2)
            .push(nav_row)
            .push(
                widget::container(widget::context_menu(webview, Some(menu_items)))
                    .height(Length::Fill),
            )
            .into()
    }
}
