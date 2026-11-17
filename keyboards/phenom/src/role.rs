use embassy_futures::select::{Either, select};
use rmk::event::{KeyboardEvent, KeyboardEventSource};
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

pub const fn matrix_config(right: bool, rows: usize, cols: usize) -> PeripheralMatrixConfig {
    PeripheralMatrixConfig {
        rows: (rows / 2) as u8,
        cols: cols as u8,
        row_offset: if right { (rows / 2) as u8 } else { 0 },
        col_offset: 0,
    }
}

pub fn local_matrix_event(mut event: KeyboardEvent, local: PeripheralMatrixConfig) -> Option<KeyboardEvent> {
    if let KeyboardEventSource::Key(mut pos) = event.source {
        pos.row = pos.row.checked_sub(local.row_offset)?;
        pos.col = pos.col.checked_sub(local.col_offset)?;
        if pos.row >= local.rows || pos.col >= local.cols {
            return None;
        }
        event.source = KeyboardEventSource::Key(pos);
    }
    Some(event)
}
