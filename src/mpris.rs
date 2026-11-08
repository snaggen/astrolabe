//! MPRIS (`org.mpris.MediaPlayer2`) D-Bus integration.
//!
//! Exposes the active tab's Media Session to the desktop so COSMIC's media
//! applet and hardware media keys can see metadata / playback status and drive
//! playback. The engine half lives in astrolabe-webview (`ServoState::media_*` and
//! `send_media_action`); this module bridges it to D-Bus.
//!
//! The server runs on a dedicated thread with its own current-thread Tokio
//! runtime (zbus picks the Tokio backend when a runtime is present). Two
//! channels bridge it to the iced app:
//! - `state_tx`: app → server, latest metadata/status to publish.
//! - `action_rx`: server → app, media actions requested over D-Bus.

use astrolabe_webview::MediaSessionActionType;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{connection, interface};

/// Playback status as MPRIS defines it (`Playing`/`Paused`/`Stopped`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

impl PlaybackStatus {
    fn as_str(self) -> &'static str {
        match self {
            PlaybackStatus::Playing => "Playing",
            PlaybackStatus::Paused => "Paused",
            PlaybackStatus::Stopped => "Stopped",
        }
    }
}

/// A snapshot of the active tab's media state, sent from the app to the server.
#[derive(Clone, Debug)]
pub struct MediaUpdate {
    pub status: PlaybackStatus,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// Handle held by the app: pushes state to the server and receives actions.
pub struct MprisHandle {
    state_tx: async_channel::Sender<MediaUpdate>,
    /// Cloned into the subscription that turns actions into app messages.
    pub action_rx: async_channel::Receiver<MediaSessionActionType>,
}

impl MprisHandle {
    /// Publishes the active tab's latest media state to the desktop. Non-blocking;
    /// a full channel just drops the update (the next one supersedes it).
    pub fn publish(&self, update: MediaUpdate) {
        let _ = self.state_tx.try_send(update);
    }
}

/// The `org.mpris.MediaPlayer2` root interface. Minimal: the browser is not a
/// standalone player, so it cannot quit or raise via MPRIS.
struct RootInterface;

#[interface(name = "org.mpris.MediaPlayer2")]
impl RootInterface {
    fn raise(&self) {}
    fn quit(&self) {}

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> &str {
        "Astrolabe"
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

/// The `org.mpris.MediaPlayer2.Player` interface. Playback controls forward to
/// Servo as [`MediaSessionActionType`] actions; properties reflect the latest
/// state pushed from the app.
struct PlayerInterface {
    action_tx: async_channel::Sender<MediaSessionActionType>,
    status: PlaybackStatus,
    metadata: std::collections::HashMap<String, OwnedValue>,
}

impl PlayerInterface {
    fn send(&self, action: MediaSessionActionType) {
        let _ = self.action_tx.try_send(action);
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl PlayerInterface {
    fn play(&self) {
        self.send(MediaSessionActionType::Play);
    }
    fn pause(&self) {
        self.send(MediaSessionActionType::Pause);
    }
    fn play_pause(&self) {
        // Toggle based on the last known status.
        let action = if self.status == PlaybackStatus::Playing {
            MediaSessionActionType::Pause
        } else {
            MediaSessionActionType::Play
        };
        self.send(action);
    }
    fn stop(&self) {
        self.send(MediaSessionActionType::Stop);
    }
    fn next(&self) {
        self.send(MediaSessionActionType::NextTrack);
    }
    fn previous(&self) {
        self.send(MediaSessionActionType::PreviousTrack);
    }
    fn seek(&self, offset: i64) {
        let action = if offset < 0 {
            MediaSessionActionType::SeekBackward
        } else {
            MediaSessionActionType::SeekForward
        };
        self.send(action);
    }
    fn set_position(&self, _track_id: ObjectPath<'_>, _position: i64) {}

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.status.as_str().to_owned()
    }
    #[zbus(property)]
    fn metadata(&self) -> std::collections::HashMap<String, OwnedValue> {
        self.metadata.clone()
    }
    // Servo does not tell the embedder which media-session action handlers a page
    // registered, so next/previous availability cannot be tracked per page. We
    // advertise them as available: a page with a real playlist gets working
    // next/previous (Servo invokes its handler), at the cost of showing controls
    // that do nothing on a single audio/video element.
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        0
    }
}

/// Builds the MPRIS `a{sv}` metadata map from a [`MediaUpdate`]. Always includes
/// the required `mpris:trackid`.
fn build_metadata(update: &MediaUpdate) -> std::collections::HashMap<String, OwnedValue> {
    let mut map = std::collections::HashMap::new();
    // A stable dummy track path — the browser has no real track objects.
    if let Ok(path) = ObjectPath::try_from("/org/mpris/MediaPlayer2/track/Current")
        .map_err(zbus::Error::from)
        .and_then(|p| Value::from(p).try_to_owned().map_err(Into::into))
    {
        map.insert("mpris:trackid".to_owned(), path);
    }
    if let Some(title) = &update.title
        && let Ok(v) = Value::from(title.clone()).try_to_owned()
    {
        map.insert("xesam:title".to_owned(), v);
    }
    if let Some(artist) = &update.artist
        && let Ok(v) = Value::from(vec![artist.clone()]).try_to_owned()
    {
        map.insert("xesam:artist".to_owned(), v);
    }
    if let Some(album) = &update.album
        && let Ok(v) = Value::from(album.clone()).try_to_owned()
    {
        map.insert("xesam:album".to_owned(), v);
    }
    map
}

const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const BUS_NAME: &str = "org.mpris.MediaPlayer2.astrolabe";

/// Starts the MPRIS server on a dedicated thread and returns a handle, or `None`
/// if the thread could not be spawned. D-Bus connection failures (e.g. no
/// session bus) are logged and leave the handle inert.
pub fn spawn() -> Option<MprisHandle> {
    let (state_tx, state_rx) = async_channel::unbounded::<MediaUpdate>();
    let (action_tx, action_rx) = async_channel::unbounded::<MediaSessionActionType>();

    let spawned = std::thread::Builder::new()
        .name("mpris".to_owned())
        .spawn(move || run(action_tx, state_rx));

    match spawned {
        Ok(_) => Some(MprisHandle {
            state_tx,
            action_rx,
        }),
        Err(e) => {
            tracing::warn!("Failed to start MPRIS thread: {e}");
            None
        }
    }
}

/// Thread body: owns the Tokio runtime, the D-Bus connection, and the loop that
/// publishes state updates as `PropertiesChanged`.
fn run(
    action_tx: async_channel::Sender<MediaSessionActionType>,
    state_rx: async_channel::Receiver<MediaUpdate>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::warn!("MPRIS: failed to build runtime: {e}");
            return;
        }
    };

    runtime.block_on(async move {
        if let Err(e) = serve(action_tx, state_rx).await {
            tracing::warn!("MPRIS server stopped: {e}");
        }
    });
}

/// Builds the connection, serves both interfaces, then publishes each incoming
/// [`MediaUpdate`] by mutating the player interface and emitting the change.
async fn serve(
    action_tx: async_channel::Sender<MediaSessionActionType>,
    state_rx: async_channel::Receiver<MediaUpdate>,
) -> zbus::Result<()> {
    let player = PlayerInterface {
        action_tx,
        status: PlaybackStatus::Stopped,
        metadata: std::collections::HashMap::new(),
    };

    let connection = connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, RootInterface)?
        .serve_at(OBJECT_PATH, player)?
        .build()
        .await?;

    let iface_ref = connection
        .object_server()
        .interface::<_, PlayerInterface>(OBJECT_PATH)
        .await?;

    // Loop ends when the app drops `state_tx`, releasing the bus name on exit.
    while let Ok(update) = state_rx.recv().await {
        let metadata = build_metadata(&update);
        let emitter: &SignalEmitter<'_> = iface_ref.signal_emitter();
        let mut iface = iface_ref.get_mut().await;
        iface.status = update.status;
        iface.metadata = metadata;
        iface.playback_status_changed(emitter).await?;
        iface.metadata_changed(emitter).await?;
    }

    Ok(())
}
