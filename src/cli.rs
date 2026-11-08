//! Command-line arguments.
//!
//! Only startup and profile plumbing lives here — everything a user can change
//! while the browser runs belongs in `cosmic_config` instead.

use std::path::PathBuf;

use clap::Parser;
use url::Url;

/// Options parsed from the command line.
#[derive(Clone, Debug, Default, Eq, Parser, PartialEq)]
#[command(
    name = "astrolabe",
    version,
    about = "A web browser for the COSMIC desktop"
)]
pub struct Cli {
    /// Page to open at startup, overriding the configured startup page.
    /// A bare host name is read as https://<host>.
    #[arg(value_name = "URL", value_parser = parse_url)]
    pub url: Option<Url>,

    /// Store cookies, history and other profile data here instead of in the
    /// XDG data directory.
    #[arg(long, value_name = "DIR")]
    pub profile_dir: Option<PathBuf>,

    /// Read *.js user scripts from here instead of from the XDG config directory.
    #[arg(long = "userscripts", value_name = "DIR")]
    pub userscripts_dir: Option<PathBuf>,

    /// Run the remote devtools server on this port for the current session,
    /// whatever the setting says.
    #[arg(long, value_name = "PORT", value_parser = clap::value_parser!(u16).range(1..))]
    pub devtools_port: Option<u16>,

    /// Accept WebDriver automation on this port. Without it no server runs.
    #[cfg(feature = "webdriver")]
    #[arg(long, value_name = "PORT", value_parser = clap::value_parser!(u16).range(1..))]
    pub webdriver_port: Option<u16>,
}

/// Normalize a command-line URL: anything with a scheme is taken as it is —
/// including schemes without an authority, such as `data:` and `about:` — and a
/// bare host name becomes `https://<host>`.
fn parse_url(raw: &str) -> Result<Url, String> {
    let normalized = if has_scheme(raw) {
        raw.to_owned()
    } else {
        format!("https://{raw}")
    };
    Url::parse(&normalized).map_err(|e| format!("invalid URL '{raw}': {e}"))
}

/// True when `raw` opens with a URL scheme (`https:`, `data:`, `about:`) rather
/// than a bare address.
///
/// `localhost:8080` is the ambiguous case: it has the shape of a scheme, but
/// means a host and a port. Only digits may follow the colon in a port, and no
/// scheme is all digits, so that tells the two apart.
fn has_scheme(raw: &str) -> bool {
    let Some((prefix, rest)) = raw.split_once(':') else {
        return false;
    };
    let is_scheme = prefix.starts_with(|c: char| c.is_ascii_alphabetic())
        && prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    let port_follows = rest
        .split(['/', '?', '#'])
        .next()
        .is_some_and(|port| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()));

    is_scheme && !port_follows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse an argument list the way `Cli::parse` reads `std::env::args`,
    /// program name included.
    fn options(args: &[&str]) -> Cli {
        let argv = std::iter::once("astrolabe").chain(args.iter().copied());
        Cli::try_parse_from(argv).expect("arguments should parse")
    }

    fn error(args: &[&str]) -> String {
        let argv = std::iter::once("astrolabe").chain(args.iter().copied());
        Cli::try_parse_from(argv)
            .expect_err("arguments should be rejected")
            .to_string()
    }

    #[test]
    fn no_arguments_yields_defaults() {
        assert_eq!(options(&[]), Cli::default());
    }

    #[test]
    fn bare_host_becomes_https() {
        let url = options(&["example.com"]).url.expect("url");
        assert_eq!(url.as_str(), "https://example.com/");
    }

    #[test]
    fn explicit_scheme_is_kept() {
        let url = options(&["http://example.com/a"]).url.expect("url");
        assert_eq!(url.as_str(), "http://example.com/a");
    }

    #[test]
    fn schemes_without_an_authority_are_kept() {
        let data = options(&["data:text/html,<p>hi</p>"]).url.expect("url");
        assert_eq!(data.scheme(), "data");
        let about = options(&["about:blank"]).url.expect("url");
        assert_eq!(about.as_str(), "about:blank");
    }

    #[test]
    fn a_host_with_a_port_is_not_read_as_a_scheme() {
        for raw in ["localhost:8080", "localhost:8080/app", "127.0.0.1:8731"] {
            let url = options(&[raw]).url.expect("url");
            assert_eq!(url.scheme(), "https", "{raw}");
            assert_eq!(
                url.port(),
                Some(if raw.starts_with('1') { 8731 } else { 8080 })
            );
        }
    }

    #[test]
    fn directories_and_port_are_parsed() {
        let cli = options(&[
            "--devtools-port",
            "6081",
            "--profile-dir",
            "/tmp/profile",
            "--userscripts=/tmp/scripts",
        ]);
        assert_eq!(cli.devtools_port, Some(6081));
        assert_eq!(cli.profile_dir, Some(PathBuf::from("/tmp/profile")));
        assert_eq!(cli.userscripts_dir, Some(PathBuf::from("/tmp/scripts")));
    }

    #[test]
    fn bad_arguments_are_reported_not_panicked() {
        assert!(!error(&["--nope"]).is_empty());
        assert!(!error(&["--devtools-port"]).is_empty());
        assert!(!error(&["--devtools-port", "0"]).is_empty());
        assert!(!error(&["--devtools-port", "99999"]).is_empty());
        assert!(!error(&["--devtools-port", "http"]).is_empty());
        assert!(!error(&["--profile-dir"]).is_empty());
        assert!(!error(&["a.example", "b.example"]).is_empty());
    }

    #[test]
    fn a_malformed_url_names_the_argument() {
        assert!(error(&["http://[not a url"]).contains("invalid URL"));
    }
}
