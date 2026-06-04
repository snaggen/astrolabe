use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use cosmic_webview::{DialogMsg, ServoState, WebViewFrame, WebViewWidget};
use dpi::PhysicalSize;
use libcosmic::app::{Core, Task};
use libcosmic::executor;
use libcosmic::iced::{Event, Length, Subscription, window};
use libcosmic::prelude::*;
use libcosmic::widget;
use libcosmic::widget::menu::{self, action::MenuAction};
use servo::{
    ContextMenu, ContextMenuAction, ContextMenuItem, EditingActionEvent, EmbedderControl,
    InputEvent,
};
use url::Url;

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
    /// Servo needs to spin its event loop.
    ServoWake,
    /// A mouse or keyboard event to forward to Servo.
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
    /// User selected an item from Servo's context menu.
    ContextMenuSelected(ContextMenuAction),
    /// Forwarded from ServoState's built-in dialog handler.
    Dialog(DialogMsg),
    /// WebView widget was laid out at a new physical size.
    WebViewResized(u32, u32),
    /// HiDPI scale factor changed.
    ScaleChanged(f32),
}

// ── Application state ─────────────────────────────────────────────────────────

pub struct App {
    core: Core,
    servo: ServoState,
    /// Persistent GPU texture updated in-place each frame.
    frame: WebViewFrame,
    /// Text currently shown in the URL bar input field.
    url_bar_text: String,
    /// True while the URL bar holds keyboard focus.
    url_bar_focused: bool,
    /// HiDPI scale for converting logical cursor coordinates to physical pixels.
    scale: f32,
    /// Kept alive while a context menu is open; dropping sends an auto-dismiss to Servo.
    context_menu: Option<ContextMenu>,
    /// Items from the most recent Servo context menu; cleared after selection or dismiss.
    context_items: Vec<ContextMenuItem>,
}

impl App {
    /// Parse the URL bar text and navigate if valid; prepend https:// if no scheme present.
    fn try_navigate(&mut self) {
        let raw = self.url_bar_text.trim().to_owned();
        let with_scheme = if raw.contains("://") {
            raw.clone()
        } else {
            format!("https://{raw}")
        };
        if let Ok(url) = Url::parse(&with_scheme) {
            self.url_bar_text = url.to_string();
            self.servo.navigate(url);
        }
    }

    /// Dismiss the active context menu without selecting an item.
    /// Dropping the ContextMenu handle sends an automatic dismiss to Servo.
    fn dismiss_context_menu(&mut self) {
        self.context_menu = None;
        self.context_items.clear();
    }
}

// ── Servo subscription helper ─────────────────────────────────────────────────

/// Newtype so async_channel::Receiver<()> satisfies Hash for Subscription::run_with.
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
        let initial_url = Url::parse("about:blank").expect("about:blank is always valid");
        // Read initial scale from the same env var iced uses so Servo's CSS
        // viewport is correct from the first frame.
        let scale = std::env::var("COSMIC_SCALE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(1.0);
        // Window dimensions must match the size passed to Settings in main.rs.
        let size = PhysicalSize::new(1280u32, 800);
        let (servo, _rx) = ServoState::new(initial_url.clone(), size, scale);
        let app = App {
            core,
            servo,
            frame: WebViewFrame::new(),
            url_bar_text: initial_url.to_string(),
            url_bar_focused: false,
            scale,
            context_menu: None,
            context_items: Vec::new(),
        };
        (app, Task::none())
    }

    fn subscription(&self) -> Subscription<Message> {
        use libcosmic::iced::event;
        use libcosmic::iced::futures::StreamExt as _;

        let servo_sub = Subscription::run_with(ServoRx(self.servo.waker_rx()), |s| {
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
                self.servo.spin();

                // Servo delivers context menus one frame after right-click via take_dialog.
                match self.servo.take_dialog() {
                    Some(EmbedderControl::ContextMenu(menu)) => {
                        self.context_items = menu.items().to_vec();
                        self.context_menu = Some(menu);
                    }
                    Some(control) => {
                        if let Some(task) = self.servo.handle_dialog_control(control) {
                            return task.map(|msg| libcosmic::action::app(Message::Dialog(msg)));
                        }
                    }
                    None => {}
                }

                // Keep the URL bar in sync with the page URL unless the user is typing.
                if !self.url_bar_focused
                    && let Some(url) = self.servo.current_url()
                {
                    self.url_bar_text = url.to_string();
                }

                self.set_header_title(self.servo.title().unwrap_or_default());

                if let Some(img) = self.servo.current_frame().as_ref() {
                    let (w, h) = img.dimensions();
                    self.frame.update(w, h, img.as_raw());
                }

                if let Some((reason, _bt)) = self.servo.take_crash() {
                    tracing::error!("WebView crashed: {reason}");
                }
            }

            Message::Input(event) => {
                self.servo.send_input(event);
            }

            Message::UrlBarFocused => {
                self.url_bar_focused = true;
            }

            Message::UrlBarUnfocused => {
                self.url_bar_focused = false;
            }

            Message::UrlBarChanged(text) => {
                self.url_bar_text = text;
            }

            Message::Navigate => {
                self.try_navigate();
            }

            Message::GoBack => {
                self.dismiss_context_menu();
                self.servo.go_back();
            }

            Message::GoForward => {
                self.dismiss_context_menu();
                self.servo.go_forward();
            }

            Message::Reload => {
                self.dismiss_context_menu();
                self.servo.reload();
            }

            Message::StopLoading => {
                // The Servo embedding API does not yet expose a stop method.
                // The button still provides visual feedback that a load is in progress.
            }

            Message::CopySelection => {
                self.dismiss_context_menu();
                self.servo
                    .send_input(InputEvent::EditingAction(EditingActionEvent::Copy));
            }

            Message::Paste => {
                self.dismiss_context_menu();
                self.servo
                    .send_input(InputEvent::EditingAction(EditingActionEvent::Paste));
            }

            Message::ContextMenuSelected(action) => {
                if let Some(menu) = self.context_menu.take() {
                    menu.select(action);
                }
                self.context_items.clear();
            }

            Message::Dialog(msg) => {
                self.servo.update_dialog(msg);
            }

            Message::WebViewResized(w, h) => {
                self.servo.resize(PhysicalSize::new(w, h));
            }

            Message::ScaleChanged(factor) => {
                self.scale = factor;
                self.servo.set_scale_factor(factor);
            }
        }

        Task::none()
    }

    fn header_start(&self) -> Vec<Element<'_, Message>> {
        use servo::LoadStatus;

        let back = widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
            .on_press_maybe(self.servo.can_go_back().then_some(Message::GoBack));

        let forward = widget::button::icon(widget::icon::from_name("go-next-symbolic"))
            .on_press_maybe(self.servo.can_go_forward().then_some(Message::GoForward));

        // Show a stop button while a page is loading, reload once complete.
        let reload_or_stop = if self.servo.load_status() == LoadStatus::Complete {
            widget::button::icon(widget::icon::from_name("view-refresh-symbolic"))
                .on_press(Message::Reload)
        } else {
            widget::button::icon(widget::icon::from_name("process-stop-symbolic"))
                .on_press(Message::StopLoading)
        };

        vec![back.into(), forward.into(), reload_or_stop.into()]
    }

    fn header_center(&self) -> Vec<Element<'_, Message>> {
        let url_bar = widget::text_input("https://example.com", &self.url_bar_text)
            .on_focus(Message::UrlBarFocused)
            .on_unfocus(Message::UrlBarUnfocused)
            .on_input(Message::UrlBarChanged)
            .on_submit(|_| Message::Navigate)
            .select_on_focus(true)
            .width(Length::Fill);

        vec![url_bar.into()]
    }

    fn dialog(&self) -> Option<Element<'_, Message>> {
        self.servo.dialog_view().map(|e| e.map(Message::Dialog))
    }

    fn view(&self) -> Element<'_, Message> {
        let webview = WebViewWidget::new(&self.frame)
            .scale(self.scale)
            .on_input(Message::Input)
            .on_resize(Message::WebViewResized)
            .keyboard_enabled(!self.url_bar_focused);

        // Use Servo's context-sensitive items when available (arrives ~1 frame after
        // right-click). Falls back to a static browser menu until they arrive.
        let menu_items = if self.context_items.is_empty() {
            build_default_menu(self.servo.can_go_back(), self.servo.can_go_forward())
        } else {
            build_servo_menu(&self.context_items)
        };

        widget::context_menu(webview, Some(menu_items)).into()
    }
}
