//! MCP server that lets an AI assistant drive Astrolabe through its WebDriver
//! port: navigate, read the page, take screenshots, click and type.
//!
//! Start the browser with WebDriver enabled, then point an MCP client at this
//! binary over stdio:
//!
//! ```text
//! cargo run --features webdriver -- --webdriver-port 7000
//! claude mcp add astrolabe -- /path/to/astrolabe-mcp --port 7000
//! ```

mod actions;
mod tools;
mod webdriver;

use clap::Parser;
use rmcp::ServiceExt;

/// Command-line options.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Astrolabe's WebDriver port (its --webdriver-port).
    #[arg(long, default_value_t = 7000, value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let driver = webdriver::WebDriver::new(format!("http://127.0.0.1:{}", args.port));
    // Stdout carries the protocol, so nothing else may be printed there.
    let server = tools::BrowserTools::new(driver)
        .serve(rmcp::transport::stdio())
        .await?;
    server.waiting().await?;
    Ok(())
}
