//! Adblock filter engine wrapping Brave's `adblock` crate.
//!
//! Builds one [`adblock::Engine`] per protection level and exposes a
//! [`ContentFilterFn`] suitable for [`astrolabe_webview::ServoState::set_content_filter`].

use std::sync::Arc;

use adblock::Engine;
use adblock::lists::{FilterSet, ParseOptions};
use adblock::request::Request;
use astrolabe_webview::ContentFilterFn;
use url::Url;

use crate::config::CookieProtectionLevel;

/// Compiled filter engines, one per protection level.
///
/// Both engines include any user-configured custom filter lists on top of the
/// built-in lists. Engines are expensive to build (parsing filter lists) so we
/// build them once at startup and share them via [`Arc`] across all tabs.
pub struct FilterEngines {
    standard: Arc<Engine>,
    strict: Arc<Engine>,
}

// adblock::Engine doesn't implement Debug, so we implement it manually.
impl std::fmt::Debug for FilterEngines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterEngines").finish_non_exhaustive()
    }
}

impl FilterEngines {
    /// Build engines from filter-list text.
    ///
    /// `custom_extra_texts` are user-configured lists applied on top of both
    /// Standard and Strict engines.
    pub fn build(
        standard_texts: &[&str],
        strict_extra_texts: &[&str],
        custom_extra_texts: &[&str],
    ) -> Self {
        let standard = Arc::new(build_engine(standard_texts, custom_extra_texts));
        let strict = Arc::new(build_engine(
            standard_texts,
            &[strict_extra_texts, custom_extra_texts].concat(),
        ));
        Self { standard, strict }
    }

    /// Returns a [`ContentFilterFn`] for the given protection level.
    pub fn filter_fn(self: &Arc<Self>, level: CookieProtectionLevel) -> ContentFilterFn {
        let engine = match level {
            CookieProtectionLevel::Standard => Arc::clone(&self.standard),
            CookieProtectionLevel::Strict => Arc::clone(&self.strict),
        };
        Arc::new(
            move |url: &Url, resource_type: &str, source: Option<&str>| {
                should_block(&engine, url.as_str(), resource_type, source.unwrap_or(""))
            },
        )
    }
}

fn build_engine(primary: &[&str], extra: &[&str]) -> Engine {
    let mut set = FilterSet::new(false);
    let opts = ParseOptions::default();
    for text in primary.iter().chain(extra) {
        set.add_filter_list((*text).to_owned(), opts);
    }
    Engine::new_with_filter_set(set)
}

fn should_block(engine: &Engine, url: &str, resource_type: &str, source: &str) -> bool {
    // The HTTP method is unknown here — Servo's content-filter callback does not
    // report it. An unparseable method leaves it unset, which makes filters
    // carrying a `$method=` option never match while every other filter is
    // unaffected. Guessing "GET" would instead mismatch XHR and form POSTs.
    let Ok(req) = Request::new(url, source, resource_type, "") else {
        return false;
    };
    engine.check_network_request(&req).should_block()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tracker rule, an exception for one site, and a third-party-only rule.
    const LIST: &str = "\
||tracker.example^\n\
@@||tracker.example^$domain=allowed.example\n\
||cdn.example/pixel.gif^$third-party\n";

    fn engine() -> Engine {
        build_engine(&[LIST], &[])
    }

    #[test]
    fn a_matching_rule_blocks() {
        assert!(should_block(
            &engine(),
            "https://tracker.example/beacon.js",
            "script",
            "https://site.example/",
        ));
    }

    #[test]
    fn an_exception_overrides_the_block() {
        assert!(!should_block(
            &engine(),
            "https://tracker.example/beacon.js",
            "script",
            "https://allowed.example/",
        ));
    }

    #[test]
    fn an_unlisted_host_is_allowed() {
        assert!(!should_block(
            &engine(),
            "https://site.example/app.js",
            "script",
            "https://site.example/",
        ));
    }

    #[test]
    fn a_third_party_rule_ignores_same_origin_requests() {
        let engine = engine();
        assert!(should_block(
            &engine,
            "https://cdn.example/pixel.gif",
            "image",
            "https://site.example/",
        ));
        assert!(!should_block(
            &engine,
            "https://cdn.example/pixel.gif",
            "image",
            "https://cdn.example/",
        ));
    }

    #[test]
    fn a_malformed_url_is_not_blocked() {
        assert!(!should_block(&engine(), "not a url", "script", ""));
    }
}
