//! A minimal WebDriver client for the one browser behind `--webdriver-port`.
//!
//! Servo accepts a single session at a time, so the client opens one lazily on
//! the first command and keeps it. If the browser restarts, the old session id
//! is rejected; the client then opens a new session and retries once.

use std::fmt;

use reqwest::Method;
use serde_json::{Value, json};
use tokio::sync::Mutex;

/// Key under which WebDriver returns an element reference.
const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";

/// Why a WebDriver command failed.
#[derive(Debug)]
pub enum Error {
    /// The browser could not be reached, e.g. it is not running.
    Http(reqwest::Error),
    /// The browser answered with a WebDriver error.
    WebDriver { error: String, message: String },
    /// The browser answered with something that is not valid WebDriver.
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => write!(
                f,
                "cannot reach Astrolabe's WebDriver port ({e}); is it running with --webdriver-port?"
            ),
            Self::WebDriver { error, message } => write!(f, "{error}: {message}"),
            Self::Protocol(detail) => write!(f, "unexpected WebDriver response: {detail}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Client for one browser's WebDriver server.
pub struct WebDriver {
    http: reqwest::Client,
    base_url: String,
    /// Id of the open session, if one has been created.
    session: Mutex<Option<String>>,
}

impl WebDriver {
    /// A client for the WebDriver server at `base_url`, e.g. `http://127.0.0.1:7000`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into(),
            session: Mutex::new(None),
        }
    }

    /// Navigate the current tab to `url` and wait for it to load.
    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.command(Method::POST, "/url", Some(json!({ "url": url })))
            .await
            .map(drop)
    }

    /// Go one step back in the tab's history.
    pub async fn back(&self) -> Result<()> {
        self.command(Method::POST, "/back", Some(json!({})))
            .await
            .map(drop)
    }

    /// Go one step forward in the tab's history.
    pub async fn forward(&self) -> Result<()> {
        self.command(Method::POST, "/forward", Some(json!({})))
            .await
            .map(drop)
    }

    /// The URL of the current page.
    pub async fn current_url(&self) -> Result<String> {
        let value = self.command(Method::GET, "/url", None).await?;
        as_string(value)
    }

    /// The title of the current page.
    pub async fn title(&self) -> Result<String> {
        let value = self.command(Method::GET, "/title", None).await?;
        as_string(value)
    }

    /// A PNG of the page's viewport, base64-encoded.
    pub async fn screenshot(&self) -> Result<String> {
        let value = self.command(Method::GET, "/screenshot", None).await?;
        as_string(value)
    }

    /// Run `script` as the body of a function in the page and return its
    /// result. The script reads `args` through `arguments`.
    pub async fn execute(&self, script: &str, args: Vec<Value>) -> Result<Value> {
        let body = json!({ "script": script, "args": args });
        self.command(Method::POST, "/execute/sync", Some(body))
            .await
    }

    /// Run a sequence of input actions (pointer, keyboard).
    pub async fn perform_actions(&self, actions: Value) -> Result<()> {
        let body = json!({ "actions": actions });
        self.command(Method::POST, "/actions", Some(body))
            .await
            .map(drop)
    }

    /// Click the first element matching the CSS `selector`.
    pub async fn click_element(&self, selector: &str) -> Result<()> {
        let element = self.find_element(selector).await?;
        let path = format!("/element/{element}/click");
        self.command(Method::POST, &path, Some(json!({})))
            .await
            .map(drop)
    }

    /// Type `text` into the first element matching the CSS `selector`.
    pub async fn type_into_element(&self, selector: &str, text: &str) -> Result<()> {
        let element = self.find_element(selector).await?;
        let path = format!("/element/{element}/value");
        self.command(Method::POST, &path, Some(json!({ "text": text })))
            .await
            .map(drop)
    }

    /// The WebDriver reference of the first element matching `selector`.
    async fn find_element(&self, selector: &str) -> Result<String> {
        let body = json!({ "using": "css selector", "value": selector });
        let value = self.command(Method::POST, "/element", Some(body)).await?;
        value
            .get(ELEMENT_KEY)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Protocol(format!("no element reference in {value}")))
    }

    /// Send a session command, opening a session first if there is none.
    ///
    /// A rejected session id means the browser restarted; the command is then
    /// retried once on a fresh session.
    async fn command(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let session = self.session_id().await?;
        let url = format!("/session/{session}{path}");
        match self.request(method.clone(), &url, body.clone()).await {
            Err(Error::WebDriver { error, .. }) if error == "invalid session id" => {
                self.session.lock().await.take();
                let session = self.session_id().await?;
                let url = format!("/session/{session}{path}");
                self.request(method, &url, body).await
            }
            result => result,
        }
    }

    /// The open session's id, creating the session if needed.
    async fn session_id(&self) -> Result<String> {
        let mut session = self.session.lock().await;
        if let Some(id) = session.as_ref() {
            return Ok(id.clone());
        }
        let body = json!({ "capabilities": { "alwaysMatch": {} } });
        let value = self.request(Method::POST, "/session", Some(body)).await?;
        let id = session_id_from(&value)?;
        *session = Some(id.clone());
        Ok(id)
    }

    /// Send one HTTP request and unwrap WebDriver's `{"value": ...}` envelope.
    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base_url));
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let response = request.send().await?;
        let bytes = response.bytes().await?;
        parse_response(&bytes)
    }
}

/// Unwrap a WebDriver response body into its `value`, or the error it reports.
fn parse_response(body: &[u8]) -> Result<Value> {
    let parsed: Value =
        serde_json::from_slice(body).map_err(|e| Error::Protocol(format!("invalid JSON ({e})")))?;
    let Some(value) = parsed.get("value") else {
        return Err(Error::Protocol(format!("no \"value\" in {parsed}")));
    };
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Err(Error::WebDriver {
            error: error.to_owned(),
            message: message.to_owned(),
        });
    }
    Ok(value.clone())
}

/// The session id from a New Session response.
///
/// Servo allows one session at a time and answers a second request with an
/// empty id rather than an error, so an empty id is reported as such.
fn session_id_from(value: &Value) -> Result<String> {
    match value.get("sessionId").and_then(Value::as_str) {
        Some("") => Err(Error::WebDriver {
            error: "session not created".to_owned(),
            message: "another WebDriver session is already open; Servo allows only one".to_owned(),
        }),
        Some(id) => Ok(id.to_owned()),
        None => Err(Error::Protocol(format!("no session id in {value}"))),
    }
}

fn as_string(value: Value) -> Result<String> {
    match value {
        Value::String(s) => Ok(s),
        other => Err(Error::Protocol(format!("expected a string, got {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn a_value_is_unwrapped() {
        let value = parse_response(br#"{"value":"https://example.com/"}"#).unwrap();
        assert_eq!(value, json!("https://example.com/"));
    }

    #[test]
    fn a_webdriver_error_is_reported() {
        let body =
            br#"{"value":{"error":"no such element","message":"not found","stacktrace":""}}"#;
        match parse_response(body) {
            Err(Error::WebDriver { error, message }) => {
                assert_eq!(error, "no such element");
                assert_eq!(message, "not found");
            }
            other => panic!("expected a WebDriver error, got {other:?}"),
        }
    }

    #[test]
    fn a_body_without_value_is_a_protocol_error() {
        assert!(matches!(parse_response(b"{}"), Err(Error::Protocol(_))));
        assert!(matches!(
            parse_response(b"not json"),
            Err(Error::Protocol(_))
        ));
    }

    #[test]
    fn an_empty_session_id_means_one_is_already_open() {
        let err = session_id_from(&json!({ "sessionId": "" })).unwrap_err();
        assert!(err.to_string().contains("only one"), "{err}");
        assert_eq!(
            session_id_from(&json!({ "sessionId": "abc" })).unwrap(),
            "abc"
        );
    }

    /// Serve one canned response per connection, recording each request line.
    ///
    /// Responses close the connection, so every request arrives on a new one.
    async fn fake_server(
        responses: Vec<&'static str>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut seen = Vec::new();
            for body in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0u8; 8192];
                let read = socket.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..read]);
                seen.push(request.lines().next().unwrap_or_default().to_owned());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            seen
        });
        (base_url, handle)
    }

    #[tokio::test]
    async fn the_session_is_opened_once_and_reused() {
        let (base_url, server) = fake_server(vec![
            r#"{"value":{"sessionId":"s1","capabilities":{}}}"#,
            r#"{"value":"Example"}"#,
            r#"{"value":"https://example.com/"}"#,
        ])
        .await;
        let driver = WebDriver::new(base_url);

        assert_eq!(driver.title().await.unwrap(), "Example");
        assert_eq!(driver.current_url().await.unwrap(), "https://example.com/");

        let seen = server.await.unwrap();
        assert_eq!(
            seen,
            [
                "POST /session HTTP/1.1",
                "GET /session/s1/title HTTP/1.1",
                "GET /session/s1/url HTTP/1.1",
            ]
        );
    }

    #[tokio::test]
    async fn a_rejected_session_is_replaced_and_the_command_retried() {
        let (base_url, server) = fake_server(vec![
            r#"{"value":{"sessionId":"old","capabilities":{}}}"#,
            r#"{"value":{"error":"invalid session id","message":"gone"}}"#,
            r#"{"value":{"sessionId":"new","capabilities":{}}}"#,
            r#"{"value":"Example"}"#,
        ])
        .await;
        let driver = WebDriver::new(base_url);

        assert_eq!(driver.title().await.unwrap(), "Example");

        let seen = server.await.unwrap();
        assert_eq!(
            seen,
            [
                "POST /session HTTP/1.1",
                "GET /session/old/title HTTP/1.1",
                "POST /session HTTP/1.1",
                "GET /session/new/title HTTP/1.1",
            ]
        );
    }
}
