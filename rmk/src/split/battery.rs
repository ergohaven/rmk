use core::cell::Cell;

use embassy_sync::blocking_mutex::Mutex;
use rmk_types::battery::BatteryStatus;

use crate::RawMutex;

static PERIPHERAL_BATTERIES: Mutex<RawMutex, Cell<[BatteryStatus; crate::SPLIT_PERIPHERALS_NUM]>> =
    Mutex::new(Cell::new([BatteryStatus::Unavailable; crate::SPLIT_PERIPHERALS_NUM]));

pub(crate) fn update_peripheral_battery_status(id: usize, status: BatteryStatus) {
    PERIPHERAL_BATTERIES.lock(|cell| {
        let mut statuses = cell.get();
        if let Some(slot) = statuses.get_mut(id) {
            *slot = status;
            cell.set(statuses);
        }
    });
}

pub(crate) fn current_peripheral_battery_status(id: usize) -> BatteryStatus {
    PERIPHERAL_BATTERIES.lock(|cell| cell.get().get(id).copied().unwrap_or(BatteryStatus::Unavailable))
}
