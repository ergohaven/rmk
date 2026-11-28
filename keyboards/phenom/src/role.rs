use embassy_futures::select::{Either, select};
use rmk::split::PeripheralMatrixConfig;

pub async fn primary(
    configured: impl core::future::Future<Output = ()>,
    deadline: impl core::future::Future<Output = ()>,
) -> bool {
    matches!(select(configured, deadline).await, Either::First(()))
}

pub const fn module_ids(right: bool) -> (u8, u8) {
    (right as u8, 2 + right as u8)
}

/// The other (non-USB-primary) half's matrix window: absolute rows 0..10,
/// this half is the primary so the link events arrive local to the peripheral.
pub const fn matrix_config(right: bool, rows: usize, cols: usize) -> PeripheralMatrixConfig {
    PeripheralMatrixConfig {
        rows: (rows / 2) as u8,
        cols: cols as u8,
        row_offset: if right { (rows / 2) as u8 } else { 0 },
        col_offset: 0,
    }
}
