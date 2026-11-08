//! The experimental web-platform features offered on the settings page: how
//! each is presented, which are on by default, and pushing the choice to the
//! engine.

use astrolabe_webview::ExperimentalFeature;

use crate::config::BrowserConfig;

/// The settings group a feature is listed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    WebApis,
    Graphics,
    Css,
    Performance,
}

impl Group {
    /// Every group, in display order.
    pub const ALL: &'static [Group] = &[
        Group::WebApis,
        Group::Graphics,
        Group::Css,
        Group::Performance,
    ];

    /// The heading shown above the group.
    pub fn title(self) -> &'static str {
        match self {
            Group::WebApis => "Web APIs",
            Group::Graphics => "Graphics",
            Group::Css => "CSS and layout",
            Group::Performance => "Performance measurement",
        }
    }
}

/// How a feature is presented on the settings page.
pub struct FeatureInfo {
    pub feature: ExperimentalFeature,
    pub group: Group,
    pub title: &'static str,
    pub description: &'static str,
}

const fn info(
    feature: ExperimentalFeature,
    group: Group,
    title: &'static str,
    description: &'static str,
) -> FeatureInfo {
    FeatureInfo {
        feature,
        group,
        title,
        description,
    }
}

/// Every feature, in display order within its group.
pub const CATALOGUE: &[FeatureInfo] = {
    use ExperimentalFeature as F;
    use Group as G;
    &[
        info(
            F::IntersectionObserver,
            G::WebApis,
            "IntersectionObserver",
            "Lets pages react when elements scroll into view. Used for lazy loading by many news and shopping sites.",
        ),
        info(
            F::IndexedDb,
            G::WebApis,
            "IndexedDB",
            "Structured storage that web apps use to work offline and keep data between visits.",
        ),
        info(
            F::ExecCommand,
            G::WebApis,
            "Rich text editing",
            "document.execCommand(), which many WYSIWYG editors need to accept typing and formatting.",
        ),
        info(
            F::AsyncClipboard,
            G::WebApis,
            "Async Clipboard API",
            "Lets pages read and write clipboard text through navigator.clipboard.",
        ),
        info(
            F::FontFace,
            G::WebApis,
            "CSS Font Loading API",
            "Lets pages load web fonts from script with FontFace.",
        ),
        info(
            F::ServiceWorker,
            G::WebApis,
            "Service workers",
            "Background scripts that let web apps work offline and cache their own resources.",
        ),
        info(
            F::CookieStore,
            G::WebApis,
            "Cookie Store API",
            "Asynchronous cookie access through cookieStore.",
        ),
        info(
            F::CredentialManagement,
            G::WebApis,
            "Credential Management API",
            "Lets sites store and request sign-in credentials through navigator.credentials.",
        ),
        info(
            F::EntriesApi,
            G::WebApis,
            "File and Directory Entries API",
            "Access to dropped folders and their contents.",
        ),
        info(
            F::Sanitizer,
            G::WebApis,
            "HTML Sanitizer API",
            "Built-in removal of unsafe markup from HTML that pages insert.",
        ),
        info(
            F::ProtocolHandlers,
            G::WebApis,
            "Protocol handlers",
            "Lets sites register as handlers for links such as mailto: with registerProtocolHandler().",
        ),
        info(
            F::WakeLock,
            G::WebApis,
            "Screen Wake Lock API",
            "Lets pages ask to keep the screen on, for example while showing a recipe.",
        ),
        info(
            F::VisualViewport,
            G::WebApis,
            "Visual Viewport API",
            "Tells pages which part of the layout is visible when zoomed.",
        ),
        info(
            F::WebAnimations,
            G::WebApis,
            "Web Animations API",
            "Script-driven animations with element.animate().",
        ),
        info(
            F::WebGl2,
            G::Graphics,
            "WebGL 2",
            "The newer WebGL version used by many 3D sites, maps and games.",
        ),
        info(
            F::WebGpu,
            G::Graphics,
            "WebGPU",
            "Modern GPU graphics and compute API.",
        ),
        info(
            F::OffscreenCanvas,
            G::Graphics,
            "OffscreenCanvas",
            "Canvas drawing outside the page, for example in workers.",
        ),
        info(
            F::CanvasCapture,
            G::Graphics,
            "Canvas capture",
            "Records a canvas as a video stream with captureStream().",
        ),
        info(
            F::ContainerQueries,
            G::Css,
            "Container queries",
            "Styles that adapt to the size of their container instead of the window.",
        ),
        info(
            F::CssColumns,
            G::Css,
            "Multi-column layout",
            "Flows text into several columns, as in newspapers.",
        ),
        info(
            F::FontVariations,
            G::Css,
            "Variable fonts",
            "Single font files with adjustable weight, width and other axes.",
        ),
        info(
            F::CssWritingModes,
            G::Css,
            "Writing modes",
            "Vertical text, as used for Chinese, Japanese and Korean.",
        ),
        info(
            F::EllipseCorners,
            G::Css,
            "Elliptical corners",
            "border-radius with different horizontal and vertical radii.",
        ),
        info(
            F::AdoptedStyleSheets,
            G::Css,
            "Constructable stylesheets",
            "Stylesheets created from script and shared through adoptedStyleSheets, used by web components.",
        ),
        info(
            F::MathMl,
            G::Css,
            "MathML",
            "Renders mathematical notation written in MathML.",
        ),
        info(
            F::LargestContentfulPaint,
            G::Performance,
            "Largest Contentful Paint",
            "Reports to pages when their main content has been drawn.",
        ),
        info(
            F::ContainerTiming,
            G::Performance,
            "Container Timing",
            "Reports to pages when parts of them have been drawn.",
        ),
    ]
};

/// Features on until the user changes them: those servoshell turns on with
/// `--enable-experimental-web-platform-features`.
const ON_BY_DEFAULT: &[ExperimentalFeature] = &[
    ExperimentalFeature::AsyncClipboard,
    ExperimentalFeature::ContainerQueries,
    ExperimentalFeature::CssColumns,
    ExperimentalFeature::EllipseCorners,
    ExperimentalFeature::ExecCommand,
    ExperimentalFeature::FontFace,
    ExperimentalFeature::FontVariations,
    ExperimentalFeature::IndexedDb,
    ExperimentalFeature::IntersectionObserver,
    ExperimentalFeature::OffscreenCanvas,
    ExperimentalFeature::ProtocolHandlers,
    ExperimentalFeature::Sanitizer,
    ExperimentalFeature::WebGl2,
    ExperimentalFeature::WebGpu,
];

/// The stored form of the default choice: the preference names of the
/// features on by default.
pub fn default_enabled() -> Vec<String> {
    ON_BY_DEFAULT
        .iter()
        .map(|f| f.pref_name().to_string())
        .collect()
}

/// Switches every feature on or off in the engine as `config` says.
///
/// Before the engine starts this sets what the first page load sees;
/// afterwards it takes effect on the next page load.
pub fn apply(config: &BrowserConfig) {
    for &feature in ExperimentalFeature::ALL {
        astrolabe_webview::set_experimental_feature(
            feature,
            config.experimental_feature_enabled(feature),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_lists_every_feature_once() {
        for &feature in ExperimentalFeature::ALL {
            let count = CATALOGUE.iter().filter(|i| i.feature == feature).count();
            assert_eq!(count, 1, "{feature:?}");
        }
        assert_eq!(CATALOGUE.len(), ExperimentalFeature::ALL.len());
    }

    #[test]
    fn every_group_has_features() {
        for &group in Group::ALL {
            assert!(CATALOGUE.iter().any(|i| i.group == group), "{group:?}");
        }
    }

    #[test]
    fn default_config_enables_exactly_the_default_features() {
        let config = BrowserConfig::default();
        for &feature in ExperimentalFeature::ALL {
            assert_eq!(
                config.experimental_feature_enabled(feature),
                ON_BY_DEFAULT.contains(&feature),
                "{feature:?}"
            );
        }
    }

    #[test]
    fn intersection_observer_is_on_by_default() {
        // Its absence breaks common news sites, which is why this page exists.
        assert!(
            BrowserConfig::default()
                .experimental_feature_enabled(ExperimentalFeature::IntersectionObserver)
        );
    }
}
