use std::time::{Duration, Instant};

use crate::input_method::TextInputVisibility;

/// A protocol's Deactivate must not hide a field detected by another source.
#[derive(Default)]
pub struct AutoVisibility {
    active: [bool; 2],
    shown: bool,
    hide_at: Option<Instant>,
}

impl AutoVisibility {
    pub fn update(&mut self, source: usize, visibility: TextInputVisibility, now: Instant) {
        self.active[source] = visibility == TextInputVisibility::Show;
        if self.active.iter().any(|active| *active) {
            self.hide_at = None;
        } else if self.shown && self.hide_at.is_none() {
            // Focus-out often precedes focus-in from the other API.
            self.hide_at = Some(now + Duration::from_millis(180));
        }
    }

    pub fn poll(&mut self, now: Instant) -> Option<bool> {
        let show = self.active.iter().any(|active| *active);
        if show && !self.shown {
            self.shown = true;
            Some(true)
        } else if !show && self.hide_at.is_some_and(|deadline| now >= deadline) {
            self.hide_at = None;
            self.shown = false;
            Some(false)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wayland_deactivation_does_not_override_accessible_input() {
        let mut state = AutoVisibility::default();
        let now = Instant::now();
        state.update(0, TextInputVisibility::Show, now);
        assert_eq!(state.poll(now), Some(true));
        state.update(1, TextInputVisibility::Show, now);
        state.update(0, TextInputVisibility::Hide, now);
        assert_eq!(state.poll(now + Duration::from_secs(1)), None);
    }
    #[test]
    fn focus_transition_does_not_flicker_and_focus_loss_hides() {
        let mut state = AutoVisibility::default();
        let now = Instant::now();
        state.update(0, TextInputVisibility::Show, now);
        state.poll(now);
        state.update(0, TextInputVisibility::Hide, now);
        assert_eq!(state.poll(now), None);
        state.update(
            1,
            TextInputVisibility::Show,
            now + Duration::from_millis(60),
        );
        assert_eq!(state.poll(now + Duration::from_millis(200)), None);
        state.update(
            1,
            TextInputVisibility::Hide,
            now + Duration::from_millis(300),
        );
        assert_eq!(state.poll(now + Duration::from_millis(500)), Some(false));
    }
}
