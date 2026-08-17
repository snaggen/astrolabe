//! Translucency for surfaces the browser draws on top of the window background.
//!
//! Everything else follows COSMIC without help from us: libcosmic asks the
//! compositor to blur behind the window when the desktop's frosted-windows
//! setting is on, paints the window background at the blur strength the desktop
//! chose, and sets [`Theme::transparent`] so themed widgets follow suit. Web
//! content stays opaque because the engine paints it.
//!
//! What does need saying is how a *card* relates to the page behind it. A card
//! sits on top of an already translucent background, so it takes a weaker blur —
//! and a weaker blur is more opaque, which is what keeps a card readable over a
//! busy desktop.
//!
//! [`Theme::transparent`]: libcosmic::Theme::transparent

use libcosmic::cosmic_theme::BlurStrength;
use libcosmic::iced::{Background, Border, Color};
use libcosmic::theme;
use libcosmic::widget::container;

/// Blur behind a card. Weaker than the desktop's default for windows
/// ([`BlurStrength::Medium`]), so cards read as more solid than the page.
const CARD_BLUR: BlurStrength = BlurStrength::VeryLow;

/// Opacity for a card, or `None` while the window is opaque (frosted windows
/// off, or a maximized window on a desktop that exempts those).
///
/// Never goes below the page's own opacity: a desktop set to a very weak blur
/// would otherwise leave cards more translucent than the page they sit on.
fn card_alpha(theme: &theme::Theme) -> Option<f32> {
    if !theme.transparent {
        return None;
    }
    let cosmic = theme.cosmic();
    let page_alpha = Color::from(cosmic.background(true).base).a;
    Some(cosmic.alpha_map.blurred_alpha(CARD_BLUR).max(page_alpha))
}

/// A card on the New Tab page: the themed card colours, kept more opaque than
/// the window background behind it.
///
/// Identical to libcosmic's own card style apart from that opacity.
pub fn card() -> theme::Container<'static> {
    theme::Container::custom(|theme| {
        let cosmic = theme.cosmic();
        let component = &cosmic.background(false).component;
        let mut base = Color::from(component.base);
        if let Some(alpha) = card_alpha(theme) {
            base.a = alpha;
        }
        container::Style {
            icon_color: Some(Color::from(component.on)),
            text_color: Some(Color::from(component.on)),
            background: Some(Background::Color(base)),
            border: Border {
                radius: cosmic.corner_radii.radius_s.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use libcosmic::cosmic_theme::AlphaMap;

    /// The point of giving cards their own blur strength: they must come out
    /// more opaque than a window background at the desktop's default strength.
    #[test]
    fn a_card_is_more_opaque_than_the_page_behind_it() {
        let map = AlphaMap::default();
        assert!(map.blurred_alpha(CARD_BLUR) > map.blurred_alpha(BlurStrength::default()));
    }
}
