// Relative Z on touch sources: 1/2 remain legacy click pulses; 0x100/0x101
// carry released/held state without changing the PointingEvent wire layout.
const HOLD_REFRESH_MS: u32 = 100;

#[derive(Default)]
pub struct TouchpadHold {
    held: bool,
    last_report_ms: u32,
}

impl TouchpadHold {
    pub fn update(&mut self, enabled: bool, gesture: u8, fingers: u8, now_ms: u32) -> Option<i16> {
        let held = enabled && gesture & 2 != 0 && fingers == 1;
        let previous = self.held;
        self.held = held;
        // Refresh stationary holds below the receiver's 500 ms timeout,
        // without interleaving every motion event with a split-wire barrier.
        if held != previous || (held && now_ms.wrapping_sub(self.last_report_ms) >= HOLD_REFRESH_MS) {
            self.last_report_ms = now_ms;
            Some(0x100 | i16::from(held))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_refreshes_until_release_without_turning_taps_into_holds() {
        let mut hold = TouchpadHold::default();
        assert_eq!(hold.update(true, 1, 0, 0), None);
        assert_eq!(hold.update(true, 2, 1, 0), Some(0x101));
        assert_eq!(hold.update(true, 2, 1, 100), Some(0x101));
        assert_eq!(hold.update(true, 0, 0, 101), Some(0x100));
        assert_eq!(hold.update(true, 0, 0, 102), None);
    }

    #[test]
    fn disabled_gestures_second_finger_and_reset_release_the_hold() {
        for (enabled, gesture, fingers) in [(false, 2, 1), (true, 2, 2), (true, 0, 1)] {
            let mut hold = TouchpadHold::default();
            assert_eq!(hold.update(true, 2, 1, 0), Some(0x101));
            assert_eq!(hold.update(enabled, gesture, fingers, 8), Some(0x100));
        }
        let mut hold = TouchpadHold::default();
        hold.update(true, 2, 1, 0);
        assert_eq!(hold.update(false, 0, 0, 0), Some(0x100));
        assert_eq!(hold.update(false, 0, 0, 0), None);
    }

    #[test]
    fn hold_refreshes_at_100ms_without_delaying_edges_or_wrapping_time() {
        let mut hold = TouchpadHold::default();
        assert_eq!(hold.update(true, 2, 1, u32::MAX - 50), Some(0x101));
        for elapsed in 1..100 {
            assert_eq!(hold.update(true, 2, 1, (u32::MAX - 50).wrapping_add(elapsed)), None);
        }
        assert_eq!(hold.update(true, 2, 1, 49), Some(0x101));
        assert_eq!(hold.update(true, 0, 0, 50), Some(0x100));
        assert_eq!(hold.update(true, 2, 1, 51), Some(0x101));
    }
}
