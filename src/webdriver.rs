//! WebDriver automation state, behind the `webdriver` feature.
//!
//! Servo runs the protocol server and answers everything that is about a
//! *document* — scripts, elements, cookies. What it cannot answer is anything
//! about windows and tabs, because it does not know how this browser arranges
//! them; `App::handle_webdriver_command` answers those and forwards the rest
//! into the engine.
//!
//! Navigation is the one part with state of its own. `LoadUrl` and friends carry
//! a sender that must be answered when the load *finishes*, not when it starts,
//! so those senders wait here until the tab reports `LoadStatus::Complete`. A
//! client blocks on its HTTP request meanwhile: a sender that is never answered
//! is a hung test rather than a failed one, which is why closing a tab cancels
//! the ones it owns.
//!
//! Every new session also waits here, for the user to allow it. The server
//! listens on all network interfaces, so a client may not even be on this
//! computer. Servo asks the embedder which tab a new session should drive, and
//! only when a session starts; that question is held until the user answers.

use std::collections::HashMap;

use astrolabe_webview::{GenericSender, WebDriverCommandMsg, WebDriverLoadStatus, WebViewId};

/// The WebDriver server and the navigations it is waiting on.
pub struct WebDriver {
    commands: astrolabe_webview::webdriver::WebDriverCommands,
    /// Waiting clients, with the tab's load count from before the navigation was
    /// asked for. The tab reports `Complete` for the page it is *already*
    /// showing, so the count is what tells the new load from the old one.
    load_status_senders: HashMap<WebViewId, PendingLoad>,
    /// New sessions waiting for the user's approval.
    sessions: SessionGate<Option<WebViewId>>,
}

/// Holds a new session until the user allows or denies it.
///
/// `T` is what an allowed session is handed: the tab it will drive.
pub struct SessionGate<T> {
    /// Answers the waiting session. Dropping it unanswered fails the session.
    pending: Option<Box<dyn FnOnce(T)>>,
}

impl<T> Default for SessionGate<T> {
    fn default() -> Self {
        Self { pending: None }
    }
}

impl<T> SessionGate<T> {
    /// Hold a new session; `answer` lets it start once the user allows it.
    pub fn hold(&mut self, answer: impl FnOnce(T) + 'static) {
        self.pending = Some(Box::new(answer));
    }

    /// Whether a session is waiting for the user.
    pub fn is_waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// Let the waiting session start with `value`.
    pub fn allow(&mut self, value: T) {
        if let Some(answer) = self.pending.take() {
            answer(value);
        }
    }

    /// Fail the waiting session.
    ///
    /// Servo has already created the session it asked about and keeps it,
    /// unusable, so it refuses every new session until the browser restarts.
    pub fn deny(&mut self) {
        self.pending = None;
    }
}

/// A client waiting for a navigation to finish.
struct PendingLoad {
    sender: GenericSender<WebDriverLoadStatus>,
    /// The tab's load count when the navigation was requested.
    generation: u64,
}

impl WebDriver {
    /// Starts the server on `port`, or returns `None` if it cannot start.
    ///
    /// The engine must already be running: the server borrows its waker and the
    /// preferences it resets sessions to.
    pub fn start(port: u16) -> Option<Self> {
        let commands = astrolabe_webview::webdriver::start_webdriver(port)?;
        tracing::info!("WebDriver server listening on port {port}");
        Some(Self {
            commands,
            load_status_senders: HashMap::new(),
            sessions: SessionGate::default(),
        })
    }

    /// New sessions waiting for the user's approval.
    pub fn sessions(&mut self) -> &mut SessionGate<Option<WebViewId>> {
        &mut self.sessions
    }

    /// Whether a new session is waiting for the user's approval.
    pub fn awaits_approval(&self) -> bool {
        self.sessions.is_waiting()
    }

    /// Takes the next pending command, or `None` when the queue is empty.
    pub fn next_command(&self) -> Option<WebDriverCommandMsg> {
        self.commands.try_recv()
    }

    /// Records that a client is waiting for `webview` to finish loading.
    ///
    /// `generation` is the tab's load count from *before* the navigation was
    /// requested.
    pub fn expect_load(
        &mut self,
        webview: WebViewId,
        sender: GenericSender<WebDriverLoadStatus>,
        generation: u64,
    ) {
        let pending = PendingLoad { sender, generation };
        if let Some(previous) = self.load_status_senders.insert(webview, pending) {
            // A newer navigation replaced this one; the waiting client is told
            // so rather than left to time out.
            let _ = previous.sender.send(WebDriverLoadStatus::NavigationStop);
        }
    }

    /// Answers a client waiting on `webview` once its *own* navigation has
    /// finished: the tab must have started a load since the request, and that
    /// load must now be complete.
    pub fn notify_load_state(&mut self, webview: WebViewId, generation: u64, complete: bool) {
        let started = self
            .load_status_senders
            .get(&webview)
            .is_some_and(|pending| generation > pending.generation);
        if started
            && complete
            && let Some(pending) = self.load_status_senders.remove(&webview)
        {
            let _ = pending.sender.send(WebDriverLoadStatus::Complete);
        }
    }

    /// Releases a waiting sender when its tab goes away.
    pub fn forget(&mut self, webview: WebViewId) {
        if let Some(pending) = self.load_status_senders.remove(&webview) {
            let _ = pending.sender.send(WebDriverLoadStatus::NavigationStop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    /// A gate holding one session, and the value it was answered with, if any.
    fn gate_with_waiting_session() -> (SessionGate<u32>, Rc<Cell<Option<u32>>>) {
        let answered = Rc::new(Cell::new(None));
        let mut gate = SessionGate::default();
        let seen = Rc::clone(&answered);
        gate.hold(move |value| seen.set(Some(value)));
        (gate, answered)
    }

    #[test]
    fn a_held_session_waits_for_the_user() {
        let (gate, answered) = gate_with_waiting_session();
        assert!(gate.is_waiting());
        assert_eq!(answered.get(), None);
    }

    #[test]
    fn an_allowed_session_is_answered() {
        let (mut gate, answered) = gate_with_waiting_session();
        gate.allow(7);
        assert_eq!(answered.get(), Some(7));
        assert!(!gate.is_waiting());
    }

    #[test]
    fn a_denied_session_is_never_answered() {
        let (mut gate, answered) = gate_with_waiting_session();
        gate.deny();
        assert_eq!(answered.get(), None);
        assert!(!gate.is_waiting());
    }

    #[test]
    fn a_session_is_answered_only_once() {
        let (mut gate, answered) = gate_with_waiting_session();
        gate.allow(1);
        gate.allow(2);
        assert_eq!(answered.get(), Some(1));
    }
}
