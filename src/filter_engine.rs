//! Adblock filter engine wrapping Brave's `adblock` crate.
//!
//! Builds one [`adblock::Engine`] per protection level and exposes a
//! [`ContentFilterFn`] suitable for [`cosmic_webview::ServoState::set_content_filter`].

use std::sync::Arc;

use adblock::Engine;
use adblock::lists::{FilterSet, ParseOptions};
use adblock::request::Request;
use cosmic_webview::ContentFilterFn;
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
        set.add_filter_list(text, opts);
    }
    Engine::from_filter_set(set, true)
}

fn should_block(engine: &Engine, url: &str, resource_type: &str, source: &str) -> bool {
    let Ok(req) = Request::new(url, source, resource_type) else {
        return false;
    };
    let result = engine.check_network_request(&req);
    result.matched && result.exception.is_none()
}
