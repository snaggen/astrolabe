//! The MCP tools: each one maps onto one or a few WebDriver commands.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::actions;
use crate::webdriver::WebDriver;

/// Longest page text returned unless the caller asks for more.
const DEFAULT_TEXT_LIMIT: usize = 20_000;
/// Most elements `find_elements` reports unless the caller asks for more.
const DEFAULT_FIND_LIMIT: usize = 20;

const VIEWPORT_SCRIPT: &str =
    "return { width: innerWidth, height: innerHeight, devicePixelRatio, scrollX, scrollY };";

const TEXT_SCRIPT: &str = "return document.body ? document.body.innerText : '';";

const SCROLL_SCRIPT: &str =
    "const [dx, dy] = arguments; window.scrollBy(dx, dy); return { scrollX, scrollY };";

/// Describes matching elements with the centre point a click should aim for.
const FIND_SCRIPT: &str = r#"
const [selector, limit] = arguments;
return Array.from(document.querySelectorAll(selector)).slice(0, limit).map(el => {
  const r = el.getBoundingClientRect();
  return {
    tag: el.tagName.toLowerCase(),
    text: (el.innerText || el.value || el.getAttribute('aria-label') || '').trim().slice(0, 80),
    x: Math.round(r.left + r.width / 2),
    y: Math.round(r.top + r.height / 2),
    width: Math.round(r.width),
    height: Math.round(r.height),
    in_viewport: r.width > 0 && r.height > 0 && r.bottom > 0 && r.top < innerHeight,
  };
});
"#;

const INSTRUCTIONS: &str = "Drives the Astrolabe web browser (Servo engine) through its WebDriver \
port. Coordinates are CSS pixels from the viewport's top-left corner; a screenshot is in device \
pixels, so divide by devicePixelRatio before clicking. Use find_elements to get click targets. \
Pages that open alert/confirm dialogs are not supported.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct NavigateArgs {
    /// URL to open, or "back" / "forward" to move through the tab's history.
    url: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ReadTextArgs {
    /// Most characters to return (default 20000).
    max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct FindArgs {
    /// CSS selector, e.g. "a", "button", "input[name=q]".
    selector: String,
    /// Most elements to report (default 20).
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ClickArgs {
    /// CSS selector of the element to click. Give this or x and y.
    selector: Option<String>,
    /// Horizontal viewport position in CSS pixels.
    x: Option<i64>,
    /// Vertical viewport position in CSS pixels.
    y: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct TypeArgs {
    /// Text to type.
    text: String,
    /// CSS selector of the field to type into; without it, the text goes to
    /// whatever has focus.
    selector: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct KeyArgs {
    /// Key or combination, e.g. "Enter", "Escape", "ArrowDown", "Control+a", "Shift+Tab".
    key: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ScrollArgs {
    /// Pixels to scroll down (negative scrolls up).
    delta_y: f64,
    /// Pixels to scroll right (negative scrolls left). Default 0.
    delta_x: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ScriptArgs {
    /// Function body to run in the page; use `return` to hand back a value.
    script: String,
}

/// MCP server exposing the browser behind one WebDriver port.
#[derive(Clone)]
pub struct BrowserTools {
    driver: Arc<WebDriver>,
    tool_router: ToolRouter<Self>,
}

impl BrowserTools {
    pub fn new(driver: WebDriver) -> Self {
        Self {
            driver: Arc::new(driver),
            tool_router: Self::tool_router(),
        }
    }

    /// "Now at <url> — <title>", to confirm where a navigation ended up.
    async fn location(&self) -> Result<String, crate::webdriver::Error> {
        let url = self.driver.current_url().await?;
        let title = self.driver.title().await?;
        Ok(format!("Now at {url} — {title}"))
    }
}

#[tool_router]
impl BrowserTools {
    /// Open a URL in the current tab, or go back / forward in its history.
    #[tool]
    async fn navigate(
        &self,
        Parameters(args): Parameters<NavigateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let moved = match args.url.as_str() {
            "back" => self.driver.back().await,
            "forward" => self.driver.forward().await,
            url => self.driver.navigate(url).await,
        };
        let result = match moved {
            Ok(()) => self.location().await,
            Err(e) => Err(e),
        };
        Ok(text_result(result))
    }

    /// Screenshot of the page's viewport, with its size and device pixel ratio.
    #[tool(annotations(read_only_hint = true))]
    async fn screenshot(&self) -> Result<CallToolResult, McpError> {
        let png = match self.driver.screenshot().await {
            Ok(png) => png,
            Err(e) => return Ok(error_result(e)),
        };
        let viewport = self
            .driver
            .execute(VIEWPORT_SCRIPT, vec![])
            .await
            .unwrap_or(Value::Null);
        Ok(CallToolResult::success(vec![
            ContentBlock::image(png, "image/png"),
            ContentBlock::text(format!("Viewport: {viewport}")),
        ]))
    }

    /// The current URL, title, viewport size and scroll position.
    #[tool(annotations(read_only_hint = true))]
    async fn page_info(&self) -> Result<CallToolResult, McpError> {
        let info = async {
            let location = self.location().await?;
            let viewport = self.driver.execute(VIEWPORT_SCRIPT, vec![]).await?;
            Ok(format!("{location}\nViewport: {viewport}"))
        };
        Ok(text_result(info.await))
    }

    /// The visible text of the page (document.body.innerText).
    #[tool(annotations(read_only_hint = true))]
    async fn read_page_text(
        &self,
        Parameters(args): Parameters<ReadTextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = args.max_chars.unwrap_or(DEFAULT_TEXT_LIMIT);
        let text = self.driver.execute(TEXT_SCRIPT, vec![]).await.map(|v| {
            let text = v.as_str().unwrap_or_default();
            let (kept, cut) = truncate_chars(text, limit);
            if cut {
                format!("{kept}\n[… truncated at {limit} characters]")
            } else {
                kept.to_owned()
            }
        });
        Ok(text_result(text))
    }

    /// List elements matching a CSS selector, with tag, text and the centre
    /// point to click.
    #[tool(annotations(read_only_hint = true))]
    async fn find_elements(
        &self,
        Parameters(args): Parameters<FindArgs>,
    ) -> Result<CallToolResult, McpError> {
        let limit = args.limit.unwrap_or(DEFAULT_FIND_LIMIT);
        let found = self
            .driver
            .execute(FIND_SCRIPT, vec![json!(args.selector), json!(limit)])
            .await
            .map(|v| pretty(&v));
        Ok(text_result(found))
    }

    /// Click an element by CSS selector, or a point in the viewport.
    #[tool]
    async fn click(
        &self,
        Parameters(args): Parameters<ClickArgs>,
    ) -> Result<CallToolResult, McpError> {
        let clicked = match (args.selector, args.x, args.y) {
            (Some(selector), _, _) => self.driver.click_element(&selector).await,
            (None, Some(x), Some(y)) => self.driver.perform_actions(actions::click_at(x, y)).await,
            _ => return Ok(message_error("give either selector, or both x and y")),
        };
        Ok(text_result(clicked.map(|()| "Clicked".to_owned())))
    }

    /// Type text into a field, or into whatever has focus.
    #[tool]
    async fn type_text(
        &self,
        Parameters(args): Parameters<TypeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let typed = match args.selector {
            Some(selector) => self.driver.type_into_element(&selector, &args.text).await,
            None => {
                self.driver
                    .perform_actions(actions::type_text(&args.text))
                    .await
            }
        };
        Ok(text_result(typed.map(|()| "Typed".to_owned())))
    }

    /// Press a key or key combination on whatever has focus.
    #[tool]
    async fn press_key(
        &self,
        Parameters(args): Parameters<KeyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let sequence = match actions::press_key(&args.key) {
            Ok(sequence) => sequence,
            Err(e) => return Ok(message_error(&e)),
        };
        let pressed = self.driver.perform_actions(sequence).await;
        Ok(text_result(
            pressed.map(|()| format!("Pressed {}", args.key)),
        ))
    }

    /// Scroll the page by a number of CSS pixels.
    #[tool]
    async fn scroll(
        &self,
        Parameters(args): Parameters<ScrollArgs>,
    ) -> Result<CallToolResult, McpError> {
        let delta = vec![json!(args.delta_x.unwrap_or(0.0)), json!(args.delta_y)];
        let position = self.driver.execute(SCROLL_SCRIPT, delta).await;
        Ok(text_result(position.map(|v| format!("Scrolled to {v}"))))
    }

    /// Run JavaScript in the page and return its result as JSON.
    #[tool]
    async fn execute_javascript(
        &self,
        Parameters(args): Parameters<ScriptArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self.driver.execute(&args.script, vec![]).await;
        Ok(text_result(value.map(|v| pretty(&v))))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BrowserTools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("astrolabe", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// A tool result carrying `text`, or the error as a failed result the model can read.
fn text_result(result: Result<String, crate::webdriver::Error>) -> CallToolResult {
    match result {
        Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
        Err(e) => error_result(e),
    }
}

fn error_result(error: crate::webdriver::Error) -> CallToolResult {
    message_error(&error.to_string())
}

fn message_error(message: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// The first `max` characters of `text`, and whether anything was cut.
fn truncate_chars(text: &str, max: usize) -> (&str, bool) {
    match text.char_indices().nth(max) {
        Some((end, _)) => (&text[..end], true),
        None => (text, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_kept_whole() {
        assert_eq!(truncate_chars("abc", 5), ("abc", false));
        assert_eq!(truncate_chars("abc", 3), ("abc", false));
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        assert_eq!(truncate_chars("åäöx", 3), ("åäö", true));
    }

    #[test]
    fn every_tool_is_listed() {
        let tools = BrowserTools::new(WebDriver::new("http://127.0.0.1:1"));
        let mut names: Vec<String> = tools
            .tool_router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "click",
                "execute_javascript",
                "find_elements",
                "navigate",
                "page_info",
                "press_key",
                "read_page_text",
                "screenshot",
                "scroll",
                "type_text",
            ]
        );
    }
}
