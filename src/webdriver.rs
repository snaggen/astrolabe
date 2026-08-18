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

use std::collections::HashMap;

use cosmic_webview::{GenericSender, WebDriverCommandMsg, WebDriverLoadStatus, WebViewId};

/// The WebDriver server and the navigations it is waiting on.
pub struct WebDriver {
    commands: cosmic_webview::webdriver::WebDriverCommands,
    /// Waiting clients, with the tab's load count from before the navigation was
    /// asked for. The tab reports `Complete` for the page it is *already*
    /// showing, so the count is what tells the new load from the old one.
    load_status_senders: HashMap<WebViewId, PendingLoad>,
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
        let commands = cosmic_webview::webdriver::start_webdriver(port)?;
        tracing::info!("WebDriver server listening on port {port}");
        Some(Self {
            commands,
            load_status_senders: HashMap::new(),
        })
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
