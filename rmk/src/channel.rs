//! Exposed channels which can be used to share data across devices & processors

use core::future::poll_fn;

use embassy_sync::channel::{Channel, TrySendError};
#[cfg(feature = "_ble")]
use embassy_sync::signal::Signal;
pub use embassy_sync::{blocking_mutex, channel, pubsub, zerocopy_channel};
use embassy_time::Instant;
use rmk_types::connection::ConnectionType;
use usbd_hid::descriptor::{MediaKeyboardReport, SystemControlReport};
#[cfg(feature = "_ble")]
use {
    crate::ble::profile::BleProfileAction,
    rmk_types::{ble::BleState, led_indicator::LedIndicator},
};

#[cfg(all(feature = "vial", feature = "_ble"))]
use crate::VIAL_CHANNEL_SIZE;
#[cfg(feature = "steno")]
use crate::hid::StenoReport;
use crate::hid::{KeyboardReport, MouseReport, Report};
use crate::{REPORT_CHANNEL_SIZE, RawMutex};

/// One HID report together with the instant at which its producer entered the
/// transport path. The timestamp lets RTT diagnostics measure motion age at
/// the actual BLE notification rather than inferring it from queue length.
#[derive(Debug)]
pub struct QueuedReport {
    payload: QueuedReportPayload,
    enqueued_at: Instant,
}

/// A relative mouse report kept at the event's native width until the active
/// transport is ready to serialize it as one or more signed 8-bit HID reports.
/// Keeping one input event as one queue item prevents an extreme i16 delta from
/// filling the bounded HID queue with hundreds of pre-expanded reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WideMouseReport {
    pub(crate) buttons: u8,
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) wheel: i32,
    pub(crate) pan: i32,
}

#[derive(Debug)]
pub(crate) enum QueuedReportPayload {
    Hid(Report),
    WideMouse(WideMouseReport),
}

impl QueuedReport {
    pub(crate) fn new(report: Report) -> Self {
        Self {
            payload: QueuedReportPayload::Hid(report),
            enqueued_at: Instant::now(),
        }
    }

    pub(crate) fn new_wide_mouse(buttons: u8, x: i16, y: i16, wheel: i16, pan: i16) -> Self {
        Self {
            payload: QueuedReportPayload::WideMouse(WideMouseReport {
                buttons,
                x: i32::from(x),
                y: i32::from(y),
                wheel: i32::from(wheel),
                pan: i32::from(pan),
            }),
            enqueued_at: Instant::now(),
        }
    }

    /// Returns the wrapped HID report without consuming the queue item.
    pub fn report(&self) -> &Report {
        match &self.payload {
            QueuedReportPayload::Hid(report) => report,
            QueuedReportPayload::WideMouse(_) => panic!("wide mouse queue item is not a serialized HID report"),
        }
    }

    /// Consumes the queue item and returns its wrapped HID report.
    pub fn into_report(self) -> Report {
        match self.payload {
            QueuedReportPayload::Hid(report) => report,
            QueuedReportPayload::WideMouse(_) => panic!("wide mouse queue item is not a serialized HID report"),
        }
    }

    pub(crate) fn payload(&self) -> &QueuedReportPayload {
        &self.payload
    }

    pub(crate) fn into_payload(self) -> QueuedReportPayload {
        self.payload
    }

    pub(crate) fn enqueued_at(&self) -> Instant {
        self.enqueued_at
    }
}

type ReportChannel = Channel<RawMutex, QueuedReport, REPORT_CHANNEL_SIZE>;

/// Signal for LED indicator, used in BLE keyboards only since BLE receiving is not async
#[cfg(feature = "_ble")]
pub(crate) static LED_SIGNAL: Signal<RawMutex, LedIndicator> = Signal::new();

/// Drained by the USB HID writer task. Routed through `send_hid_report`
/// from the keyboard task and ad-hoc producers (e.g. steno chord output).
#[cfg(not(feature = "_no_usb"))]
pub static USB_REPORT_CHANNEL: ReportChannel = Channel::new();

/// Drained by the BLE HID writer task. Routed through `send_hid_report`.
#[cfg(feature = "_ble")]
pub static BLE_REPORT_CHANNEL: ReportChannel = Channel::new();

fn report_channel(transport: ConnectionType) -> Option<&'static ReportChannel> {
    match transport {
        #[cfg(not(feature = "_no_usb"))]
        ConnectionType::Usb => Some(&USB_REPORT_CHANNEL),
        #[cfg(feature = "_ble")]
        ConnectionType::Ble => Some(&BLE_REPORT_CHANNEL),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

fn active_report_channel() -> Option<(ConnectionType, &'static ReportChannel)> {
    let transport = crate::state::active_transport()?;
    report_channel(transport).map(|ch| (transport, ch))
}

fn report_destination() -> Option<(ConnectionType, &'static ReportChannel)> {
    if let Some(active) = active_report_channel() {
        return Some(active);
    }

    #[cfg(feature = "_ble")]
    if crate::state::current_ble_status().state == BleState::Sleeping {
        return Some((ConnectionType::Ble, &BLE_REPORT_CHANNEL));
    }

    None
}

/// Reports generated while no transport is selected are normally dropped.
/// During BLE idle sleep, reports are retained in the BLE queue. This lets the
/// keyboard processor handle the wake key's release and subsequent input while
/// the transport reconnects; the new BLE writer drains the ordered reports.
pub async fn send_hid_report(report: Report) {
    let is_mouse = matches!(&report, Report::MouseReport(_));
    enqueue_hid_report(QueuedReport::new(report), is_mouse).await;
}

/// Enqueue one native-width relative mouse event. Transport writers own HID
/// chunking, so the pointing processor never blocks while expanding an i16
/// delta into many i8 reports.
pub(crate) async fn send_hid_mouse_report(buttons: u8, x: i16, y: i16, wheel: i16, pan: i16) {
    enqueue_hid_report(QueuedReport::new_wide_mouse(buttons, x, y, wheel, pan), true).await;
}

async fn enqueue_hid_report(mut queued_report: QueuedReport, _diag_is_mouse: bool) {
    let Some((transport, ch)) = report_destination() else {
        return;
    };

    #[cfg(feature = "rtt_diag")]
    let diag_started = Instant::now();
    #[cfg(feature = "rtt_diag")]
    let mut diag_full_retries = 0u32;

    loop {
        match ch.try_send(queued_report) {
            Ok(()) => {
                #[cfg(feature = "rtt_diag")]
                if matches!(transport, ConnectionType::Ble) {
                    crate::rtt_diag::record_hid_enqueue(
                        _diag_is_mouse,
                        ch.len(),
                        diag_full_retries,
                        Instant::now().duration_since(diag_started).as_micros() as u32,
                        true,
                    );
                }
                return;
            }
            Err(TrySendError::Full(r)) => {
                queued_report = r;
                #[cfg(feature = "rtt_diag")]
                {
                    diag_full_retries = diag_full_retries.saturating_add(1);
                }
            }
        }

        poll_fn(|cx| ch.poll_ready_to_send(cx)).await;
        if crate::state::active_transport() != Some(transport) {
            #[cfg(feature = "rtt_diag")]
            if matches!(transport, ConnectionType::Ble) {
                crate::rtt_diag::record_hid_enqueue(
                    _diag_is_mouse,
                    ch.len(),
                    diag_full_retries,
                    Instant::now().duration_since(diag_started).as_micros() as u32,
                    false,
                );
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{QueuedReport, QueuedReportPayload, WideMouseReport};

    #[test]
    fn extreme_native_mouse_delta_stays_one_wide_queue_item() {
        let queued = QueuedReport::new_wide_mouse(3, i16::MAX, i16::MIN, 321, -654);

        let QueuedReportPayload::WideMouse(report) = queued.into_payload() else {
            panic!("native mouse delta must remain wide while queued");
        };
        assert_eq!(
            report,
            WideMouseReport {
                buttons: 3,
                x: i32::from(i16::MAX),
                y: i32::from(i16::MIN),
                wheel: 321,
                pan: -654,
            }
        );
    }
}

/// Drops the report when the active transport's queue is full or no
/// transport is selected. Use for producers where back-pressure would block
/// the matrix scan (e.g. steno chord output).
pub(crate) fn try_send_hid_report(report: Report) {
    if let Some((_, ch)) = active_report_channel() {
        let _ = ch.try_send(QueuedReport::new(report));
    }
}

/// Drains queued reports for `transport` and leaves an all-up report of every
/// type for its writer. Called on active-transport flips so the previous host
/// releases any pressed keys and buttons without replaying stale queued reports later.
pub(crate) fn clear_and_release_report_channel(transport: ConnectionType) {
    if let Some(ch) = report_channel(transport) {
        ch.clear();
        for report in [
            Report::KeyboardReport(KeyboardReport::default()),
            Report::MouseReport(MouseReport::default()),
            Report::MediaKeyboardReport(MediaKeyboardReport { usage_id: 0 }),
            Report::SystemControlReport(SystemControlReport { usage_id: 0 }),
            #[cfg(feature = "steno")]
            Report::StenoReport(StenoReport::default()),
        ] {
            let _ = ch.try_send(QueuedReport::new(report));
        }
    }
}

#[cfg(feature = "_ble")]
pub(crate) static BLE_PROFILE_CHANNEL: Channel<RawMutex, BleProfileAction, 1> = Channel::new();

/// Vial RX from BLE GATT `output_data` writes — one 32-byte chunk per write.
/// Pushed by `gatt_events_task`, drained by [`crate::ble::host::HostGattHandler::run`].
#[cfg(all(feature = "vial", feature = "_ble"))]
pub(crate) static VIAL_BLE_RX_CHANNEL: Channel<RawMutex, [u8; 32], VIAL_CHANNEL_SIZE> = Channel::new();

/// Rynk RX from the BLE `output_data` writes. The 512 B ring is ~2× one MTU's maximal payload.
#[cfg(all(feature = "rynk", feature = "_ble"))]
pub(crate) static RYNK_BLE_RX_PIPE: embassy_sync::pipe::Pipe<RawMutex, 512> = embassy_sync::pipe::Pipe::new();
