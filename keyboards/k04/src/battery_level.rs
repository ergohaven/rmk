//! Charge-aware K:04 battery level filtering.
//!
//! The board only exposes battery voltage and USB VBUS. While USB power is
//! present, the charger raises terminal voltage long before the cell is full,
//! so that voltage is not a trustworthy state-of-charge measurement.

const HYSTERESIS_PCT: u8 = 2;

#[derive(Default)]
pub(crate) struct BatteryLevelTracker {
    trusted_level: Option<u8>,
}

impl BatteryLevelTracker {
    /// Return the level that may safely be published for the current power
    /// state. Charging and a failed sample keep the last discharging
    /// measurement; a cold boot on USB reports an unknown level until the
    /// first unplugged sample.
    pub(crate) fn observe(&mut self, measured: Option<u8>, charging: bool) -> Option<u8> {
        if charging {
            return self.trusted_level;
        }

        let Some(next) = measured else {
            return self.trusted_level;
        };
        let filtered = match self.trusted_level {
            Some(current) if next != 0 && next != 100 && next.abs_diff(current) < HYSTERESIS_PCT => current,
            _ => next,
        };
        self.trusted_level = Some(filtered);
        Some(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_boot_while_charging_does_not_claim_full() {
        let mut tracker = BatteryLevelTracker::default();

        assert_eq!(tracker.observe(Some(100), true), None);
    }

    #[test]
    fn charging_keeps_the_last_trusted_discharging_level() {
        let mut tracker = BatteryLevelTracker::default();

        assert_eq!(tracker.observe(Some(74), false), Some(74));
        assert_eq!(tracker.observe(Some(100), true), Some(74));
        assert_eq!(tracker.observe(Some(98), true), Some(74));
    }

    #[test]
    fn unplugging_refreshes_the_level_from_voltage() {
        let mut tracker = BatteryLevelTracker::default();

        assert_eq!(tracker.observe(Some(73), false), Some(73));
        assert_eq!(tracker.observe(Some(100), true), Some(73));
        assert_eq!(tracker.observe(Some(79), false), Some(79));
    }

    #[test]
    fn failed_sample_keeps_the_last_trusted_level() {
        let mut tracker = BatteryLevelTracker::default();

        assert_eq!(tracker.observe(Some(74), false), Some(74));
        assert_eq!(tracker.observe(None, false), Some(74));
        assert_eq!(tracker.observe(Some(70), false), Some(70));
    }

    #[test]
    fn failed_first_sample_stays_unknown() {
        let mut tracker = BatteryLevelTracker::default();

        assert_eq!(tracker.observe(None, false), None);
    }

    #[test]
    fn discharging_measurements_keep_small_change_hysteresis() {
        let mut tracker = BatteryLevelTracker::default();

        assert_eq!(tracker.observe(Some(75), false), Some(75));
        assert_eq!(tracker.observe(Some(74), false), Some(75));
        assert_eq!(tracker.observe(Some(73), false), Some(73));
    }
}
