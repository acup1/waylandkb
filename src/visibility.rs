use std::time::{Duration, Instant};

use crate::input_method::TextInputVisibility;

/// A protocol's Deactivate must not hide a field detected by another source.
pub struct AutoVisibility {
    enabled: bool,
    active: [bool; 2],
    shown: bool,
    hide_at: Option<Instant>,
}

impl Default for AutoVisibility {
    fn default() -> Self {
        Self {
            enabled: true,
            active: [false; 2],
            shown: false,
            hide_at: None,
        }
    }
}

impl AutoVisibility {
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        // Do not change manual visibility when switching modes. Cancel pending
        // hides, but retain live focus so enabling can show an already focused input.
        self.shown = false;
        self.hide_at = None;
    }

    pub fn update(&mut self, source: usize, visibility: TextInputVisibility, now: Instant) {
        self.active[source] = visibility == TextInputVisibility::Show;
        if !self.enabled {
            return;
        }
        if self.active.iter().any(|active| *active) {
            self.hide_at = None;
        } else if self.shown && self.hide_at.is_none() {
            // Focus-out often precedes focus-in from the other API.
            self.hide_at = Some(now + Duration::from_millis(180));
        }
    }

    pub fn poll(&mut self, now: Instant) -> Option<bool> {
        if !self.enabled {
            return None;
        }
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
    fn auto_visibility_is_enabled_by_default() {
        assert!(AutoVisibility::default().is_enabled());
    }

    #[test]
    fn disabled_focus_events_never_change_manual_visibility() {
        let mut state = AutoVisibility::default();
        let now = Instant::now();
        state.set_enabled(false);
        for source in 0..2 {
            state.update(source, TextInputVisibility::Show, now);
            assert_eq!(state.poll(now), None);
        }
        for source in 0..2 {
            state.update(source, TextInputVisibility::Hide, now);
            assert_eq!(state.poll(now + Duration::from_secs(1)), None);
        }
        assert!(!state.is_enabled());
    }

    #[test]
    fn enabling_uses_current_focus_without_waiting_for_another_event() {
        let mut state = AutoVisibility::default();
        let now = Instant::now();
        state.set_enabled(false);
        state.update(0, TextInputVisibility::Show, now);
        state.update(1, TextInputVisibility::Show, now);
        state.update(0, TextInputVisibility::Hide, now);
        state.set_enabled(true);
        assert_eq!(state.poll(now), Some(true));
        assert_eq!(state.poll(now), None);
        state.update(1, TextInputVisibility::Hide, now);
        assert_eq!(state.poll(now + Duration::from_secs(1)), Some(false));
    }

    #[test]
    fn disabling_cancels_pending_hide_even_after_reenabling() {
        let mut state = AutoVisibility::default();
        let now = Instant::now();
        state.update(0, TextInputVisibility::Show, now);
        assert_eq!(state.poll(now), Some(true));
        state.update(0, TextInputVisibility::Hide, now);
        state.set_enabled(false);
        assert_eq!(state.poll(now + Duration::from_secs(1)), None);
        state.set_enabled(true);
        assert_eq!(state.poll(now + Duration::from_secs(2)), None);
    }

    #[test]
    fn focus_lost_while_disabled_is_not_reopened_on_enable() {
        let mut state = AutoVisibility::default();
        let now = Instant::now();
        state.update(0, TextInputVisibility::Show, now);
        assert_eq!(state.poll(now), Some(true));
        state.set_enabled(false);
        state.update(0, TextInputVisibility::Hide, now);
        state.set_enabled(true);
        assert_eq!(state.poll(now), None);
    }

    #[test]
    fn setting_the_same_mode_preserves_pending_hide() {
        let mut state = AutoVisibility::default();
        let now = Instant::now();
        state.update(0, TextInputVisibility::Show, now);
        state.poll(now);
        state.update(0, TextInputVisibility::Hide, now);
        state.set_enabled(true);
        assert_eq!(state.poll(now + Duration::from_secs(1)), Some(false));
    }

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
