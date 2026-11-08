//! Desktop notifications (`org.freedesktop.Notifications`) for the Notification API.
//!
//! A page that has been granted the notification permission can post one through
//! `new Notification(...)`; Servo hands it to the embedder, and this module puts
//! it on the desktop's notification bus. Servo checks the permission before the
//! notification reaches us, so anything arriving here is cleared to display.
//!
//! Like [`crate::mpris`], the D-Bus work runs on its own thread with a
//! current-thread Tokio runtime — zbus needs a runtime, and the browser's own
//! loop must not block on the bus.

use zbus::proxy;

/// How long the desktop shows a notification that does not require interaction.
/// `0` would mean "never expire", which is the wrong default for web content.
const DEFAULT_TIMEOUT_MS: i32 = 5_000;

/// The subset of the freedesktop notification spec this browser uses.
#[proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}

/// A notification reduced to what the desktop bus takes.
///
/// Servo's own type is not `Send`, so the parts that cross to the D-Bus thread
/// are copied out first.
#[derive(Clone, Debug)]
pub struct Notification {
    pub title: String,
    pub body: String,
    /// Pages reuse a tag to replace an earlier notification instead of stacking
    /// several; empty means the page did not ask for that.
    pub tag: String,
    /// Whether the notification should stay until the user acts on it.
    pub require_interaction: bool,
}

impl From<&astrolabe_webview::Notification> for Notification {
    fn from(notification: &astrolabe_webview::Notification) -> Self {
        Self {
            title: notification.title.clone(),
            body: notification.body.clone(),
            tag: notification.tag.clone(),
            require_interaction: notification.require_interaction,
        }
    }
}

/// Handle held by the app: posts notifications to the desktop.
pub struct NotificationHandle {
    tx: async_channel::Sender<Notification>,
}

impl NotificationHandle {
    /// Queues a notification for display. Non-blocking; failures are logged by
    /// the sending thread, since a page cannot do anything about them.
    pub fn show(&self, notification: Notification) {
        let _ = self.tx.try_send(notification);
    }
}

/// Starts the notification thread, or returns `None` if the thread cannot start.
///
/// A missing notification server is not an error here: the send simply fails
/// later and is logged, exactly as when the desktop has notifications turned off.
pub fn spawn() -> Option<NotificationHandle> {
    let (tx, rx) = async_channel::unbounded::<Notification>();

    match std::thread::Builder::new()
        .name("notifications".to_owned())
        .spawn(move || run(rx))
    {
        Ok(_) => Some(NotificationHandle { tx }),
        Err(e) => {
            tracing::warn!("Failed to start notification thread: {e}");
            None
        }
    }
}

/// Thread body: owns the runtime and the connection, and posts what arrives.
fn run(rx: async_channel::Receiver<Notification>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::warn!("Notifications: failed to build runtime: {e}");
            return;
        }
    };

    runtime.block_on(async move {
        let connection = match zbus::Connection::session().await {
            Ok(connection) => connection,
            Err(e) => {
                tracing::warn!("Notifications: no session bus: {e}");
                return;
            }
        };
        let proxy = match NotificationsProxy::new(&connection).await {
            Ok(proxy) => proxy,
            Err(e) => {
                tracing::warn!("Notifications: no notification server: {e}");
                return;
            }
        };

        // Tag → id of the notification posted under it, so a page that reuses a
        // tag replaces its own earlier notification rather than stacking a new one.
        let mut by_tag: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

        while let Ok(notification) = rx.recv().await {
            let replaces = if notification.tag.is_empty() {
                0
            } else {
                by_tag.get(&notification.tag).copied().unwrap_or(0)
            };
            let timeout = if notification.require_interaction {
                0
            } else {
                DEFAULT_TIMEOUT_MS
            };

            match proxy
                .notify(
                    "astrolabe",
                    replaces,
                    "io.github.snaggen.Astrolabe",
                    &notification.title,
                    &notification.body,
                    &[],
                    std::collections::HashMap::new(),
                    timeout,
                )
                .await
            {
                Ok(id) if !notification.tag.is_empty() => {
                    by_tag.insert(notification.tag.clone(), id);
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("Failed to show notification: {e}"),
            }
        }
    });
}
