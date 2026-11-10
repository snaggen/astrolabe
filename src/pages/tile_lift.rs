//! Hover animation for New Tab grid cells: the hovered cell lifts towards the
//! pointer by growing slightly, and settles back when the pointer leaves.

use std::collections::HashMap;
use std::time::Duration;

use libcosmic::iced::Animation;
use libcosmic::iced::animation::Easing;
use libcosmic::iced::time::Instant;

/// Time a cell takes to lift or settle.
const LIFT_DURATION: Duration = Duration::from_millis(160);

/// Lift animations of the New Tab grid cells, keyed by URL.
///
/// Only cells that are lifted or still settling have an entry, so the map stays
/// as small as the number of cells the pointer recently crossed.
#[derive(Debug)]
pub struct TileLift {
    animations: HashMap<String, Animation<bool>>,
    /// Time the view interpolates at, advanced on every animation frame.
    now: Instant,
}

impl Default for TileLift {
    fn default() -> Self {
        Self {
            animations: HashMap::new(),
            now: Instant::now(),
        }
    }
}

impl TileLift {
    /// Start lifting the cell for `url`.
    pub fn lift(&mut self, url: String, at: Instant) {
        self.now = at;
        self.animations
            .entry(url)
            .or_insert_with(new_animation)
            .go_mut(true, at);
    }

    /// Start settling the cell for `url` back to its resting size.
    pub fn settle(&mut self, url: &str, at: Instant) {
        self.now = at;
        if let Some(animation) = self.animations.get_mut(url) {
            animation.go_mut(false, at);
        }
    }

    /// Advance the animation clock, dropping cells that have settled.
    pub fn tick(&mut self, at: Instant) {
        self.now = at;
        self.animations
            .retain(|_, animation| animation.value() || animation.is_animating(at));
    }

    /// Whether any cell is still lifting or settling at `at`.
    pub fn is_animating(&self, at: Instant) -> bool {
        self.animations.values().any(|a| a.is_animating(at))
    }

    /// How far the cell for `url` is lifted: 0.0 at rest, 1.0 fully lifted.
    pub fn progress(&self, url: &str) -> f32 {
        self.animations
            .get(url)
            .map_or(0.0, |a| a.interpolate(0.0, 1.0, self.now))
    }
}

fn new_animation() -> Animation<bool> {
    Animation::new(false)
        .duration(LIFT_DURATION)
        .easing(Easing::EaseOut)
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.com/";

    #[test]
    fn an_unknown_cell_rests_at_normal_size() {
        let lift = TileLift::default();
        assert_eq!(lift.progress(URL), 0.0);
    }

    #[test]
    fn a_hovered_cell_is_fully_lifted_once_the_animation_ends() {
        let start = Instant::now();
        let mut lift = TileLift::default();
        lift.lift(URL.to_string(), start);
        assert!(lift.is_animating(start + LIFT_DURATION / 2));

        lift.tick(start + LIFT_DURATION * 2);
        assert!(!lift.is_animating(start + LIFT_DURATION * 2));
        assert!((lift.progress(URL) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_cell_is_partly_lifted_halfway_through() {
        let start = Instant::now();
        let mut lift = TileLift::default();
        lift.lift(URL.to_string(), start);
        lift.tick(start + LIFT_DURATION / 2);
        let progress = lift.progress(URL);
        assert!(progress > 0.0 && progress < 1.0, "progress {progress}");
    }

    #[test]
    fn a_settled_cell_is_dropped() {
        let start = Instant::now();
        let mut lift = TileLift::default();
        lift.lift(URL.to_string(), start);
        let lifted = start + LIFT_DURATION * 2;
        lift.tick(lifted);
        lift.settle(URL, lifted);

        // Still settling: the entry stays so the cell can shrink smoothly.
        lift.tick(lifted + LIFT_DURATION / 2);
        assert!(lift.animations.contains_key(URL));

        lift.tick(lifted + LIFT_DURATION * 2);
        assert!(lift.animations.is_empty());
        assert_eq!(lift.progress(URL), 0.0);
    }

    #[test]
    fn settling_an_unknown_cell_does_nothing() {
        let mut lift = TileLift::default();
        lift.settle(URL, Instant::now());
        assert!(lift.animations.is_empty());
    }
}
