#[cfg(feature = "subrating")]
use bt_hci::cmd::le::LeSubrateRequest;
use bt_hci::cmd::le::{LeReadLocalSupportedFeatures, LeReadPhy, LeSetPhy};
use bt_hci::controller::{ControllerCmdAsync, ControllerCmdSync};
use bt_hci::param::Error as HciError;
#[cfg(feature = "split")]
use embassy_futures::join::join;
use embassy_futures::join::{join3, join4};
use embassy_futures::select::{Either, Either4, select, select4};
#[cfg(feature = "split")]
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
#[cfg(feature = "split")]
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
#[cfg(feature = "host_first_split_wake")]
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use rmk_types::ble::BleState;
use rmk_types::connection::ConnectionType;
use rmk_types::led_indicator::LedIndicator;
use trouble_host::prelude::appearance::human_interface_device::KEYBOARD;
use trouble_host::prelude::service::{BATTERY, HUMAN_INTERFACE_DEVICE};
use trouble_host::prelude::*;

#[cfg(feature = "dongle")]
use crate::ble::adv::{Adv, advertise};
use crate::ble::battery_service::BleBatteryServer;
#[cfg(feature = "split")]
use crate::ble::battery_service::BlePeripheralBatteryServer;
use crate::ble::ble_server::{BleHidServer, Server};
use crate::ble::device_info::{PnPID, VidSource};
#[cfg(feature = "host")]
use crate::ble::host::{HOST_WRITE_BUFFER_SIZE, HostGattHandler, HostWriteOutcome};
use crate::ble::led::BleLedReader;
#[cfg(feature = "passkey_entry")]
use crate::ble::passkey::{PasskeyInputState, next_gatt_event};
use crate::ble::profile::{BOND_SLOTS, ProfileInfo, ProfileManager};
use crate::ble::sleep::{
    InputActivityWaiter, report_activity, request_local_sleep, request_sleep, reset_host_power_input,
    take_host_power_input, wait_for_host_power_input,
};
use crate::channel::{BLE_REPORT_CHANNEL, LED_SIGNAL, QueuedReportPayload, WideMouseReport};
use crate::config::{BleBatteryConfig, BleHostPowerConfig, DeviceConfig, RmkConfig};
use crate::core_traits::Runnable;
use crate::event::BleAdvertisingMode;
use crate::hid::{HidWriterTrait, MouseReport, run_led_reader};
#[cfg(feature = "split")]
use crate::split::PeripheralMatrixConfig;
#[cfg(feature = "split")]
use crate::split::ble::central::{run_peripheral_session, scan_and_connect_peripherals};
use crate::state::set_ble_state;

pub(crate) mod adv;
pub(crate) mod battery_service;
pub(crate) mod ble_server;
pub(crate) mod device_info;
#[cfg(feature = "host")]
pub(crate) mod host;
pub(crate) mod led;
pub mod passkey;
pub(crate) mod profile;
#[cfg(any(feature = "split", feature = "dongle"))]
pub(crate) mod scan;
pub(crate) mod sleep;

#[cfg(all(feature = "subrating", feature = "_no_subrating"))]
compile_error!("You may not enable feature `subrating` on unsupported platforms!");

/// Max number of connections of a keyboard's BLE stack; a dongle sizes its
/// own — see [`crate::dongle::Dongle`].
pub(crate) const CONNECTIONS_MAX: usize = crate::SPLIT_PERIPHERALS_NUM + 1;

/// Max number of L2CAP channels
pub(crate) const L2CAP_CHANNELS_MAX: usize = CONNECTIONS_MAX * 4; // Signal + att + smp + hid

// Custom messages share the GATT write dispatcher with the host protocol.
const GATT_WRITE_BUFFER_SIZE: usize = {
    #[cfg(feature = "host")]
    let size = HOST_WRITE_BUFFER_SIZE;
    #[cfg(not(feature = "host"))]
    let size = 32;
    #[cfg(all(feature = "dongle", feature = "custom_message"))]
    let size = {
        let custom =
            <crate::custom_message::CustomMessage as postcard::experimental::max_size::MaxSize>::POSTCARD_MAX_SIZE;
        if custom > size { custom } else { size }
    };
    size
};

// High-duty directed advertising terminates in the controller at roughly
// 1.28 s. Starting filtered undirected advertising on the same boundary can
// race that termination and make nrf-sdc return HCI Command Disallowed, then
// panic. Use the already bond-filtered undirected path from the first packet.
const DIRECTED_RECONNECT_WINDOW_MS: u64 = 0;
const FAST_BONDED_RECONNECT_TOTAL_MS: u64 = 5_000;
const FAST_ADVERTISING_TIMEOUT_SECS: u64 = 30;
/// Advertising window of the dongle profile.
#[cfg(feature = "dongle")]
const DONGLE_ADVERTISING_TIMEOUT_SECS: u64 = 300;
const HOST_PHY_UPDATE_ATTEMPTS: u8 = 3;
const HOST_PHY_UPDATE_SETTLE_MS: u64 = 80;
const HOST_CONNECTION_LIVENESS_POLL_MS: u64 = 250;
const HOST_DISCONNECT_EVENT_TIMEOUT_MS: u64 = 750;
const HOST_SESSION_RELEASE_GRACE_MS: u64 = 100;
const HOST_SESSION_RELEASE_SETTLE_MS: u64 = 100;
const HOST_CONN_PARAM_UPDATE_TIMEOUT_SECS: u64 = 2;
#[cfg(feature = "host_first_split_wake")]
const HOST_ACTIVE_CONN_PARAM_ATTEMPTS: u8 = 2;
#[cfg(feature = "host_first_split_wake")]
const HOST_ACTIVE_CONN_PARAM_RETRY_MS: u64 = 100;
#[cfg(feature = "host_fixed_15ms")]
const HOST_FIXED_CONN_PARAM_ATTEMPTS: u8 = 3;
#[cfg(feature = "host_fixed_15ms")]
const HOST_FIXED_CONN_PARAM_RETRY_MS: u64 = 250;
const HID_WRITE_TIMEOUT_SECS: u64 = 2;
#[cfg(all(
    feature = "mouse_interval_control",
    feature = "mouse_vector_preserve",
    not(any(feature = "host_fixed_15ms", feature = "fixed_mouse_pacing_15ms"))
))]
const MOUSE_CONTROL_INTERVAL: Duration = Duration::from_micros(7_500);
#[cfg(all(
    feature = "mouse_interval_control",
    any(
        not(feature = "mouse_vector_preserve"),
        feature = "host_fixed_15ms",
        feature = "fixed_mouse_pacing_15ms"
    )
))]
const MOUSE_CONTROL_INTERVAL: Duration = Duration::from_millis(15);
#[cfg(not(feature = "host_first_split_wake"))]
const HOST_IDLE_MAX_LATENCY: u16 = 30;
#[cfg(feature = "host_first_split_wake")]
const HOST_IDLE_MAX_LATENCY: u16 = 4;
#[cfg(feature = "host_first_split_wake")]
const HOST_LOW_DUTY_EFFECTIVE_INTERVAL_US: u64 = 150_000;
const HOST_INTERACTIVE_MAX_LATENCY: u16 = 0;
const VIAL_LINK_IDLE_TIMEOUT_SECS: u64 = 30;
const HCI_LINK_UPDATE_ATTEMPTS: u8 = 12;
const HCI_LINK_UPDATE_RETRY_MS: u64 = 20;

// The controller accepts only one link-control procedure at a time. Host PHY
// updates and one or more split links share it, so serialize our commands
// before handling controller-level collisions from procedures started by the
// peer or stack itself.
static BLE_HCI_LINK_UPDATE_MUTEX: Mutex<crate::RawMutex, ()> = Mutex::new(());
/// Host-protocol (Vial/Rynk) traffic, which switches the link to zero latency.
#[cfg(feature = "host")]
static HOST_BLE_ACTIVITY: Signal<crate::RawMutex, ()> = Signal::new();

/// Wakes the connected host-power task when a runtime policy changes.
static HOST_POWER_CONFIG_CHANGED: Signal<crate::RawMutex, ()> = Signal::new();

/// Latched cross-link ordering state for an opt-in keyboard-wide wake.
#[cfg(feature = "host_first_split_wake")]
static HOST_WAKE_ORDER_GATE: Watch<crate::RawMutex, HostWakeOrderGate, CONNECTIONS_MAX> =
    Watch::new_with(HostWakeOrderGate::Open);

#[cfg(feature = "host_first_split_wake")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostWakeOrderGate {
    Open,
    Pending,
}

#[cfg(feature = "host_first_split_wake")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostWakeOrderEvent {
    HostEnteredIdle,
    HostActiveConfirmed,
    SessionEnded,
}

#[cfg(feature = "host_first_split_wake")]
const fn next_host_wake_order_gate(_current: HostWakeOrderGate, event: HostWakeOrderEvent) -> HostWakeOrderGate {
    match event {
        HostWakeOrderEvent::HostEnteredIdle => HostWakeOrderGate::Pending,
        HostWakeOrderEvent::HostActiveConfirmed | HostWakeOrderEvent::SessionEnded => HostWakeOrderGate::Open,
    }
}

#[cfg(feature = "host_first_split_wake")]
fn set_host_wake_order_gate_on<const N: usize>(
    gate: &Watch<crate::RawMutex, HostWakeOrderGate, N>,
    event: HostWakeOrderEvent,
) {
    let current = gate.try_get().unwrap_or(HostWakeOrderGate::Open);
    gate.sender().send(next_host_wake_order_gate(current, event));
}

#[cfg(feature = "host_first_split_wake")]
pub(crate) fn host_wake_order_gate() -> HostWakeOrderGate {
    HOST_WAKE_ORDER_GATE.try_get().unwrap_or(HostWakeOrderGate::Open)
}

#[cfg(feature = "host_first_split_wake")]
async fn wait_for_host_wake_order_gate_on<const N: usize>(gate: &Watch<crate::RawMutex, HostWakeOrderGate, N>) {
    let Some(mut receiver) = gate.receiver() else {
        // Capacity is sized for every possible connection. Fail open if a
        // future topology violates that invariant rather than deadlocking.
        warn!("[WAKE_ORDER_V30G] split_waiter_capacity_exhausted gate=bypass");
        return;
    };
    receiver.get_and(|state| *state == HostWakeOrderGate::Open).await;
}

#[cfg(feature = "host_first_split_wake")]
pub(crate) async fn wait_for_host_wake_order_gate() {
    wait_for_host_wake_order_gate_on(&HOST_WAKE_ORDER_GATE).await;
}

/// Opens the gate when a host-power task is cancelled or leaves its session.
#[cfg(feature = "host_first_split_wake")]
struct HostWakeOrderSession<'a, const N: usize> {
    gate: &'a Watch<crate::RawMutex, HostWakeOrderGate, N>,
    pending: bool,
}

#[cfg(feature = "host_first_split_wake")]
impl<'a, const N: usize> HostWakeOrderSession<'a, N> {
    fn new_on(gate: &'a Watch<crate::RawMutex, HostWakeOrderGate, N>) -> Self {
        set_host_wake_order_gate_on(gate, HostWakeOrderEvent::SessionEnded);
        Self { gate, pending: false }
    }

    fn close_for_idle(&mut self) {
        self.pending = true;
        set_host_wake_order_gate_on(self.gate, HostWakeOrderEvent::HostEnteredIdle);
        info!("[WAKE_ORDER_V30G] event=host_idle gate=closed");
    }

    fn open_after_confirmation(&mut self, applied: HostConnParamsSnapshot) {
        self.pending = false;
        set_host_wake_order_gate_on(self.gate, HostWakeOrderEvent::HostActiveConfirmed);
        info!(
            "[WAKE_ORDER_V30G] event=host_active_confirmed gate=open interval_us={} latency={}",
            applied.interval.as_micros(),
            applied.latency
        );
    }
}

#[cfg(feature = "host_first_split_wake")]
impl HostWakeOrderSession<'static, CONNECTIONS_MAX> {
    fn new() -> Self {
        Self::new_on(&HOST_WAKE_ORDER_GATE)
    }
}

#[cfg(feature = "host_first_split_wake")]
impl<const N: usize> Drop for HostWakeOrderSession<'_, N> {
    fn drop(&mut self) {
        if self.pending {
            warn!("[WAKE_ORDER_V30G] event=host_session_end gate=open reason=teardown_or_failure");
        }
        set_host_wake_order_gate_on(self.gate, HostWakeOrderEvent::SessionEnded);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HostConnParamsSnapshot {
    interval: Duration,
    latency: u16,
}

/// Carries the parameters that the controller actually applied. This lets the
/// bootstrap task distinguish a host that accepted 7.5 ms from an Apple host
/// that retained 15 ms and would otherwise keep the preceding slave latency.
static HOST_CONN_PARAMS_UPDATED: Signal<crate::RawMutex, HostConnParamsSnapshot> = Signal::new();

/// Fixed pacing starts a fresh 15 ms slot after every completed HID write.
#[cfg(feature = "fixed_mouse_pacing_15ms")]
fn fixed_mouse_pacing_deadline(completed_at: Instant) -> Instant {
    completed_at + MOUSE_CONTROL_INTERVAL
}

/// Notify the BLE transport that its runtime host-power policy changed.
pub fn notify_host_power_config_changed() {
    HOST_POWER_CONFIG_CHANGED.signal(());
}

/// BLE transport. Owns the whole BLE stack.
///
/// On a split build the transport is the BLE split central:
/// `run` also loads the peripherals' stored addresses and drives the
/// radio and per-peripheral session tasks on the same stack.
pub struct BleTransport<'a, C>
where
    C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>,
{
    /// Taken by `run`: the stack it builds consumes the controller.
    /// The option can be removed by changing to `run(self)`, but it requires
    /// https://github.com/rust-lang/rust/issues/59087 to be fixed
    controller: Option<C>,
    address: [u8; 6],
    device_config: DeviceConfig<'static>,
    config: BleBatteryConfig,
    host_power_config: Option<BleHostPowerConfig>,
    /// One matrix region per split peripheral.
    #[cfg(feature = "split")]
    peripheral_matrices: [PeripheralMatrixConfig; crate::SPLIT_PERIPHERALS_NUM],
    #[cfg(feature = "host")]
    host_service: Option<&'a crate::host::HostService<'a>>,
    // Keeps `'a` in the type's parameter list across all feature configurations.
    #[cfg(not(feature = "host"))]
    _phantom: core::marker::PhantomData<&'a ()>,
}

impl<'a, C> BleTransport<'a, C>
where
    C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>,
{
    pub fn new(
        controller: C,
        address: [u8; 6],
        rmk_config: RmkConfig<'static>,
        #[cfg(feature = "split")] peripheral_matrices: [PeripheralMatrixConfig; crate::SPLIT_PERIPHERALS_NUM],
    ) -> Self {
        Self {
            controller: Some(controller),
            address,
            device_config: rmk_config.device_config,
            config: rmk_config.ble_battery_config,
            host_power_config: None,
            #[cfg(feature = "split")]
            peripheral_matrices,
            #[cfg(feature = "host")]
            host_service: None,
            #[cfg(not(feature = "host"))]
            _phantom: core::marker::PhantomData,
        }
    }

    /// Attach the host-protocol service (Vial or Rynk, picked at compile
    /// time by feature). See
    /// [`UsbTransport::with_host_service`](crate::usb::UsbTransport::with_host_service).
    #[cfg(feature = "host")]
    pub fn with_host_service(mut self, service: &'a crate::host::HostService<'a>) -> Self {
        self.host_service = Some(service);
        self
    }

    /// Apply a host-link power policy: low-duty connection parameters after
    /// its idle timeout, then a full disconnect until local input.
    pub fn with_host_power_config(mut self, host_power_config: BleHostPowerConfig) -> Self {
        self.host_power_config = Some(host_power_config);
        self
    }

    /// Set the initial cadence of each split link; a board can still switch
    /// one at runtime with
    /// [`set_split_link_profile`](crate::split::ble::central::set_split_link_profile).
    #[cfg(feature = "split")]
    pub fn with_split_link_profiles(
        self,
        profiles: [crate::split::ble::central::SplitLinkProfile; crate::SPLIT_PERIPHERALS_NUM],
    ) -> Self {
        for (peripheral_id, profile) in profiles.into_iter().enumerate() {
            crate::split::ble::central::set_split_link_profile(peripheral_id, profile);
        }
        self
    }
}

#[cfg(not(feature = "split"))]
impl<'a, C> Runnable for BleTransport<'a, C>
where
    C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>,
{
    async fn run(&mut self) -> ! {
        // Load the preferred connection from storage
        let preferred = crate::state::load_preferred_connection().await;
        crate::state::set_preferred_connection(preferred);

        let controller = self.controller.take().expect("BleTransport::run called twice");
        // Exactly one link — the host.
        let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> = HostResources::new();
        let stack = trouble_host::new(controller, &mut resources)
            .set_random_address(Address::random(self.address))
            .build();
        run_ble_keyboard(
            &stack,
            &self.device_config,
            &self.config,
            self.host_power_config,
            #[cfg(feature = "host")]
            self.host_service,
        )
        .await
    }
}

#[cfg(feature = "split")]
impl<
    'a,
    #[cfg(not(feature = "subrating"))] C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>
        + ControllerCmdSync<bt_hci::cmd::le::LeSetScanParams>,
    #[cfg(feature = "subrating")] C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>
        + ControllerCmdSync<bt_hci::cmd::le::LeSetScanParams>
        + ControllerCmdAsync<LeSubrateRequest>,
> Runnable for BleTransport<'a, C>
{
    async fn run(&mut self) -> ! {
        // Load the preferred connection from storage
        let preferred = crate::state::load_preferred_connection().await;
        crate::state::set_preferred_connection(preferred);

        let controller = self.controller.take().expect("BleTransport::run called twice");

        let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> = HostResources::new();
        let stack = trouble_host::new(controller, &mut resources)
            .set_random_address(Address::random(self.address))
            .build();

        // The connect peripherals task hands each established connection to its
        // session task, and a session task reports back when its session ends.
        let conn_channels: [Channel<NoopRawMutex, Connection<'_, DefaultPacketPool>, 1>; crate::SPLIT_PERIPHERALS_NUM] =
            core::array::from_fn(|_| Channel::new());
        let ended: Channel<NoopRawMutex, usize, { crate::SPLIT_PERIPHERALS_NUM }> = Channel::new();

        let sessions =
            embassy_futures::join::join_array(core::array::from_fn::<_, { crate::SPLIT_PERIPHERALS_NUM }, _>(|i| {
                run_peripheral_session(i, &conn_channels[i], &ended, &stack, self.peripheral_matrices[i])
            }));
        join3(
            run_ble_keyboard(
                &stack,
                &self.device_config,
                &self.config,
                self.host_power_config,
                #[cfg(feature = "host")]
                self.host_service,
            ),
            sessions,
            scan_and_connect_peripherals(&stack, &conn_channels, &ended),
        )
        .await;
        unreachable!("BleTransport sub-tasks must run forever")
    }
}

/// Owns the GATT server and the profile manager, and advertises→connects→
/// serves forever, joined with the stack runner and the sleep manager.
async fn run_ble_keyboard<
    #[cfg(feature = "host")] 'r,
    C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>,
>(
    stack: &Stack<'_, C, DefaultPacketPool>,
    device_config: &DeviceConfig<'static>,
    config: &BleBatteryConfig,
    host_power_config: Option<BleHostPowerConfig>,
    #[cfg(feature = "host")] host_service: Option<&'r crate::host::HostService<'r>>,
) -> ! {
    let product_name = device_config.product_name;

    info!("Starting advertising and GATT service");
    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: product_name,
        appearance: &appearance::human_interface_device::KEYBOARD,
    }))
    .unwrap();

    server
        .set(
            &server.device_config_service.pnp_id,
            &PnPID {
                vid_source: VidSource::UsbIF,
                vendor_id: device_config.vid,
                product_id: device_config.pid,
                product_version: 0x0001,
            },
        )
        .unwrap();
    server
        .set(
            &server.device_config_service.serial_number,
            &device_config.serial_number,
        )
        .unwrap();
    server
        .set(
            &server.device_config_service.manufacturer_name,
            &heapless::String::try_from(device_config.manufacturer).expect("manufacturer is too long for BLE"),
        )
        .unwrap();
    let server = &server;

    if crate::ble::passkey::passkey_entry_enabled() {
        stack.set_io_capabilities(IoCapabilities::KeyboardOnly);
    }
    let mut profile_manager: ProfileManager<_, _, BOND_SLOTS> = ProfileManager::new(stack);
    // Load the bonded devices from storage
    profile_manager.load_bonded_devices().await;
    profile_manager.update_stack_bonds();

    let mut peripheral = stack.peripheral();
    let runner = stack.runner();

    let use_2m_phy = crate::BLE_USE_2M_PHY && !cfg!(feature = "use_1m_phy");

    let connection_loop = async {
        let mut resuming_from_sleep = false;
        let mut session_sequence = 0u32;
        // Deadline at which the current dongle advertising session stops;
        // `None` until the first dongle advertise below sets it.
        #[cfg(feature = "dongle")]
        let mut adv_deadline: Option<Instant> = None;
        loop {
            // Let the split halves connect before the host link competes with
            // them for radio time.
            #[cfg(feature = "split")]
            if let Either::Second(()) = select(
                crate::split::ble::central::wait_for_split_connection_window(),
                profile_manager.update_profile(),
            )
            .await
            {
                continue;
            }

            let profile = crate::state::current_profile();
            let active_bond_info = profile_manager.bond_info(profile);
            let active_peer = active_bond_info.as_ref().map(|info| info.info.identity.addr);
            // The dongle slot advertises directed to the bonded dongle or as a
            // seeking broadcast, and keeps its link at 2M; the normal profiles
            // advertise as a plain HID keyboard under the host power policy.
            #[cfg(feature = "dongle")]
            let dongle_link = crate::state::current_profile() == crate::ble::profile::DONGLE_PROFILE;
            #[cfg(not(feature = "dongle"))]
            let dongle_link = false;
            let session_power_config = host_power_config.filter(|_| !dongle_link);
            let host_link_policy =
                host_link_startup_policy(active_bond_info.is_some(), session_power_config.is_some(), use_2m_phy);

            // During wake advertising, subscribe before opening the radio
            // window so a second input can request another attempt even if
            // the current reconnect window expires.
            let wake_during_advertising = WakeAdvertisingInput::new(resuming_from_sleep);

            let advertising = async {
                // A USB-only board leaves the radio to its split links; profile
                // switches are still served below.
                if !crate::BLE_HOST_ADVERTISING {
                    info!("[adv] host advertising disabled");
                    return core::future::pending().await;
                }
                #[cfg(feature = "dongle")]
                if dongle_link {
                    let adv = match active_peer {
                        Some(addr) => Adv::Directed(addr),
                        None => Adv::DongleSeeking,
                    };
                    // Wait for 10ms to ensure the USB is checked
                    Timer::after_millis(10).await;
                    info!("[adv] dongle advertising");
                    set_ble_state(BleState::Advertising);
                    // Advertise only for the time left in this session, so
                    // rejected reconnects can't push the timeout out indefinitely.
                    let now = Instant::now();
                    let deadline =
                        *adv_deadline.get_or_insert(now + Duration::from_secs(DONGLE_ADVERTISING_TIMEOUT_SECS));
                    let conn = advertise(
                        &mut peripheral,
                        &server.server,
                        adv,
                        deadline.saturating_duration_since(now),
                    )
                    .await?;
                    if let Err(e) = conn.raw().set_bondable(true) {
                        error!("Set bondable error: {:?}", e);
                    }
                    return Ok(conn);
                }
                advertise_host(product_name, &mut peripheral, server, active_peer, resuming_from_sleep).await
            };

            match select(advertising, profile_manager.update_profile()).await {
                Either::First(Ok(conn)) => {
                    // Check the bond info after the connection is just created.
                    if let Some(bond) = profile_manager.bond_info(profile)
                        && !bond.info.identity.match_identity(&conn.raw().peer_identity())
                    {
                        warn!("[ble] connected peer doesn't match the active profile, disconnecting");
                        disconnect_and_wait(&conn).await;
                        continue;
                    }
                    // When connecting to BLE host, check the connected peer is not a dongle.
                    #[cfg(feature = "dongle")]
                    if !dongle_link && profile_manager.is_bonded_dongle(&conn.raw().peer_identity()) {
                        warn!("[ble] the bonded dongle connected on a host BLE profile, disconnecting");
                        disconnect_and_wait(&conn).await;
                        continue;
                    }

                    session_sequence = session_sequence.wrapping_add(1);
                    let session_id = session_sequence;
                    info!(
                        "[BLE_SESSION_V15] id={} phase=start raw_connected={} connections_max={} l2cap_max={}",
                        session_id,
                        conn.raw().is_connected(),
                        CONNECTIONS_MAX,
                        L2CAP_CHANNELS_MAX
                    );
                    // The wake observer is needed only until advertising
                    // succeeds. Drop both of its PubSub subscribers before
                    // entering a connection that can live for hours.
                    wake_during_advertising.connected();

                    // Do NOT emit BleState::Connected here. gatt_events_task emits
                    // Connected when it sees GattConnectionEvent::Encrypted.
                    let connection_was_resume = resuming_from_sleep;
                    match select(
                        serve_keyboard_connection(
                            server,
                            &conn,
                            stack,
                            profile,
                            &profile_manager,
                            config,
                            session_power_config,
                            host_link_policy,
                            dongle_link,
                            #[cfg(feature = "host")]
                            host_service,
                        ),
                        profile_manager.update_profile(),
                    )
                    .await
                    {
                        Either::First(BleKeyboardExit::Disconnected) => {
                            // If wake advertising connected but encryption
                            // never completed, retain Sleeping and retry.
                            resuming_from_sleep =
                                connection_was_resume && crate::state::current_ble_status().state == BleState::Sleeping;
                        }
                        Either::First(BleKeyboardExit::IdleTimeout) => {
                            info!("Host BLE idle timeout, disconnecting until local input");

                            // Subscribe before disconnecting so input during
                            // teardown is retained as the wake event.
                            let wake = InputActivityWaiter::new();
                            let activity_during_transition = take_host_power_input() == Some(false);

                            if conn.raw().is_connected() {
                                disconnect_and_wait(&conn).await;
                            }

                            if activity_during_transition {
                                report_activity();
                                resuming_from_sleep = true;
                            } else {
                                match select(wake.wait(), profile_manager.update_profile()).await {
                                    Either::First(()) => {
                                        report_activity();
                                        resuming_from_sleep = true;
                                    }
                                    Either::Second(()) => {
                                        report_activity();
                                        resuming_from_sleep = false;
                                    }
                                }
                            }
                        }
                        Either::First(BleKeyboardExit::HidWriteStalled) => {
                            error!("BLE HID output stalled, disconnecting for a fail-closed reconnect");

                            // Abandon every report from the unhealthy
                            // session and make the first report after the
                            // reconnect an all-up keyboard state. Sleeping
                            // keeps any new local input queued while the
                            // bonded host reconnects.
                            report_activity();
                            prepare_hid_write_recovery();

                            if conn.raw().is_connected() {
                                disconnect_and_wait(&conn).await;
                            }
                            resuming_from_sleep = true;
                        }
                        #[cfg(feature = "host_first_split_wake")]
                        Either::First(BleKeyboardExit::ConnectionParamsStalled) => {
                            error!(
                                "BLE active connection parameters were not restored, disconnecting for a fail-closed reconnect"
                            );

                            // Reports produced while the host remained in
                            // low-duty mode are stale; reconnect from a
                            // clean all-up HID state.
                            report_activity();
                            prepare_hid_write_recovery();

                            if conn.raw().is_connected() {
                                disconnect_and_wait(&conn).await;
                            }
                            resuming_from_sleep = true;
                        }
                        Either::Second(()) => {
                            resuming_from_sleep = false;
                            report_activity();

                            // When the profile changes, manually disconnect
                            // from the current host.
                            if conn.raw().is_connected() {
                                disconnect_and_wait(&conn).await;
                            }
                        }
                    }

                    // A logical GATT exit can precede nrf-sdc's final
                    // DisconnectionComplete processing by several
                    // milliseconds. Starting advertising in that gap leaks
                    // the old ACL/GATT resources and eventually produces a
                    // permanent OutOfMemory reconnect loop. Do not leave
                    // this scope until the physical link is down.
                    ensure_host_session_released(&conn, session_id).await;
                    drop(conn);
                    Timer::after_millis(HOST_SESSION_RELEASE_SETTLE_MS).await;
                    info!("[BLE_SESSION_V15] id={} phase=released", session_id);
                }
                Either::First(Err(BleHostError::BleHost(Error::Timeout))) => {
                    // A failed BLE host window must not put the whole
                    // keyboard to sleep while another host transport is
                    // still available. This is especially important for a
                    // USB Qube: its BLE stack is also needed for split
                    // links, but the Qube itself is already connected to
                    // the PC over USB. It is not user activity either: a
                    // USB Qube hits this timeout every pairing window, and
                    // reporting it kept resetting the split idle timeout
                    // so the halves never entered their low-power cadence.
                    if crate::state::active_transport().is_some() {
                        warn!("Advertising timeout while another transport is active, staying awake");
                        resuming_from_sleep = false;
                        set_ble_state(BleState::Inactive);
                        #[cfg(feature = "dongle")]
                        {
                            adv_deadline = None;
                        }
                        continue;
                    }

                    set_ble_state(BleState::Sleeping);
                    request_sleep();

                    let wake = wake_during_advertising.into_waiter();
                    warn!("Advertising timeout, sleeping until local input");

                    match select(wake.wait(), profile_manager.update_profile()).await {
                        Either::First(()) => {
                            report_activity();
                            resuming_from_sleep = true;
                        }
                        Either::Second(()) => {
                            report_activity();
                            resuming_from_sleep = false;
                        }
                    }
                }
                Either::First(Err(e)) => {
                    #[cfg(feature = "defmt")]
                    let e = defmt::Debug2Format(&e);
                    error!("Advertise error: {:?}", e);
                    // This also rate-limits a controller that is still
                    // completing the preceding disconnection.
                    Timer::after_millis(250).await;
                }
                Either::Second(()) => {
                    report_activity();
                    resuming_from_sleep = false;
                }
            };

            // Starts a fresh advertising session.
            #[cfg(feature = "dongle")]
            {
                adv_deadline = None;
            }

            // Sleeping remains set while wake advertising is in progress so
            // HID reports stay in the BLE queue for the reconnecting host.
            if !matches!(
                crate::state::current_ble_status().state,
                BleState::Advertising | BleState::Sleeping
            ) {
                set_ble_state(BleState::Inactive);
            }
        }
    };

    #[cfg(feature = "split")]
    let event_handler = crate::split::ble::central::ScanHandler;
    #[cfg(not(feature = "split"))]
    let event_handler = NoopHandler;

    // The sleep manager lives here because this is the single always-present
    // BLE task: split or not, connected or not, it keeps running, so the
    // sleep state can never get stuck.
    join4(
        ble_task(runner, &event_handler),
        connection_loop,
        sleep::run_sleep_manager(),
        profile_manager.run(),
    )
    .await;
    unreachable!("BleTransport sub-tasks must run forever")
}

/// NoopHandler is used on the device which never scans,
/// such as a split peripheral or a normal keyboard.
pub(crate) struct NoopHandler;

impl EventHandler for NoopHandler {}

/// Latched by [`ble_task`] on its first poll, so the roles that drive the same
/// stack know the runner is about to pump it.
static STACK_STARTED: Signal<crate::RawMutex, ()> = Signal::new();

/// Wait until [`ble_task`] is up, plus a grace period. Polled because the
/// one-shot latch has multiple waiters.
pub(crate) async fn wait_for_stack_started() {
    while !STACK_STARTED.signaled() {
        Timer::after_millis(500).await;
    }
    Timer::after_millis(500).await;
}

/// This is a background task that is required to run forever alongside any other BLE tasks.
pub(crate) async fn ble_task<C: Controller, P: PacketPool, E: EventHandler>(mut runner: Runner<'_, C, P>, handler: &E) {
    STACK_STARTED.signal(());
    if let Err(e) = runner.run_with_handler(handler).await {
        error!("[ble_task] runner stopped, rebooting: {:?}", e);
        Timer::after_millis(100).await;
        crate::boot::reboot_keyboard();
    }
}

/// Stream Events until the connection closes.
///
/// This function will handle the GATT events and process them.
/// This is how we interact with read and write requests.
async fn gatt_events_task<C: Controller + ControllerCmdAsync<LeSetPhy>>(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_, DefaultPacketPool>,
    stack: &Stack<'_, C, DefaultPacketPool>,
    profile: u8,
    profiles: &ProfileManager<'_, '_, C, DefaultPacketPool, BOND_SLOTS>,
    session_ready: &Signal<crate::RawMutex, ()>,
    local_hid_suspend: bool,
) -> Result<(), Error> {
    let level = server.battery_service.level;
    #[cfg(feature = "split")]
    let peripheral_levels = server.peripheral_battery_services.levels;
    let output_keyboard = server.hid_service.output_keyboard;
    let hid_control_point = server.hid_service.hid_control_point;
    let input_keyboard = server.hid_service.input_keyboard;
    let mouse = server.hid_service.mouse_report;
    let media = server.hid_service.media_report;
    let system_control = server.hid_service.system_report;

    #[cfg(feature = "passkey_entry")]
    let mut passkey_state = PasskeyInputState::new();

    #[cfg(feature = "host")]
    let mut host_gatt_handler = HostGattHandler::new(server);

    loop {
        #[cfg(feature = "passkey_entry")]
        let Some(event) = next_gatt_event(conn, &mut passkey_state).await else {
            continue;
        };
        #[cfg(not(feature = "passkey_entry"))]
        let event = conn.next().await;

        match event {
            GattConnectionEvent::Disconnected { reason } => {
                #[cfg(feature = "passkey_entry")]
                passkey_state.clear();
                info!("[gatt] disconnected: {:?}", reason);
                break;
            }
            GattConnectionEvent::PairingComplete { security_level, bond } => {
                #[cfg(feature = "passkey_entry")]
                passkey_state.clear();
                info!("[gatt] pairing complete: {:?}", security_level);
                if let Some(bond_info) = bond {
                    let cccd_table = server
                        .get_client_att_table(conn.raw())
                        .and_then(|t| heapless::Vec::from_slice(t.raw()).ok())
                        .unwrap_or_default();
                    profiles.set_bond(ProfileInfo {
                        slot_num: profile,
                        info: bond_info,
                        removed: false,
                        cccd_table,
                    });
                }
            }
            GattConnectionEvent::PairingFailed(err) => {
                #[cfg(feature = "passkey_entry")]
                passkey_state.clear();
                error!("[gatt] pairing error: {:?}", err);
            }
            GattConnectionEvent::Encrypted { security_level, .. } => {
                info!("[gatt] encrypted: {:?}", security_level);
                mark_ble_session_ready(session_ready);
            }
            GattConnectionEvent::Gatt { event: gatt_event } => {
                let mut cccd_updated = false;
                let result = match &gatt_event {
                    GattEvent::Read(event) => {
                        debug!("Read GATT Event: {:?}", event.handle());
                        if conn.raw().security_level()?.encrypted() {
                            server.refresh_battery_level(event.handle());
                            None
                        } else {
                            Some(AttErrorCode::INSUFFICIENT_ENCRYPTION)
                        }
                    }
                    GattEvent::Write(event) => {
                        #[cfg(all(feature = "dongle", feature = "custom_message"))]
                        let is_custom_message = event.handle() == server.dongle_event_service.custom_to_keyboard.handle;
                        #[cfg(not(all(feature = "dongle", feature = "custom_message")))]
                        let is_custom_message = false;
                        let encrypted = conn.raw().security_level()?.encrypted();

                        // trouble-host 0.7 exposes written bytes via a closure; copy them out
                        // once so the dispatch below (which awaits) can use them freely.
                        let mut data_buf = [0u8; GATT_WRITE_BUFFER_SIZE];
                        let data_len = event.with_data(|_, data| {
                            let n = data.len().min(data_buf.len());
                            data_buf[..n].copy_from_slice(&data[..n]);
                            data.len()
                        });
                        let data = &data_buf[..data_len.min(data_buf.len())];
                        let mut control_point_write = false;

                        if event.handle() == output_keyboard.handle {
                            if data_len == 1 {
                                let led_indicator = LedIndicator::from_bits(data[0]);
                                debug!("Got keyboard state: {:?}", led_indicator);
                                LED_SIGNAL.signal(led_indicator);
                            } else {
                                warn!("Wrong keyboard state data: {:?}", data);
                            }
                        } else if event.handle() == input_keyboard.cccd_handle.expect("No CCCD for input keyboard")
                            || event.handle() == mouse.cccd_handle.expect("No CCCD for mouse report")
                            || event.handle() == media.cccd_handle.expect("No CCCD for media report")
                            || event.handle() == system_control.cccd_handle.expect("No CCCD for system report")
                            || event.handle() == level.cccd_handle.expect("No CCCD for battery level")
                            || {
                                #[cfg(feature = "split")]
                                {
                                    peripheral_levels.iter().any(|level| {
                                        event.handle()
                                            == level.cccd_handle.expect("No CCCD for peripheral battery level")
                                    })
                                }
                                #[cfg(not(feature = "split"))]
                                {
                                    false
                                }
                            }
                        {
                            cccd_updated = true;
                        } else if event.handle() == hid_control_point.handle {
                            control_point_write = true;
                        } else if is_custom_message {
                            #[cfg(all(feature = "dongle", feature = "custom_message"))]
                            match postcard::from_bytes::<crate::custom_message::CustomMessage>(data) {
                                Ok(message) => match message.target {
                                    #[cfg(feature = "split")]
                                    crate::custom_message::CustomMessageTarget::Peripherals => {
                                        crate::custom_message::send(message)
                                    }
                                    crate::custom_message::CustomMessageTarget::Central => {
                                        crate::event::publish_event(message)
                                    }
                                    _ => (),
                                },
                                Err(_) => warn!("[ble] undecodable custom message dropped"),
                            }
                        } else {
                            #[cfg(feature = "host")]
                            match host_gatt_handler.handle_write(event.handle(), data, encrypted).await {
                                HostWriteOutcome::Handled => {
                                    // A configurator session keeps the keyboard
                                    // awake and the link interactive.
                                    HOST_BLE_ACTIVITY.signal(());
                                    report_activity();
                                }
                                HostWriteOutcome::CccdUpdated => cccd_updated = true,
                                HostWriteOutcome::ControlPoint => control_point_write = true,
                                HostWriteOutcome::Unhandled => {
                                    debug!("Write GATT Event to Unknown: {:?}", event.handle())
                                }
                            }
                            #[cfg(not(feature = "host"))]
                            debug!("Write GATT Event to Unknown: {:?}", event.handle());
                        }

                        if control_point_write {
                            info!("Write GATT Event to Control Point: {:?}", event.handle());
                            // Forward HID suspend/resume to the persistent sleep manager.
                            // HID Class spec opcodes for the HID Control Point characteristic:
                            //   - 0: HID_CTRL_SUSPEND
                            //   - 1: HID_CTRL_EXIT_SUSPEND
                            if data_len == 1 {
                                match hid_control_point_action(data[0], local_hid_suspend) {
                                    HidControlPointAction::Disconnect => request_sleep(),
                                    HidControlPointAction::LocalSleep => request_local_sleep(),
                                    HidControlPointAction::Activity => report_activity(),
                                    HidControlPointAction::Ignore => {}
                                }
                            }
                        }

                        if encrypted {
                            None
                        } else {
                            Some(AttErrorCode::INSUFFICIENT_ENCRYPTION)
                        }
                    }
                    GattEvent::Other(_) => None,
                    GattEvent::NotAllowed(_) => None,
                };

                // This step is also performed at drop(), but writing it explicitly is necessary
                // in order to ensure reply is sent.
                let result = if let Some(code) = result {
                    gatt_event.reject(code)
                } else {
                    gatt_event.accept()
                };
                // Update CCCD table after processing the event
                if cccd_updated {
                    // When macOS wakes up from sleep mode, it won't send EXIT SUSPEND command
                    // So we need to monitor the sleep state by using CCCD write event
                    report_activity();

                    if let Some(table) = server.get_client_att_table(conn.raw())
                        && let Ok(bytes) = heapless::Vec::from_slice(table.raw())
                    {
                        profiles.update_profile_cccd_table(profile, bytes);
                    }
                }
                match result {
                    Ok(reply) => reply.send().await,
                    Err(e) => warn!("[gatt] error sending response: {:?}", e),
                }
            }
            GattConnectionEvent::PhyUpdated { tx_phy, rx_phy } => {
                info!("[gatt] PhyUpdated: {:?}, {:?}", tx_phy, rx_phy)
            }
            GattConnectionEvent::ConnectionParamsUpdated {
                conn_interval,
                peripheral_latency,
                supervision_timeout,
            } => {
                info!(
                    "[gatt] ConnectionParamsUpdated: {:?}ms, {:?}, {:?}ms",
                    conn_interval.as_millis(),
                    peripheral_latency,
                    supervision_timeout.as_millis()
                );
                HOST_CONN_PARAMS_UPDATED.signal(HostConnParamsSnapshot {
                    interval: conn_interval,
                    latency: peripheral_latency,
                });
            }
            GattConnectionEvent::RequestConnectionParams(req) => {
                info!(
                    "[gatt] RequestConnectionParams: interval: ({:?}, {:?})ms, {:?}, {:?}ms",
                    req.params().min_connection_interval.as_millis(),
                    req.params().max_connection_interval.as_millis(),
                    req.params().max_latency,
                    req.params().supervision_timeout.as_millis(),
                );

                // The host connection policy is owned locally; reject peer
                // updates so an unsolicited request cannot replace it.
                if let Err(e) = req.reject(stack).await {
                    #[cfg(feature = "defmt")]
                    let e = defmt::Debug2Format(&e);
                    warn!("[gatt] failed to respond to connection parameters: {:?}", e);
                }
            }
            GattConnectionEvent::DataLengthUpdated {
                max_tx_octets,
                max_tx_time,
                max_rx_octets,
                max_rx_time,
            } => {
                info!(
                    "[gatt] DataLengthUpdated: tx/rx octets: ({:?}, {:?}), tx/rx time: ({:?}, {:?})",
                    max_tx_octets, max_rx_octets, max_tx_time, max_rx_time
                );
            }
            GattConnectionEvent::FrameSpaceUpdated {
                frame_space,
                initiator,
                phys,
                spacing_types,
            } => {
                info!(
                    "[gatt] FrameSpaceUpdated: {:?}, {:?}, {:?}, {:?}",
                    frame_space, initiator, phys, spacing_types
                );
            }
            GattConnectionEvent::ConnectionRateChanged {
                conn_interval,
                subrate_factor,
                peripheral_latency,
                continuation_number,
                supervision_timeout,
            } => {
                info!(
                    "[gatt] ConnectionRateChanged: {:?}ms, {:?}, {:?}, {:?}, {:?}ms",
                    conn_interval.as_millis(),
                    subrate_factor,
                    peripheral_latency,
                    continuation_number,
                    supervision_timeout.as_millis()
                );
            }
            GattConnectionEvent::SubratingParamsUpdated {
                subrate_factor,
                peripheral_latency,
                continuation_number,
                supervision_timeout,
            } => {
                info!(
                    "[gatt] SubratingParamsUpdated: {:?}, {:?}, {:?}, {:?}ms",
                    subrate_factor,
                    peripheral_latency,
                    continuation_number,
                    supervision_timeout.as_millis()
                );
            }
            GattConnectionEvent::PassKeyDisplay(pass_key) => info!("[gatt] PassKeyDisplay: {:?}", pass_key),
            GattConnectionEvent::PassKeyConfirm(pass_key) => info!("[gatt] PassKeyConfirm: {:?}", pass_key),
            GattConnectionEvent::PassKeyInput => {
                #[cfg(feature = "passkey_entry")]
                if crate::PASSKEY_ENTRY_ENABLED {
                    info!("[gatt] PassKeyInput: entering passkey entry mode");
                    passkey_state.begin();
                } else {
                    warn!("[gatt] PassKeyInput: disabled in config, cancelling pairing, this shouldn't happen");
                    if let Err(e) = conn.raw().pass_key_cancel() {
                        error!("[gatt] pass_key_cancel error: {:?}", e);
                    }
                }
                #[cfg(not(feature = "passkey_entry"))]
                warn!("[gatt] PassKeyInput event, should not happen")
            }
            GattConnectionEvent::BondLost => warn!("[gatt] BondLost"),
            GattConnectionEvent::OobRequest => warn!("[gatt] OobRequest"),
        }
    }
    info!("[gatt] task finished");
    Ok(())
}

/// Advertise as a HID keyboard to a BLE host and wait for it to connect.
///
/// A bonded profile only reconnects its bonded host, through a connection
/// filtered undirected advertisement, and never opens a pairing window on its
/// own. An unbonded profile opens the configured pairing window.
async fn advertise_host<'a, 'b, C: Controller>(
    name: &'a str,
    peripheral: &mut Peripheral<'a, C, DefaultPacketPool>,
    server: &'b Server<'_>,
    active_peer: Option<Address>,
    resuming_from_sleep: bool,
) -> Result<GattConnection<'a, 'b, DefaultPacketPool>, BleHostError<C::Error>> {
    // Wait for 10ms to ensure the USB is checked
    embassy_time::Timer::after_millis(10).await;
    let mut advertiser_data = [0; 31];
    AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteServiceUuids16(&[BATTERY.to_le_bytes(), HUMAN_INTERFACE_DEVICE.to_le_bytes()]),
            AdStructure::CompleteLocalName(name.as_bytes()),
            AdStructure::Unknown {
                ty: 0x19, // Appearance
                data: &KEYBOARD.to_le_bytes(),
            },
        ],
        &mut advertiser_data[..],
    )?;

    let fast_advertise_config = AdvertisementParameters {
        // Keep discovery compatible with hosts that scan advertising on LE 1M.
        // The established connection is still upgraded to LE 2M afterwards.
        primary_phy: PhyKind::Le1M,
        secondary_phy: PhyKind::Le1M,
        tx_power: TxPower::Plus8dBm,
        interval_min: Duration::from_millis(30),
        interval_max: Duration::from_millis(30),
        ..Default::default()
    };
    let slow_advertise_config = AdvertisementParameters {
        interval_min: Duration::from_millis(200),
        interval_max: Duration::from_millis(200),
        ..fast_advertise_config
    };

    let reconnect_timeout_secs = u64::from(crate::BLE_RECONNECT_TIMEOUT_SECONDS);
    let reconnect_timeout_ms = reconnect_timeout_secs.saturating_mul(1_000);
    let bonded_windows = bonded_reconnect_windows(reconnect_timeout_ms);
    let configured_pairing_timeout = u64::from(crate::BLE_PAIRING_TIMEOUT_SECONDS);
    let has_active_peer = active_peer.is_some();
    let pairing_window_secs =
        pairing_window_timeout_secs(has_active_peer, configured_pairing_timeout, reconnect_timeout_secs);

    crate::state::set_ble_advertising_mode(advertising_mode(has_active_peer));
    if !resuming_from_sleep {
        info!("[adv] advertising");
        set_ble_state(BleState::Advertising);
    }

    if let Some(peer) = active_peer {
        info!("[ADV_GUARD_V13] directed_high_duty=off strategy=filtered_undirected");
        let high_duty_window_ms = bonded_windows.directed_high_duty_ms;
        if high_duty_window_ms > 0 {
            info!("[adv] directed high duty reconnect");
            let advertiser = peripheral
                .advertise(
                    &fast_advertise_config,
                    Advertisement::ConnectableNonscannableDirectedHighDuty { peer },
                )
                .await?;
            match with_timeout(Duration::from_millis(high_duty_window_ms), advertiser.accept()).await {
                Ok(Ok(conn)) => {
                    let conn = conn.with_attribute_server(server)?;
                    info!("[adv] directed connection established");
                    if let Err(e) = conn.raw().set_bondable(true) {
                        error!("Set bondable error: {:?}", e);
                    }
                    return Ok(conn);
                }
                Ok(Err(error)) if directed_reconnect_should_continue(&error) => {
                    info!("[adv] directed reconnect timed out");
                }
                Err(_) => {
                    info!("[adv] directed reconnect window elapsed");
                }
                Ok(Err(error)) => return Err(BleHostError::BleHost(error)),
            }
        }

        if bonded_windows.fast_undirected_ms > 0 || bonded_windows.slow_undirected_ms > 0 {
            // Directed advertising is not rediscovered reliably by every host
            // after a peripheral-initiated idle disconnect. Fall back to an
            // undirected advertisement restricted to the bonded peer: the OS
            // can scan and reconnect normally, while a new host still cannot
            // connect or start pairing.
            peripheral.set_filter_accept_list(&[peer]).await?;
        }

        for (window_ms, params, window) in [
            (bonded_windows.fast_undirected_ms, fast_advertise_config, "fast"),
            (bonded_windows.slow_undirected_ms, slow_advertise_config, "slow"),
        ] {
            if window_ms == 0 {
                continue;
            }
            let bonded_reconnect_config = AdvertisementParameters {
                filter_policy: bonded_reconnect_filter_policy(),
                ..params
            };
            info!("[adv] {} filtered bonded-host reconnect", window);
            let advertiser = peripheral
                .advertise(
                    &bonded_reconnect_config,
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &advertiser_data[..],
                        scan_data: &[],
                    },
                )
                .await?;
            match with_timeout(Duration::from_millis(window_ms), advertiser.accept()).await {
                Ok(conn_res) => {
                    let conn = conn_res?.with_attribute_server(server)?;
                    info!("[adv] bonded host connection established");
                    if let Err(e) = conn.raw().set_bondable(false) {
                        error!("Set bondable error: {:?}", e);
                    }
                    return Ok(conn);
                }
                Err(_) => info!("[adv] {} bonded-host reconnect window elapsed", window),
            }
        }

        // A bonded profile must never become discoverable for a new host
        // automatically. Opening a pairing window requires an explicit bond
        // clear or switching to an unbonded profile.
        return Err(BleHostError::BleHost(Error::Timeout));
    }

    let Some(undirected_timeout_secs) = pairing_window_secs.filter(|secs| *secs > 0) else {
        return Err(BleHostError::BleHost(Error::Timeout));
    };

    let fast_timeout_secs = undirected_timeout_secs.min(FAST_ADVERTISING_TIMEOUT_SECS);
    let slow_timeout_secs = undirected_timeout_secs.saturating_sub(fast_timeout_secs);
    for (timeout_secs, params, window) in [
        (fast_timeout_secs, fast_advertise_config, "fast"),
        (slow_timeout_secs, slow_advertise_config, "slow"),
    ] {
        if timeout_secs == 0 {
            continue;
        }
        info!("[adv] {} undirected advertising", window);
        let advertiser = peripheral
            .advertise(
                &params,
                Advertisement::ConnectableScannableUndirected {
                    adv_data: &advertiser_data[..],
                    scan_data: &[],
                },
            )
            .await?;
        if let Ok(conn_res) = with_timeout(Duration::from_secs(timeout_secs), advertiser.accept()).await {
            let conn = conn_res?.with_attribute_server(server)?;
            info!("[adv] connection established");
            if let Err(e) = conn.raw().set_bondable(true) {
                error!("Set bondable error: {:?}", e);
            }
            return Ok(conn);
        }
    }
    Err(BleHostError::BleHost(Error::Timeout))
}

fn advertising_mode(has_active_bond: bool) -> BleAdvertisingMode {
    if has_active_bond {
        BleAdvertisingMode::Reconnecting
    } else {
        BleAdvertisingMode::Pairing
    }
}

fn bonded_reconnect_filter_policy() -> AdvFilterPolicy {
    AdvFilterPolicy::FilterConn
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BondedReconnectWindows {
    directed_high_duty_ms: u64,
    fast_undirected_ms: u64,
    slow_undirected_ms: u64,
}

fn bonded_reconnect_windows(reconnect_timeout_ms: u64) -> BondedReconnectWindows {
    // Directed high-duty reconnect is disabled for this profile, so the
    // reserved window is always zero.
    let directed_high_duty_ms = DIRECTED_RECONNECT_WINDOW_MS;
    let fast_undirected_ms = reconnect_timeout_ms
        .min(FAST_BONDED_RECONNECT_TOTAL_MS)
        .saturating_sub(directed_high_duty_ms);
    let slow_undirected_ms = reconnect_timeout_ms
        .saturating_sub(directed_high_duty_ms)
        .saturating_sub(fast_undirected_ms);

    BondedReconnectWindows {
        directed_high_duty_ms,
        fast_undirected_ms,
        slow_undirected_ms,
    }
}

fn pairing_window_timeout_secs(
    has_active_bond: bool,
    configured_pairing_timeout_secs: u64,
    reconnect_timeout_secs: u64,
) -> Option<u64> {
    if has_active_bond {
        None
    } else if configured_pairing_timeout_secs == 0 {
        Some(reconnect_timeout_secs)
    } else {
        Some(configured_pairing_timeout_secs)
    }
}

fn directed_reconnect_should_continue(error: &Error) -> bool {
    matches!(error, Error::Timeout)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BleKeyboardExit {
    Disconnected,
    IdleTimeout,
    HidWriteStalled,
    #[cfg(feature = "host_first_split_wake")]
    ConnectionParamsStalled,
}

/// Owns the temporary input subscriptions used only while a sleeping host is
/// being advertised to.
///
/// A successful connection must consume this guard before the long-lived BLE
/// session starts. Otherwise its unread PubSub subscribers retain every key
/// and pointing event until the bounded event queues stop the input producers.
struct WakeAdvertisingInput {
    waiter: Option<InputActivityWaiter>,
}

impl WakeAdvertisingInput {
    fn new(resuming_from_sleep: bool) -> Self {
        Self {
            waiter: resuming_from_sleep.then(InputActivityWaiter::new),
        }
    }

    fn connected(self) {
        drop(self.waiter);
    }

    fn into_waiter(self) -> InputActivityWaiter {
        self.waiter.unwrap_or_else(InputActivityWaiter::new)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HidControlPointAction {
    Disconnect,
    LocalSleep,
    Activity,
    Ignore,
}

fn hid_control_point_action(opcode: u8, local_suspend: bool) -> HidControlPointAction {
    match opcode {
        0 if local_suspend => HidControlPointAction::LocalSleep,
        0 => HidControlPointAction::Disconnect,
        1 => HidControlPointAction::Activity,
        _ => HidControlPointAction::Ignore,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostConnParamBootstrap {
    Legacy,
    BondedRefresh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HostLinkStartupPolicy {
    update_phy: bool,
    conn_params: HostConnParamBootstrap,
}

fn host_link_startup_policy(
    has_active_bond: bool,
    preserve_bonded_link: bool,
    use_2m_phy: bool,
) -> HostLinkStartupPolicy {
    if has_active_bond && preserve_bonded_link {
        HostLinkStartupPolicy {
            update_phy: false,
            conn_params: HostConnParamBootstrap::BondedRefresh,
        }
    } else {
        HostLinkStartupPolicy {
            update_phy: use_2m_phy,
            conn_params: HostConnParamBootstrap::Legacy,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostPowerTransition {
    EnterIdle,
    Disconnect,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostPowerTimer {
    Power(HostPowerTransition),
    VialIdle,
}

fn next_host_power_transition(config: BleHostPowerConfig, idle_connection: bool) -> (Duration, HostPowerTransition) {
    let disconnect_timeout = config.disconnect_timeout();
    if !idle_connection && config.idle_timeout < disconnect_timeout {
        (config.idle_timeout, HostPowerTransition::EnterIdle)
    } else {
        (disconnect_timeout, HostPowerTransition::Disconnect)
    }
}

fn next_host_power_timer(
    config: BleHostPowerConfig,
    idle_connection: bool,
    last_activity: Instant,
    vial_active: bool,
    last_vial_activity: Instant,
) -> (Instant, HostPowerTimer) {
    let (power_after, power_transition) = next_host_power_transition(config, idle_connection);
    let power_deadline = last_activity + power_after;
    let vial_deadline = last_vial_activity + Duration::from_secs(VIAL_LINK_IDLE_TIMEOUT_SECS);

    if vial_active && vial_deadline < power_deadline {
        (vial_deadline, HostPowerTimer::VialIdle)
    } else {
        (power_deadline, HostPowerTimer::Power(power_transition))
    }
}

async fn wait_for_vial_activity() {
    #[cfg(feature = "host")]
    HOST_BLE_ACTIVITY.wait().await;

    #[cfg(not(feature = "host"))]
    core::future::pending::<()>().await;
}

#[cfg(feature = "host_first_split_wake")]
const fn host_input_requires_active_confirmation(idle_connection: bool, _vial_active: bool) -> bool {
    idle_connection
}

fn host_power_transition_allowed(active_transport: Option<ConnectionType>) -> bool {
    active_transport != Some(ConnectionType::Usb)
}

#[cfg(feature = "host_first_split_wake")]
async fn set_conn_params<'a, 'b, C: Controller + ControllerCmdSync<LeReadLocalSupportedFeatures>, P: PacketPool>(
    stack: &Stack<'_, C, P>,
    conn: &GattConnection<'a, 'b, P>,
    host_power_config: Option<BleHostPowerConfig>,
    bootstrap: HostConnParamBootstrap,
) -> BleKeyboardExit {
    // The guard also clears any stale pending value before bootstrap. Once the
    // task enters host-power idle it owns the gate until confirmation or Drop.
    let mut wake_order_session = HostWakeOrderSession::new();

    if host_power_config.is_some() {
        reset_host_power_input();
        HOST_POWER_CONFIG_CHANGED.reset();
    }

    match bootstrap {
        HostConnParamBootstrap::Legacy => info!("Fresh BLE session, applying current host connection parameters"),
        HostConnParamBootstrap::BondedRefresh => {
            info!("Bonded BLE session, refreshing host connection parameters")
        }
    }

    #[cfg(feature = "host_fixed_15ms")]
    let mut active_params = {
        info!("[HOST_DIAG_V9] mode=fixed15 requested_interval_ms=15 requested_latency=0");
        Timer::after_secs(5).await;
        let target = HostConnParamsSnapshot {
            interval: Duration::from_millis(15),
            latency: HOST_INTERACTIVE_MAX_LATENCY,
        };
        match request_confirmed_active_params(
            stack,
            conn.raw(),
            target,
            0,
            HOST_FIXED_CONN_PARAM_ATTEMPTS,
            HOST_FIXED_CONN_PARAM_RETRY_MS,
        )
        .await
        {
            Some(applied) => applied,
            None if host_power_config.is_some() => return BleKeyboardExit::ConnectionParamsStalled,
            None => target,
        }
    };

    #[cfg(not(feature = "host_fixed_15ms"))]
    let mut active_params = {
        // Ported narrowly from upstream RMK #1088. Apple hosts accept the first
        // 15 ms request; other hosts can accept the later 7.5 ms request. Run the
        // sequence for bonded sessions too so an old 15 ms bond can be upgraded
        // without deleting the profile. The delay keeps link-control procedures
        // away from pairing/encryption and mirrors the upstream timing.
        let requests = host_bootstrap_connection_requests();
        let mut fast_applied = None;
        for (request_index, (interval, max_latency, supervision_timeout)) in requests.into_iter().enumerate() {
            Timer::after_secs(5).await;
            let mut params = host_connection_params(interval, max_latency);
            params.supervision_timeout = supervision_timeout;
            HOST_CONN_PARAMS_UPDATED.reset();
            update_conn_params(stack, conn.raw(), &params).await;

            if request_index == 1 {
                fast_applied = with_timeout(
                    Duration::from_secs(HOST_CONN_PARAM_UPDATE_TIMEOUT_SECS),
                    HOST_CONN_PARAMS_UPDATED.wait(),
                )
                .await
                .ok();
            }
        }

        let target = host_interactive_target(fast_applied);
        if host_requires_apple_safe_fallback(fast_applied) {
            match fast_applied {
                Some(snapshot) => info!(
                    "Host retained {:?}ms latency {}; restoring 15ms latency 0",
                    snapshot.interval.as_millis(),
                    snapshot.latency
                ),
                None => info!("No 7.5ms parameter update observed; restoring 15ms latency 0"),
            }
        } else {
            info!("Host accepted 7.5ms; removing bootstrap slave latency");
        }

        match request_confirmed_active_params(
            stack,
            conn.raw(),
            target,
            0,
            HOST_ACTIVE_CONN_PARAM_ATTEMPTS,
            HOST_ACTIVE_CONN_PARAM_RETRY_MS,
        )
        .await
        {
            Some(applied) => applied,
            None => {
                // Bootstrap is a preference negotiation, not evidence of a
                // stalled HID link. The host owns the accepted interval and
                // may reject our faster request or omit a no-op update event.
                // Keep that anchor for idle/wake, but still request latency 0
                // on wake; never treat retained slave latency as confirmed.
                let current = conn.raw().params();
                warn!(
                    "[HOST_ACTIVE_V25] phase=0 state=host_selected interval_us={} latency={}",
                    current.conn_interval.as_micros(),
                    current.peripheral_latency
                );
                HostConnParamsSnapshot {
                    interval: current.conn_interval,
                    latency: HOST_INTERACTIVE_MAX_LATENCY,
                }
            }
        }
    };

    if let Some(config) = host_power_config {
        let mut last_activity = Instant::now();
        let mut last_vial_activity = last_activity;
        let mut idle_connection = false;
        let mut vial_active = false;

        loop {
            let (deadline, timer_action) =
                next_host_power_timer(config, idle_connection, last_activity, vial_active, last_vial_activity);
            let timer = async move {
                Timer::at(deadline).await;
                timer_action
            };

            match select4(
                wait_for_host_power_input(),
                HOST_POWER_CONFIG_CHANGED.wait(),
                wait_for_vial_activity(),
                timer,
            )
            .await
            {
                Either4::First(immediate_suspend) => {
                    if immediate_suspend {
                        set_ble_state(BleState::Sleeping);
                        // The producer already signalled the persistent sleep
                        // manager. Re-signalling here can overwrite a key's
                        // concurrent activity notification in HOST_POWER_INPUT.
                        return BleKeyboardExit::IdleTimeout;
                    }

                    last_activity = Instant::now();
                    if host_input_requires_active_confirmation(idle_connection, vial_active) {
                        info!("Host BLE activity, restoring active connection parameters");
                        match request_confirmed_active_params(
                            stack,
                            conn.raw(),
                            active_params,
                            1,
                            HOST_ACTIVE_CONN_PARAM_ATTEMPTS,
                            HOST_ACTIVE_CONN_PARAM_RETRY_MS,
                        )
                        .await
                        {
                            Some(applied) => {
                                active_params = applied;
                                wake_order_session.open_after_confirmation(applied);
                            }
                            None => return BleKeyboardExit::ConnectionParamsStalled,
                        }
                    }
                    idle_connection = false;
                }
                Either4::Second(()) => {
                    // Preserve last_activity and recalculate the deadline from
                    // the caller's updated runtime policy.
                }
                Either4::Third(()) => {
                    let now = Instant::now();
                    last_activity = now;
                    last_vial_activity = now;
                    if !vial_active || idle_connection {
                        if idle_connection {
                            match request_confirmed_active_params(
                                stack,
                                conn.raw(),
                                active_params,
                                2,
                                HOST_ACTIVE_CONN_PARAM_ATTEMPTS,
                                HOST_ACTIVE_CONN_PARAM_RETRY_MS,
                            )
                            .await
                            {
                                Some(applied) => {
                                    active_params = applied;
                                    wake_order_session.open_after_confirmation(applied);
                                }
                                None => return BleKeyboardExit::ConnectionParamsStalled,
                            }
                        } else {
                            update_conn_params(
                                stack,
                                conn.raw(),
                                &host_connection_params(active_params.interval, HOST_INTERACTIVE_MAX_LATENCY),
                            )
                            .await;
                        }
                    }
                    vial_active = true;
                    idle_connection = false;
                }
                Either4::Fourth(HostPowerTimer::VialIdle) => {
                    vial_active = false;
                    update_conn_params(
                        stack,
                        conn.raw(),
                        &host_connection_params(active_params.interval, HOST_IDLE_MAX_LATENCY),
                    )
                    .await;
                }
                Either4::Fourth(HostPowerTimer::Power(HostPowerTransition::EnterIdle)) => {
                    if !host_power_transition_allowed(crate::state::active_transport()) {
                        info!("Host BLE idle transition deferred while USB is active");
                        last_activity = Instant::now();
                        continue;
                    }

                    let low_duty = host_low_duty_connection_params(active_params.interval);
                    info!(
                        "[HOST_IDLE_V26] state=request interval_us={} latency={} effective_us={}",
                        low_duty.max_connection_interval.as_micros(),
                        low_duty.max_latency,
                        low_duty.max_connection_interval.as_micros() * (u64::from(low_duty.max_latency) + 1)
                    );
                    // Close before HCI submission so a concurrent keyboard
                    // wake cannot observe a stale open gate in this window.
                    wake_order_session.close_for_idle();
                    update_conn_params(stack, conn.raw(), &low_duty).await;
                    idle_connection = true;
                }
                Either4::Fourth(HostPowerTimer::Power(HostPowerTransition::Disconnect)) => {
                    if !host_power_transition_allowed(crate::state::active_transport()) {
                        info!("Host BLE disconnect deferred while USB is active");
                        last_activity = Instant::now();
                        continue;
                    }

                    set_ble_state(BleState::Sleeping);
                    request_sleep();
                    return BleKeyboardExit::IdleTimeout;
                }
            }
        }
    }

    #[cfg(feature = "host")]
    loop {
        // Slave latency reduces radio duty while idle, but it also stretches
        // every sequential Vial round trip. Switch only the configuration
        // session to latency 0;
        // repeated Vial traffic extends the session without polling.
        HOST_BLE_ACTIVITY.wait().await;
        update_conn_params(
            stack,
            conn.raw(),
            &host_active_connection_params(HOST_INTERACTIVE_MAX_LATENCY),
        )
        .await;

        while with_timeout(
            Duration::from_secs(VIAL_LINK_IDLE_TIMEOUT_SECS),
            HOST_BLE_ACTIVITY.wait(),
        )
        .await
        .is_ok()
        {}

        update_conn_params(stack, conn.raw(), &host_active_connection_params(HOST_IDLE_MAX_LATENCY)).await;
    }

    #[cfg(not(feature = "host"))]
    core::future::pending::<BleKeyboardExit>().await
}

#[cfg(not(feature = "host_first_split_wake"))]
async fn set_conn_params<'a, 'b, C: Controller + ControllerCmdSync<LeReadLocalSupportedFeatures>, P: PacketPool>(
    stack: &Stack<'_, C, P>,
    conn: &GattConnection<'a, 'b, P>,
    host_power_config: Option<BleHostPowerConfig>,
    bootstrap: HostConnParamBootstrap,
) -> BleKeyboardExit {
    if host_power_config.is_some() {
        reset_host_power_input();
        HOST_POWER_CONFIG_CHANGED.reset();
    }

    match bootstrap {
        HostConnParamBootstrap::Legacy => info!("Fresh BLE session, applying current host connection parameters"),
        HostConnParamBootstrap::BondedRefresh => {
            info!("Bonded BLE session, refreshing host connection parameters")
        }
    }

    #[cfg(feature = "host_fixed_15ms")]
    {
        info!("[HOST_DIAG_V9] mode=fixed15 requested_interval_ms=15 requested_latency=0");
        Timer::after_secs(5).await;
        let expected = HostConnParamsSnapshot {
            interval: Duration::from_millis(15),
            latency: HOST_INTERACTIVE_MAX_LATENCY,
        };
        for attempt in 1..=HOST_FIXED_CONN_PARAM_ATTEMPTS {
            HOST_CONN_PARAMS_UPDATED.reset();
            update_conn_params(
                stack,
                conn.raw(),
                &host_connection_params(expected.interval, expected.latency),
            )
            .await;

            match with_timeout(
                Duration::from_secs(HOST_CONN_PARAM_UPDATE_TIMEOUT_SECS),
                HOST_CONN_PARAMS_UPDATED.wait(),
            )
            .await
            {
                Ok(applied) if applied == expected => {
                    info!("[HOST_DIAG_V9] confirmed interval_ms=15 latency=0 attempt={}", attempt);
                    break;
                }
                Ok(applied) => warn!(
                    "[HOST_DIAG_V9] mismatch interval_ms={} latency={} attempt={}",
                    applied.interval.as_millis(),
                    applied.latency,
                    attempt
                ),
                Err(_) => warn!("[HOST_DIAG_V9] confirmation_timeout attempt={}", attempt),
            }

            if attempt < HOST_FIXED_CONN_PARAM_ATTEMPTS {
                Timer::after_millis(HOST_FIXED_CONN_PARAM_RETRY_MS).await;
            } else {
                error!("[HOST_DIAG_V9] fixed15_not_confirmed");
            }
        }
    }

    #[cfg(not(feature = "host_fixed_15ms"))]
    {
        // Ported narrowly from upstream RMK #1088. Apple hosts accept the first
        // 15 ms request; other hosts can accept the later 7.5 ms request. Run the
        // sequence for bonded sessions too so an old 15 ms bond can be upgraded
        // without deleting the profile. The delay keeps link-control procedures
        // away from pairing/encryption and mirrors the upstream timing.
        let requests = host_bootstrap_connection_requests();
        for (request_index, (interval, max_latency, supervision_timeout)) in requests.into_iter().enumerate() {
            Timer::after_secs(5).await;
            let mut params = host_connection_params(interval, max_latency);
            params.supervision_timeout = supervision_timeout;
            HOST_CONN_PARAMS_UPDATED.reset();
            update_conn_params(stack, conn.raw(), &params).await;

            if request_index == 1 {
                let applied = with_timeout(
                    Duration::from_secs(HOST_CONN_PARAM_UPDATE_TIMEOUT_SECS),
                    HOST_CONN_PARAMS_UPDATED.wait(),
                )
                .await
                .ok();

                if host_requires_apple_safe_fallback(applied) {
                    match applied {
                        Some(snapshot) => info!(
                            "Host retained {:?}ms latency {}; restoring 15ms latency 0",
                            snapshot.interval.as_millis(),
                            snapshot.latency
                        ),
                        None => info!("No 7.5ms parameter update observed; restoring 15ms latency 0"),
                    }

                    HOST_CONN_PARAMS_UPDATED.reset();
                    update_conn_params(
                        stack,
                        conn.raw(),
                        &host_connection_params(Duration::from_millis(15), HOST_INTERACTIVE_MAX_LATENCY),
                    )
                    .await;
                }
            }
        }
    }

    if let Some(config) = host_power_config {
        let mut last_activity = Instant::now();
        let mut last_vial_activity = last_activity;
        let mut idle_connection = false;
        let mut vial_active = false;

        loop {
            let (deadline, timer_action) =
                next_host_power_timer(config, idle_connection, last_activity, vial_active, last_vial_activity);
            let timer = async move {
                Timer::at(deadline).await;
                timer_action
            };

            match select4(
                wait_for_host_power_input(),
                HOST_POWER_CONFIG_CHANGED.wait(),
                wait_for_vial_activity(),
                timer,
            )
            .await
            {
                Either4::First(immediate_suspend) => {
                    if immediate_suspend {
                        set_ble_state(BleState::Sleeping);
                        // The producer already signalled the persistent sleep
                        // manager. Re-signalling here can overwrite a key's
                        // concurrent activity notification in HOST_POWER_INPUT.
                        return BleKeyboardExit::IdleTimeout;
                    }

                    last_activity = Instant::now();
                    if idle_connection && !vial_active {
                        info!("Host BLE activity, restoring active connection parameters");
                        update_conn_params(stack, conn.raw(), &host_active_connection_params(HOST_IDLE_MAX_LATENCY))
                            .await;
                    }
                    idle_connection = false;
                }
                Either4::Second(()) => {
                    // Preserve last_activity and recalculate the deadline from
                    // the caller's updated runtime policy.
                }
                Either4::Third(()) => {
                    let now = Instant::now();
                    last_activity = now;
                    last_vial_activity = now;
                    if !vial_active || idle_connection {
                        update_conn_params(
                            stack,
                            conn.raw(),
                            &host_active_connection_params(HOST_INTERACTIVE_MAX_LATENCY),
                        )
                        .await;
                    }
                    vial_active = true;
                    idle_connection = false;
                }
                Either4::Fourth(HostPowerTimer::VialIdle) => {
                    vial_active = false;
                    update_conn_params(stack, conn.raw(), &host_active_connection_params(HOST_IDLE_MAX_LATENCY)).await;
                }
                Either4::Fourth(HostPowerTimer::Power(HostPowerTransition::EnterIdle)) => {
                    if !host_power_transition_allowed(crate::state::active_transport()) {
                        info!("Host BLE idle transition deferred while USB is active");
                        last_activity = Instant::now();
                        continue;
                    }

                    info!("Host BLE idle, switching to low-duty connection parameters");
                    update_conn_params(
                        stack,
                        conn.raw(),
                        &host_connection_params(Duration::from_millis(30), HOST_IDLE_MAX_LATENCY),
                    )
                    .await;
                    idle_connection = true;
                }
                Either4::Fourth(HostPowerTimer::Power(HostPowerTransition::Disconnect)) => {
                    if !host_power_transition_allowed(crate::state::active_transport()) {
                        info!("Host BLE disconnect deferred while USB is active");
                        last_activity = Instant::now();
                        continue;
                    }

                    set_ble_state(BleState::Sleeping);
                    request_sleep();
                    return BleKeyboardExit::IdleTimeout;
                }
            }
        }
    }

    #[cfg(feature = "host")]
    loop {
        // Slave latency 30 lets an idle keyboard skip up to 30 connection
        // events, but it also makes every sequential Vial round trip wait up
        // to 232.5 ms. Switch only the configuration session to latency 0;
        // repeated Vial traffic extends the session without polling.
        HOST_BLE_ACTIVITY.wait().await;
        update_conn_params(
            stack,
            conn.raw(),
            &host_active_connection_params(HOST_INTERACTIVE_MAX_LATENCY),
        )
        .await;

        while with_timeout(
            Duration::from_secs(VIAL_LINK_IDLE_TIMEOUT_SECS),
            HOST_BLE_ACTIVITY.wait(),
        )
        .await
        .is_ok()
        {}

        update_conn_params(stack, conn.raw(), &host_active_connection_params(HOST_IDLE_MAX_LATENCY)).await;
    }

    #[cfg(not(feature = "host"))]
    core::future::pending::<BleKeyboardExit>().await
}

fn host_connection_params(interval: Duration, max_latency: u16) -> RequestedConnParams {
    RequestedConnParams {
        min_connection_interval: interval,
        max_connection_interval: interval,
        max_latency,
        min_event_length: Duration::from_secs(0),
        max_event_length: Duration::from_secs(0),
        supervision_timeout: Duration::from_secs(5),
    }
}

#[cfg(feature = "host_first_split_wake")]
fn host_low_duty_connection_params(active_interval: Duration) -> RequestedConnParams {
    let interval_us = active_interval.as_micros().max(1);
    let event_count = HOST_LOW_DUTY_EFFECTIVE_INTERVAL_US.saturating_add(interval_us - 1) / interval_us;
    let max_latency = event_count.saturating_sub(1).min(u64::from(u16::MAX)) as u16;
    host_connection_params(active_interval, max_latency)
}

fn host_active_connection_params(max_latency: u16) -> RequestedConnParams {
    #[cfg(feature = "host_fixed_15ms")]
    {
        let _ = max_latency;
        host_connection_params(Duration::from_millis(15), HOST_INTERACTIVE_MAX_LATENCY)
    }
    #[cfg(not(feature = "host_fixed_15ms"))]
    {
        host_connection_params(Duration::from_micros(7500), max_latency)
    }
}

fn host_bootstrap_connection_requests() -> [(Duration, u16, Duration); 2] {
    [
        (Duration::from_millis(15), 30, Duration::from_secs(6)),
        (Duration::from_micros(7500), 60, Duration::from_secs(6)),
    ]
}

fn host_requires_apple_safe_fallback(applied: Option<HostConnParamsSnapshot>) -> bool {
    applied.is_none_or(|snapshot| snapshot.interval > Duration::from_micros(7500))
}

#[cfg(feature = "host_first_split_wake")]
fn host_interactive_target(applied_fast: Option<HostConnParamsSnapshot>) -> HostConnParamsSnapshot {
    HostConnParamsSnapshot {
        interval: if host_requires_apple_safe_fallback(applied_fast) {
            Duration::from_millis(15)
        } else {
            Duration::from_micros(7500)
        },
        latency: HOST_INTERACTIVE_MAX_LATENCY,
    }
}

#[cfg(feature = "host_first_split_wake")]
fn host_active_params_confirmed(target: HostConnParamsSnapshot, applied: HostConnParamsSnapshot) -> bool {
    applied.latency == HOST_INTERACTIVE_MAX_LATENCY && applied.interval <= target.interval
}

#[cfg(feature = "host_first_split_wake")]
async fn request_confirmed_active_params<
    'a,
    'b,
    C: Controller + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
>(
    stack: &Stack<'a, C, P>,
    conn: &Connection<'b, P>,
    target: HostConnParamsSnapshot,
    phase: u8,
    attempts: u8,
    retry_ms: u64,
) -> Option<HostConnParamsSnapshot> {
    for attempt in 1..=attempts {
        info!(
            "[HOST_ACTIVE_V25] phase={} state=request interval_us={} latency={} attempt={}",
            phase,
            target.interval.as_micros(),
            target.latency,
            attempt
        );
        HOST_CONN_PARAMS_UPDATED.reset();
        let submitted = update_conn_params(stack, conn, &host_connection_params(target.interval, target.latency)).await;

        if submitted {
            match with_timeout(
                Duration::from_secs(HOST_CONN_PARAM_UPDATE_TIMEOUT_SECS),
                HOST_CONN_PARAMS_UPDATED.wait(),
            )
            .await
            {
                // The signal is a notification, not the connection state.
                // trouble-host updates Connection::params before enqueueing
                // the event, and an unchanged/rejected request need not emit
                // another event. A queued event can also be superseded.
                Ok(applied) => info!(
                    "[HOST_ACTIVE_V25] phase={} state=update_observed interval_us={} latency={} attempt={}",
                    phase,
                    applied.interval.as_micros(),
                    applied.latency,
                    attempt
                ),
                Err(_) => warn!(
                    "[HOST_ACTIVE_V25] phase={} state=confirmation_timeout attempt={}",
                    phase, attempt
                ),
            }
        } else {
            warn!(
                "[HOST_ACTIVE_V25] phase={} state=request_rejected attempt={}",
                phase, attempt
            );
        }

        let current = conn.params();
        let applied = HostConnParamsSnapshot {
            interval: current.conn_interval,
            latency: current.peripheral_latency,
        };
        if conn.is_connected() && host_active_params_confirmed(target, applied) {
            info!(
                "[HOST_ACTIVE_V25] phase={} state=confirmed interval_us={} latency={} attempt={}",
                phase,
                applied.interval.as_micros(),
                applied.latency,
                attempt
            );
            return Some(applied);
        }

        if attempt < attempts {
            Timer::after_millis(retry_ms).await;
        }
    }

    error!("[HOST_ACTIVE_V25] phase={} state=not_confirmed", phase);
    None
}

/// Serve one host keyboard connection.
///
/// Returns when the connection drops or its full idle timeout expires. The
/// GATT event pump starts immediately so security events cannot queue behind
/// PHY setup; only HID output waits for bonded encryption.
#[allow(clippy::too_many_arguments)]
async fn serve_keyboard_connection<
    'a,
    'b,
    #[cfg(feature = "host")] 'r,
    C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>,
>(
    server: &'b Server<'_>,
    conn: &GattConnection<'a, 'b, DefaultPacketPool>,
    stack: &Stack<'_, C, DefaultPacketPool>,
    profile: u8,
    profiles: &ProfileManager<'_, '_, C, DefaultPacketPool, BOND_SLOTS>,
    config: &BleBatteryConfig,
    host_power_config: Option<BleHostPowerConfig>,
    host_link_policy: HostLinkStartupPolicy,
    dongle_link: bool,
    #[cfg(feature = "host")] host_service: Option<&'r crate::host::HostService<'r>>,
) -> BleKeyboardExit {
    #[cfg(feature = "host")]
    HOST_BLE_ACTIVITY.reset();

    let mut ble_hid_server = BleHidServer::new(server, conn);
    let mut ble_led_reader = BleLedReader;
    let mut ble_battery_server = config.enabled.then(|| BleBatteryServer::new(server, conn));
    #[cfg(feature = "split")]
    let mut ble_peripheral_battery_server = crate::SPLIT_BATTERY_PERIPHERAL_IDS
        .first()
        .map(|_| BlePeripheralBatteryServer::new(server, conn));

    // CCCD lookup uses cached bond info to avoid a cancellable flash read while
    // this future is racing other arms of an outer `select`.
    if let Some(bond_info) = profiles.bond_info(profile)
        && bond_info.info.identity.match_identity(&conn.raw().peer_identity())
    {
        info!("Loading CCCD table: {:?}", bond_info.cccd_table);
        match ClientAttTableView::try_from_raw(&bond_info.cccd_table) {
            Ok(view) => server.set_client_att_table(conn.raw(), &view),
            Err(e) => warn!("Invalid stored CCCD table: {:?}", e),
        }
    }

    // This is a per-connection barrier, not a second connection state. The
    // GATT event pump must start immediately because trouble-host's bounded
    // connection-event queue can otherwise lose the Encrypted notification
    // while local PHY setup is still pending. Only queued HID output waits for
    // bonded encryption.
    let session_ready: Signal<crate::RawMutex, ()> = Signal::new();

    let gatt_task = run_until_physical_disconnect(
        async {
            let e = gatt_events_task(
                server,
                conn,
                stack,
                profile,
                profiles,
                &session_ready,
                host_power_config.is_some(),
            )
            .await;
            error!("[gatt_events_task] end: {:?}", e);
            BleKeyboardExit::Disconnected
        },
        || conn.raw().is_connected(),
    );

    #[cfg(not(feature = "split"))]
    let battery_task = ble_battery_server.run();
    #[cfg(feature = "split")]
    let battery_task = join(ble_battery_server.run(), ble_peripheral_battery_server.run());

    let communication_task = run_ble_communication_tasks(
        gatt_task,
        set_conn_params(stack, conn, host_power_config, host_link_policy.conn_params),
        battery_task,
        async {
            if dongle_link {
                // `use_1m_phy` exists for legacy host adapters that cannot do
                // 2M. Always use 2M for the dongle link.
                update_ble_phy(stack, conn.raw(), PhyKind::Le2M).await;
            } else if host_link_policy.update_phy {
                ensure_host_ble_2m_phy(stack, conn.raw()).await;
            } else {
                info!("BLE session, preserving host-negotiated PHY");
            }
        },
    );

    let writer_task = run_ble_hid_writer(&mut ble_hid_server, host_power_config.is_some());
    let led_task = run_led_reader(&mut ble_led_reader, ConnectionType::Ble);

    #[cfg(feature = "host")]
    let host_task = async {
        if let Some(service) = host_service {
            // Restart after a session-fatal TX error so Rynk survives the rest
            // of the connection.
            loop {
                HostGattHandler::run(server, conn, service).await;
            }
        } else {
            core::future::pending::<()>().await;
        }
    };
    #[cfg(not(feature = "host"))]
    let host_task = core::future::pending::<()>();

    // When dongle feature is enabled, send `DongleEvent` to the dongle.
    #[cfg(feature = "dongle")]
    let dongle_event_task = async {
        if !dongle_link {
            core::future::pending::<()>().await;
        }
        crate::dongle::event::run(server, conn).await;
    };
    #[cfg(not(feature = "dongle"))]
    let dongle_event_task = core::future::pending::<()>();

    let background_task = core::pin::pin!(join3(led_task, host_task, dongle_event_task));
    let workers = run_ble_session_workers(&session_ready, writer_task, background_task);

    match select(communication_task, workers).await {
        Either::First(exit) => exit,
        Either::Second(exit) => exit,
    }
}

async fn run_until_physical_disconnect<G, F>(gatt_task: G, is_connected: F) -> BleKeyboardExit
where
    G: core::future::Future<Output = BleKeyboardExit>,
    F: FnMut() -> bool,
{
    match select(gatt_task, wait_for_physical_disconnect(is_connected)).await {
        Either::First(exit) => exit,
        Either::Second(()) => {
            warn!("[gatt] physical link closed without a connection event");
            BleKeyboardExit::Disconnected
        }
    }
}

async fn wait_for_physical_disconnect<F>(mut is_connected: F)
where
    F: FnMut() -> bool,
{
    loop {
        if !is_connected() {
            return;
        }
        Timer::after_millis(HOST_CONNECTION_LIVENESS_POLL_MS).await;
    }
}

async fn disconnect_and_wait<P: PacketPool>(conn: &GattConnection<'_, '_, P>) {
    info!(
        "[BLE_TEARDOWN_V15] phase=request raw_connected={}",
        conn.raw().is_connected()
    );
    conn.raw().disconnect();
    let disconnected_event = async {
        loop {
            if let GattConnectionEvent::Disconnected { .. } = conn.next().await {
                return;
            }
        }
    };

    // In v14 this returned as soon as is_connected() became false. The host
    // runner processed DisconnectionComplete roughly 5 ms later, so the next
    // advertising command raced resource cleanup. Prefer the actual GATT
    // disconnect event; retain a bounded fallback for an overflowing event
    // queue, then still wait for the physical flag.
    match with_timeout(
        Duration::from_millis(HOST_DISCONNECT_EVENT_TIMEOUT_MS),
        disconnected_event,
    )
    .await
    {
        Ok(()) => info!("[BLE_TEARDOWN_V15] phase=disconnect_event"),
        Err(_) => warn!("[BLE_TEARDOWN_V15] phase=disconnect_event_timeout"),
    }
    wait_for_physical_disconnect(|| conn.raw().is_connected()).await;
    info!("[BLE_TEARDOWN_V15] phase=physical_down");
}

async fn ensure_host_session_released<P: PacketPool>(conn: &GattConnection<'_, '_, P>, session_id: u32) {
    if conn.raw().is_connected() {
        warn!(
            "[BLE_SESSION_V15] id={} phase=logical_exit_raw_up waiting_grace",
            session_id
        );
        if with_timeout(
            Duration::from_millis(HOST_SESSION_RELEASE_GRACE_MS),
            wait_for_physical_disconnect(|| conn.raw().is_connected()),
        )
        .await
        .is_err()
        {
            warn!("[BLE_SESSION_V15] id={} phase=forcing_disconnect", session_id);
            conn.raw().disconnect();
            wait_for_physical_disconnect(|| conn.raw().is_connected()).await;
        }
    }
    info!("[BLE_SESSION_V15] id={} phase=physical_down", session_id);
}

async fn run_ble_communication_tasks<G, C, B, P>(
    gatt_task: G,
    conn_params_task: C,
    battery_task: B,
    phy_task: P,
) -> BleKeyboardExit
where
    G: core::future::Future<Output = BleKeyboardExit>,
    C: core::future::Future<Output = BleKeyboardExit>,
    B: core::future::Future,
    P: core::future::Future,
{
    // PHY setup is finite but must stay part of the connection lifetime after
    // it completes. Its HCI mutex still serializes controller commands with
    // split-link updates; running it beside GATT only keeps event consumption
    // live while that procedure is pending.
    let phy_task = async {
        phy_task.await;
        core::future::pending::<()>().await;
    };

    match select4(gatt_task, conn_params_task, battery_task, phy_task).await {
        Either4::First(exit) | Either4::Second(exit) => exit,
        Either4::Third(_) => unreachable!("BLE battery service must run forever"),
        Either4::Fourth(_) => unreachable!("BLE PHY keeper must run forever"),
    }
}

#[cfg(test)]
async fn join_ble_session_workers<H, B>(
    session_ready: &Signal<crate::RawMutex, ()>,
    hid_task: H,
    background_task: B,
) -> (H::Output, B::Output)
where
    H: core::future::Future,
    B: core::future::Future,
{
    embassy_futures::join::join(after_ble_session_ready(session_ready, hid_task), background_task).await
}

/// Run the per-connection workers. Only the HID writer waits for the session
/// to be ready, and only its exit ends the session; `background_task` (LED,
/// host protocol, dongle events) starts with the physical connection.
///
/// `background_task` is borrowed pinned because it carries the host protocol
/// session. Moved in by value, it would be stored once as this function's
/// argument and again in each future that awaits it.
async fn run_ble_session_workers<H, B>(
    session_ready: &Signal<crate::RawMutex, ()>,
    hid_task: H,
    background_task: core::pin::Pin<&mut B>,
) -> H::Output
where
    H: core::future::Future,
    B: core::future::Future,
{
    let background_task = async {
        background_task.await;
        core::future::pending::<H::Output>().await
    };

    match select(after_ble_session_ready(session_ready, hid_task), background_task).await {
        Either::First(exit) => exit,
        Either::Second(exit) => exit,
    }
}

async fn after_ble_session_ready<F>(session_ready: &Signal<crate::RawMutex, ()>, task: F) -> F::Output
where
    F: core::future::Future,
{
    session_ready.wait().await;
    task.await
}

fn mark_ble_session_ready(session_ready: &Signal<crate::RawMutex, ()>) {
    // A physical BLE connection is not ready for HID traffic until bonded
    // encryption has completed. Publish the authoritative state first, then
    // release the per-connection workers so queued reports cannot race it.
    set_ble_state(BleState::Connected);
    session_ready.signal(());
}

async fn run_ble_hid_writer<W>(writer: &mut W, fail_closed: bool) -> BleKeyboardExit
where
    W: HidWriterTrait<ReportType = crate::hid::Report>,
{
    #[cfg(all(feature = "rtt_diag", not(feature = "mouse_interval_control")))]
    info!("[HID_DIAG_V7] mode=age_gatt_baseline");
    #[cfg(all(
        feature = "rtt_diag",
        feature = "mouse_interval_control",
        not(feature = "mouse_vector_preserve"),
        not(feature = "host_fixed_15ms")
    ))]
    info!("[HID_DIAG_V8] mode=ble15_axis_diag interval_ms=15 chunk=independent");
    #[cfg(all(
        feature = "rtt_diag",
        feature = "mouse_vector_preserve",
        not(feature = "host_fixed_15ms")
    ))]
    info!(
        "[HID_DIAG_V22] mode=ble7500_vector_preserve interval_us=7500 chunk=proportional split_source=15ms_i16 windows_per_report=2"
    );
    #[cfg(all(
        feature = "rtt_diag",
        feature = "mouse_interval_control",
        not(feature = "mouse_vector_preserve"),
        feature = "host_fixed_15ms"
    ))]
    info!("[HID_DIAG_V9] mode=axis_fixed15 interval_ms=15 latency=0 chunk=independent");
    #[cfg(all(feature = "rtt_diag", feature = "mouse_vector_preserve", feature = "host_fixed_15ms"))]
    info!("[HID_DIAG_V9] mode=vector_fixed15 interval_ms=15 latency=0 chunk=proportional");

    let mut deferred_report = None;
    let mut pending_mouse = None;
    #[cfg(feature = "mouse_interval_control")]
    let mut next_mouse_slot = None;
    loop {
        let mut mouse = if let Some(mouse) = pending_mouse.take() {
            mouse
        } else {
            let queued = if let Some(report) = deferred_report.take() {
                report
            } else {
                BLE_REPORT_CHANNEL.receive().await
            };
            let enqueued_at = queued.enqueued_at();

            match queued.into_payload() {
                QueuedReportPayload::Hid(crate::hid::Report::MouseReport(mouse)) => {
                    AccumulatedMouseReport::new(mouse, enqueued_at)
                }
                QueuedReportPayload::WideMouse(mouse) => AccumulatedMouseReport::new_wide(mouse, enqueued_at),
                QueuedReportPayload::Hid(report) => {
                    if let Err(exit) = write_ble_hid_report(writer, &report, fail_closed, None).await {
                        return exit;
                    }
                    #[cfg(feature = "fixed_mouse_pacing_15ms")]
                    {
                        // Keyboard and consumer reports consume a host event
                        // just like mouse notifications.
                        next_mouse_slot = Some(fixed_mouse_pacing_deadline(Instant::now()));
                    }
                    continue;
                }
            }
        };

        // The control build intentionally offers at most one merged mouse
        // report per configured host interval. Waiting before draining lets
        // motion samples accumulate while keyboard/button edges remain
        // ordering boundaries. The baseline build compiles this block out.
        #[cfg(feature = "mouse_interval_control")]
        {
            let wait_started = Instant::now();
            if let Some(deadline) = next_mouse_slot {
                Timer::at(deadline).await;
            }
            #[cfg(feature = "rtt_diag")]
            crate::rtt_diag::record_mouse_slot_wait(Instant::now().duration_since(wait_started).as_micros() as u32);
            #[cfg(not(feature = "rtt_diag"))]
            let _ = wait_started;
        }

        // Healthy links send every report immediately (about 125 Hz on K:04).
        // If the previous GATT write stalled, producers will have queued several
        // adjacent motion samples; fold those samples into the next report before
        // writing again. Button edges remain ordering boundaries.
        let mut merged_reports = 0u32;
        // A previously deferred button/keyboard edge must stay ahead of any
        // reports that arrived after it while a large relative delta is being
        // emitted in multiple HID-sized chunks.
        if deferred_report.is_none() {
            while let Ok(queued) = BLE_REPORT_CHANNEL.try_receive() {
                let mergeable = mouse.can_merge_payload(queued.payload());
                if mergeable {
                    let enqueued_at = queued.enqueued_at();
                    mouse.merge_payload(queued.into_payload(), enqueued_at);
                    merged_reports = merged_reports.saturating_add(1);
                } else {
                    deferred_report = Some(queued);
                    break;
                }
            }
        }

        let mouse_diag = mouse.take_write_diag();
        let (mouse_report, chunk_diag) = mouse.take_chunk();
        #[cfg(not(feature = "rtt_diag"))]
        let _ = chunk_diag;
        let report = crate::hid::Report::MouseReport(mouse_report);
        let has_residual = mouse.has_relative_motion();
        if has_residual {
            pending_mouse = Some(mouse);
        }
        #[cfg(feature = "rtt_diag")]
        crate::rtt_diag::record_mouse_coalesce(
            merged_reports,
            has_residual,
            chunk_diag.input_x,
            chunk_diag.input_y,
            chunk_diag.residual_x,
            chunk_diag.residual_y,
        );
        #[cfg(all(feature = "mouse_interval_control", not(feature = "fixed_mouse_pacing_15ms")))]
        {
            next_mouse_slot = Some(Instant::now() + MOUSE_CONTROL_INTERVAL);
        }

        if let Err(exit) = write_ble_hid_report(writer, &report, fail_closed, Some(mouse_diag)).await {
            return exit;
        }
        #[cfg(feature = "fixed_mouse_pacing_15ms")]
        {
            // Start the next slot after completion so slow GATT writes cannot
            // create an expired deadline and an immediate catch-up report.
            next_mouse_slot = Some(fixed_mouse_pacing_deadline(Instant::now()));
        }
    }
}

#[derive(Clone, Copy)]
struct MouseWriteDiag {
    oldest_enqueued_at: Instant,
    source_reports: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MouseChunkDiag {
    input_x: i32,
    input_y: i32,
    residual_x: i32,
    residual_y: i32,
}

async fn write_ble_hid_report<W>(
    writer: &mut W,
    report: &crate::hid::Report,
    fail_closed: bool,
    mouse_diag: Option<MouseWriteDiag>,
) -> Result<(), BleKeyboardExit>
where
    W: HidWriterTrait<ReportType = crate::hid::Report>,
{
    #[cfg(feature = "rtt_diag")]
    let diag_started = Instant::now();

    let result = if fail_closed {
        match with_timeout(Duration::from_secs(HID_WRITE_TIMEOUT_SECS), writer.write_report(report)).await {
            Ok(result) => result,
            Err(_) => {
                #[cfg(feature = "rtt_diag")]
                record_ble_hid_write_diag(report, diag_started, false, mouse_diag);
                error!("Timed out sending BLE HID report");
                return Err(BleKeyboardExit::HidWriteStalled);
            }
        }
    } else {
        writer.write_report(report).await
    };

    #[cfg(feature = "rtt_diag")]
    record_ble_hid_write_diag(report, diag_started, result.is_ok(), mouse_diag);

    #[cfg(not(feature = "rtt_diag"))]
    let _ = mouse_diag;

    if let Err(e) = result {
        if fail_closed {
            error!("Failed to send BLE HID report: {:?}", e);
            return Err(BleKeyboardExit::HidWriteStalled);
        }
        error!("Failed to send report: {:?}", e);
    }

    Ok(())
}

#[cfg(feature = "rtt_diag")]
fn record_ble_hid_write_diag(
    report: &crate::hid::Report,
    started_at: Instant,
    ok: bool,
    mouse_diag: Option<MouseWriteDiag>,
) {
    let completed_at = Instant::now();
    let (motion_age_us, source_reports) = mouse_diag
        .map(|diag| {
            (
                completed_at.duration_since(diag.oldest_enqueued_at).as_micros() as u32,
                diag.source_reports,
            )
        })
        .unwrap_or((0, 0));
    crate::rtt_diag::record_hid_write(
        report,
        completed_at.duration_since(started_at).as_micros() as u32,
        ok,
        BLE_REPORT_CHANNEL.len(),
        motion_age_us,
        source_reports,
    );
}

/// Relative mouse fields are accumulated at a wider width, then emitted in
/// the minimum number of valid HID-sized chunks. This preserves total motion
/// without replaying every stale sample individually.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AccumulatedMouseReport {
    buttons: u8,
    x: i32,
    y: i32,
    wheel: i32,
    pan: i32,
    oldest_enqueued_at: Instant,
    source_reports: u32,
    preserve_vector: bool,
}

impl AccumulatedMouseReport {
    fn new(report: MouseReport, enqueued_at: Instant) -> Self {
        Self {
            buttons: report.buttons,
            x: i32::from(report.x),
            y: i32::from(report.y),
            wheel: i32::from(report.wheel),
            pan: i32::from(report.pan),
            oldest_enqueued_at: enqueued_at,
            source_reports: 1,
            preserve_vector: cfg!(feature = "mouse_vector_preserve"),
        }
    }

    fn new_wide(report: WideMouseReport, enqueued_at: Instant) -> Self {
        Self {
            buttons: report.buttons,
            x: report.x,
            y: report.y,
            wheel: report.wheel,
            pan: report.pan,
            oldest_enqueued_at: enqueued_at,
            source_reports: 1,
            // The old pointing path vector-chunked every native i16 event
            // before enqueueing. Preserve that behavior after moving the
            // chunking into the transport writer.
            preserve_vector: true,
        }
    }

    fn can_merge(&self, report: &MouseReport) -> bool {
        self.buttons == report.buttons
    }

    fn can_merge_payload(&self, payload: &QueuedReportPayload) -> bool {
        match payload {
            QueuedReportPayload::Hid(crate::hid::Report::MouseReport(report)) => self.can_merge(report),
            QueuedReportPayload::WideMouse(report) => self.buttons == report.buttons,
            QueuedReportPayload::Hid(_) => false,
        }
    }

    fn merge(&mut self, report: MouseReport, enqueued_at: Instant) {
        debug_assert!(self.can_merge(&report));
        self.x = self.x.saturating_add(i32::from(report.x));
        self.y = self.y.saturating_add(i32::from(report.y));
        self.wheel = self.wheel.saturating_add(i32::from(report.wheel));
        self.pan = self.pan.saturating_add(i32::from(report.pan));
        self.oldest_enqueued_at = self.oldest_enqueued_at.min(enqueued_at);
        self.source_reports = self.source_reports.saturating_add(1);
    }

    fn merge_wide(&mut self, report: WideMouseReport, enqueued_at: Instant) {
        debug_assert_eq!(self.buttons, report.buttons);
        self.x = self.x.saturating_add(report.x);
        self.y = self.y.saturating_add(report.y);
        self.wheel = self.wheel.saturating_add(report.wheel);
        self.pan = self.pan.saturating_add(report.pan);
        self.oldest_enqueued_at = self.oldest_enqueued_at.min(enqueued_at);
        self.source_reports = self.source_reports.saturating_add(1);
        self.preserve_vector = true;
    }

    fn merge_payload(&mut self, payload: QueuedReportPayload, enqueued_at: Instant) {
        match payload {
            QueuedReportPayload::Hid(crate::hid::Report::MouseReport(report)) => self.merge(report, enqueued_at),
            QueuedReportPayload::WideMouse(report) => self.merge_wide(report, enqueued_at),
            QueuedReportPayload::Hid(_) => unreachable!("non-mouse payload passed the merge boundary"),
        }
    }

    fn take_write_diag(&mut self) -> MouseWriteDiag {
        let diag = MouseWriteDiag {
            oldest_enqueued_at: self.oldest_enqueued_at,
            source_reports: self.source_reports,
        };
        self.source_reports = 0;
        diag
    }

    fn take_chunk(&mut self) -> (MouseReport, MouseChunkDiag) {
        let input_x = self.x;
        let input_y = self.y;

        fn take_axis(value: &mut i32) -> i16 {
            let chunk = (*value).clamp(i16::MIN.into(), i16::MAX.into()) as i16;
            *value -= i32::from(chunk);
            chunk
        }

        // The 16-bit report carries any delta that fits in one write. Only a
        // merged delta past the i16 range is split, along the motion vector
        // when direction must be preserved, per axis otherwise.
        let fits = [self.x, self.y, self.wheel, self.pan]
            .iter()
            .all(|value| i16::try_from(*value).is_ok());
        let (x, y, wheel, pan) = if !fits && self.preserve_vector {
            crate::mouse_chunk::take_vector_chunk(&mut self.x, &mut self.y, &mut self.wheel, &mut self.pan)
        } else {
            (
                take_axis(&mut self.x),
                take_axis(&mut self.y),
                take_axis(&mut self.wheel),
                take_axis(&mut self.pan),
            )
        };
        let report = MouseReport {
            buttons: self.buttons,
            x,
            y,
            wheel,
            pan,
        };

        (
            report,
            MouseChunkDiag {
                input_x,
                input_y,
                residual_x: self.x,
                residual_y: self.y,
            },
        )
    }

    fn has_relative_motion(&self) -> bool {
        self.x != 0 || self.y != 0 || self.wheel != 0 || self.pan != 0
    }
}

fn prepare_hid_write_recovery() {
    crate::channel::clear_and_release_report_channel(ConnectionType::Ble);
    set_ble_state(BleState::Sleeping);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostPhyUpdateState {
    Verified,
    Retry,
    Exhausted,
}

fn host_phy_update_state(tx_phy: PhyKind, rx_phy: PhyKind, attempt: u8) -> HostPhyUpdateState {
    if tx_phy == PhyKind::Le2M && rx_phy == PhyKind::Le2M {
        HostPhyUpdateState::Verified
    } else if attempt < HOST_PHY_UPDATE_ATTEMPTS {
        HostPhyUpdateState::Retry
    } else {
        HostPhyUpdateState::Exhausted
    }
}

async fn ensure_host_ble_2m_phy<C, P>(stack: &Stack<'_, C, P>, conn: &Connection<'_, P>)
where
    C: Controller + ControllerCmdAsync<LeSetPhy> + ControllerCmdSync<LeReadPhy>,
    P: PacketPool,
{
    let _guard = BLE_HCI_LINK_UPDATE_MUTEX.lock().await;
    for attempt in 1..=HOST_PHY_UPDATE_ATTEMPTS {
        match conn.set_phy(stack, PhyKind::Le2M).await {
            Ok(()) => info!(
                "[host_phy] LE 2M update requested ({}/{})",
                attempt, HOST_PHY_UPDATE_ATTEMPTS
            ),
            Err(BleHostError::BleHost(Error::Hci(error))) => {
                warn!(
                    "[host_phy] LE 2M update request failed ({}/{}): {:?}",
                    attempt, HOST_PHY_UPDATE_ATTEMPTS, error
                );
            }
            Err(e) => {
                #[cfg(feature = "defmt")]
                let e = defmt::Debug2Format(&e);
                warn!(
                    "[host_phy] LE 2M update request failed ({}/{}): {:?}",
                    attempt, HOST_PHY_UPDATE_ATTEMPTS, e
                );
            }
        }

        // LE Set PHY completes asynchronously. Give the controller enough
        // time for more than one normal connection event before reading the
        // negotiated PHY back.
        Timer::after_millis(HOST_PHY_UPDATE_SETTLE_MS).await;

        match conn.read_phy(stack).await {
            Ok((tx_phy, rx_phy)) => match host_phy_update_state(tx_phy, rx_phy, attempt) {
                HostPhyUpdateState::Verified => {
                    info!("[host_phy] LE 2M verified");
                    return;
                }
                HostPhyUpdateState::Retry => {
                    warn!(
                        "[host_phy] still on {:?}/{:?} after attempt {}/{}",
                        tx_phy, rx_phy, attempt, HOST_PHY_UPDATE_ATTEMPTS
                    );
                }
                HostPhyUpdateState::Exhausted => {
                    warn!(
                        "[host_phy] LE 2M not negotiated; continuing on {:?}/{:?}",
                        tx_phy, rx_phy
                    );
                    return;
                }
            },
            Err(e) => {
                #[cfg(feature = "defmt")]
                let e = defmt::Debug2Format(&e);
                warn!(
                    "[host_phy] failed to read negotiated PHY ({}/{}): {:?}",
                    attempt, HOST_PHY_UPDATE_ATTEMPTS, e
                );
            }
        }

        if !conn.is_connected() {
            return;
        }
    }
}

/// Whether an HCI error means another link-layer control procedure is still
/// running on the connection, so the request can be retried.
fn link_update_busy(error: HciError) -> bool {
    error == HciError::CONTROLLER_BUSY || error == HciError::DIFFERENT_TRANSACTION_COLLISION
}

// Set the connection PHY.
pub(crate) async fn update_ble_phy<P: PacketPool>(
    stack: &Stack<'_, impl Controller + ControllerCmdAsync<LeSetPhy>, P>,
    conn: &Connection<'_, P>,
    phy: PhyKind,
) {
    let _guard = BLE_HCI_LINK_UPDATE_MUTEX.lock().await;
    for attempt in 1..=HCI_LINK_UPDATE_ATTEMPTS {
        if !conn.is_connected() {
            return;
        }

        match conn.set_phy(stack, phy).await {
            Err(BleHostError::BleHost(Error::Hci(error))) => {
                // A connection runs one link-layer control procedure at a time, and
                // a fresh one is still running its own.
                if link_update_busy(error) && attempt < HCI_LINK_UPDATE_ATTEMPTS {
                    info!(
                        "[update_ble_phy] controller busy, retry {}/{}: {:?}",
                        attempt, HCI_LINK_UPDATE_ATTEMPTS, error
                    );
                    Timer::after_millis(HCI_LINK_UPDATE_RETRY_MS).await;
                    continue;
                }
                error!("[update_ble_phy] HCI error: {:?}", error);
            }
            Err(e) => {
                #[cfg(feature = "defmt")]
                let e = defmt::Debug2Format(&e);
                error!("[update_ble_phy] error: {:?}", e);
            }
            Ok(_) => {
                info!("[update_ble_phy] PHY updated");
            }
        }
        return;
    }
}

/// Update the connection parameters.
///
/// Returns whether the request reached the controller, so callers that mirror
/// the parameters in their own state don't record params that never landed.
pub(crate) async fn update_conn_params<
    'a,
    'b,
    C: Controller + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
>(
    stack: &Stack<'a, C, P>,
    conn: &Connection<'b, P>,
    params: &RequestedConnParams,
) -> bool {
    let _guard = BLE_HCI_LINK_UPDATE_MUTEX.lock().await;
    for attempt in 1..=HCI_LINK_UPDATE_ATTEMPTS {
        if !conn.is_connected() {
            return false;
        }

        match conn.update_connection_params(stack, params).await {
            Err(BleHostError::BleHost(Error::Hci(error))) => {
                // A connection runs one link-layer control procedure at a time, and
                // a fresh one is still running its own.
                if link_update_busy(error) && attempt < HCI_LINK_UPDATE_ATTEMPTS {
                    info!(
                        "[update_conn_params] controller busy, retry {}/{}: {:?}",
                        attempt, HCI_LINK_UPDATE_ATTEMPTS, error
                    );
                    Timer::after_millis(HCI_LINK_UPDATE_RETRY_MS).await;
                    continue;
                }
                error!("[update_conn_params] HCI error: {:?}", error);
                return false;
            }
            Err(e) => {
                #[cfg(feature = "defmt")]
                let e = defmt::Debug2Format(&e);
                error!("[update_conn_params] BLE host error: {:?}", e);
                return false;
            }
            Ok(_) => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use std::sync::{Mutex, OnceLock};

    use embassy_futures::join::join;
    #[cfg(feature = "host_first_split_wake")]
    use embassy_futures::join::join3;
    use embassy_futures::select::{Either, select};
    use embassy_sync::signal::Signal;
    #[cfg(feature = "host_first_split_wake")]
    use embassy_sync::watch::Watch;
    use embassy_time::{Duration, Instant, Timer};
    use rmk_types::ble::{BleState, BleStatus};
    use trouble_host::Error;
    use trouble_host::prelude::{AdvFilterPolicy, PhyKind};

    use super::{
        BleKeyboardExit, BondedReconnectWindows, HidControlPointAction, HostConnParamBootstrap, HostLinkStartupPolicy,
        HostPhyUpdateState, HostPowerTransition, WakeAdvertisingInput, advertising_mode,
        bonded_reconnect_filter_policy, bonded_reconnect_windows, directed_reconnect_should_continue,
        hid_control_point_action, host_link_startup_policy, host_phy_update_state, host_power_transition_allowed,
        join_ble_session_workers, link_update_busy, mark_ble_session_ready, next_host_power_transition,
        pairing_window_timeout_secs, prepare_hid_write_recovery, run_ble_communication_tasks, run_ble_hid_writer,
        run_ble_session_workers, run_until_physical_disconnect,
    };
    use crate::ble::sleep::wait_for_input_activity;
    use crate::channel::{BLE_REPORT_CHANNEL, QueuedReport};
    use crate::config::BleHostPowerConfig;
    use crate::event::{
        Axis, AxisEvent, AxisValType, BleAdvertisingMode, EventSubscriber, KeyboardEvent, PointingEvent,
        SubscribableEvent, publish_event, publish_event_async,
    };
    use crate::hid::{KeyboardReport, MouseReport, Report};
    use crate::state::{
        current_ble_advertising_mode, current_ble_status, set_ble_advertising_mode, set_ble_profile, set_ble_state,
    };
    use crate::test_support::test_block_on as block_on;

    #[cfg(all(feature = "dongle", feature = "custom_message"))]
    #[test]
    fn custom_messages_survive_gatt_write_staging() {
        use postcard::experimental::max_size::MaxSize;

        use crate::custom_message::{CustomMessage, CustomMessageTarget};

        for target in [CustomMessageTarget::Central, CustomMessageTarget::Peripherals] {
            for len in [0, 30, 31, crate::CUSTOM_MESSAGE_MAX_SIZE]
                .into_iter()
                .filter(|len| *len <= crate::CUSTOM_MESSAGE_MAX_SIZE)
            {
                let payload = vec![0xA5; len];
                let message = CustomMessage::new(&payload, target).unwrap();
                let mut wire = [0; CustomMessage::POSTCARD_MAX_SIZE];
                let encoded = postcard::to_slice(&message, &mut wire).unwrap();
                let mut staged = [0; super::GATT_WRITE_BUFFER_SIZE];
                let copied = encoded.len().min(staged.len());
                staged[..copied].copy_from_slice(&encoded[..copied]);
                let decoded = postcard::from_bytes::<CustomMessage>(&staged[..copied])
                    .expect("a valid custom message must survive GATT write staging");
                assert_eq!(decoded.data.as_slice(), payload);
                assert_eq!(decoded.target, target);
            }
        }
    }

    fn ble_status_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn successful_wake_advertising_releases_temporary_input_subscribers() {
        let mut keyboard = KeyboardEvent::subscriber();
        WakeAdvertisingInput::new(true).connected();

        let final_event = block_on(async {
            select(
                async {
                    join(
                        async {
                            // The wake release plus seven taps fill all 15
                            // remaining slots held by the leaked subscriber.
                            publish_event_async(KeyboardEvent::key(0, 0, false)).await;
                            for _ in 0..7 {
                                publish_event_async(KeyboardEvent::key(0, 0, true)).await;
                                publish_event_async(KeyboardEvent::key(0, 0, false)).await;
                            }

                            // Previously this press reached HID as event 16,
                            // while its release (event 17) blocked forever.
                            publish_event_async(KeyboardEvent::key(0, 0, true)).await;
                            publish_event_async(KeyboardEvent::key(0, 0, false)).await;
                        },
                        async {
                            let mut final_event = None;
                            for _ in 0..17 {
                                final_event = Some(keyboard.next_event().await);
                            }
                            final_event.unwrap()
                        },
                    )
                    .await
                    .1
                },
                async {
                    Timer::after_millis(10).await;
                    panic!("keyboard publication remained blocked by a stale wake subscriber")
                },
            )
            .await
        });

        assert!(matches!(
            final_event,
            Either::First(KeyboardEvent { pressed: false, .. })
        ));
    }

    #[test]
    fn only_transaction_collision_and_controller_busy_retry_link_updates() {
        use bt_hci::param::Error as HciError;

        assert!(link_update_busy(HciError::DIFFERENT_TRANSACTION_COLLISION));
        assert!(link_update_busy(HciError::CONTROLLER_BUSY));
        assert!(!link_update_busy(HciError::UNKNOWN_CONN_IDENTIFIER));
    }

    #[test]
    fn advertising_without_active_bond_uses_pairing_mode() {
        assert_eq!(advertising_mode(false), BleAdvertisingMode::Pairing);
    }

    #[test]
    fn advertising_with_active_bond_uses_reconnecting_mode() {
        assert_eq!(advertising_mode(true), BleAdvertisingMode::Reconnecting);
    }

    #[test]
    fn bonded_reconnect_is_undirected_but_connection_filtered() {
        assert_eq!(bonded_reconnect_filter_policy(), AdvFilterPolicy::FilterConn);
        assert_eq!(pairing_window_timeout_secs(true, 30, 10), None);
    }

    #[test]
    fn bonded_reconnect_uses_filtered_undirected_from_the_first_packet() {
        assert_eq!(
            bonded_reconnect_windows(10_000),
            BondedReconnectWindows {
                directed_high_duty_ms: 0,
                fast_undirected_ms: 5_000,
                slow_undirected_ms: 5_000,
            }
        );
    }

    #[test]
    fn bonded_reconnect_windows_fit_short_timeouts_without_underflow() {
        assert_eq!(
            bonded_reconnect_windows(500),
            BondedReconnectWindows {
                directed_high_duty_ms: 0,
                fast_undirected_ms: 500,
                slow_undirected_ms: 0,
            }
        );
        assert_eq!(
            bonded_reconnect_windows(3_000),
            BondedReconnectWindows {
                directed_high_duty_ms: 0,
                fast_undirected_ms: 3_000,
                slow_undirected_ms: 0,
            }
        );
    }

    #[test]
    fn bonded_reconnect_windows_preserve_zero_and_large_timeouts() {
        assert_eq!(
            bonded_reconnect_windows(0),
            BondedReconnectWindows {
                directed_high_duty_ms: 0,
                fast_undirected_ms: 0,
                slow_undirected_ms: 0,
            }
        );
        assert_eq!(
            bonded_reconnect_windows(u64::MAX),
            BondedReconnectWindows {
                directed_high_duty_ms: 0,
                fast_undirected_ms: 5_000,
                slow_undirected_ms: u64::MAX - 5_000,
            }
        );
    }

    #[test]
    fn fresh_session_always_keeps_the_production_link_bootstrap() {
        assert_eq!(
            host_link_startup_policy(false, true, true),
            HostLinkStartupPolicy {
                update_phy: true,
                conn_params: HostConnParamBootstrap::Legacy,
            }
        );
    }

    #[test]
    fn bonded_host_power_session_refreshes_params_without_repeating_phy_update() {
        assert_eq!(
            host_link_startup_policy(true, true, true),
            HostLinkStartupPolicy {
                update_phy: false,
                conn_params: HostConnParamBootstrap::BondedRefresh,
            }
        );
    }

    #[test]
    fn bonded_session_without_host_power_policy_uses_legacy_bootstrap() {
        assert_eq!(
            host_link_startup_policy(true, false, true),
            HostLinkStartupPolicy {
                update_phy: true,
                conn_params: HostConnParamBootstrap::Legacy,
            }
        );
    }

    #[test]
    fn disabled_2m_phy_is_preserved_for_fresh_sessions() {
        assert_eq!(
            host_link_startup_policy(false, true, false),
            HostLinkStartupPolicy {
                update_phy: false,
                conn_params: HostConnParamBootstrap::Legacy,
            }
        );
    }

    #[test]
    fn hid_suspend_is_local_only_for_host_power_managed_links() {
        assert_eq!(hid_control_point_action(0, true), HidControlPointAction::LocalSleep);
        assert_eq!(hid_control_point_action(0, false), HidControlPointAction::Disconnect);
        assert_eq!(hid_control_point_action(1, true), HidControlPointAction::Activity);
        assert_eq!(hid_control_point_action(2, true), HidControlPointAction::Ignore);
    }

    #[test]
    fn bonded_profile_does_not_open_pairing_window() {
        assert_eq!(pairing_window_timeout_secs(true, 60, 300), None);
    }

    #[test]
    fn unbonded_profile_uses_configured_pairing_window() {
        assert_eq!(pairing_window_timeout_secs(false, 60, 300), Some(60));
    }

    #[test]
    fn unbonded_profile_preserves_legacy_pairing_timeout_fallback() {
        assert_eq!(pairing_window_timeout_secs(false, 0, 300), Some(300));
    }

    #[test]
    fn high_duty_timeout_continues_with_low_duty_reconnect() {
        assert!(directed_reconnect_should_continue(&Error::Timeout));
        assert!(!directed_reconnect_should_continue(&Error::Disconnected));
    }

    #[test]
    fn host_phy_update_stops_only_after_bidirectional_2m_is_verified() {
        assert_eq!(
            host_phy_update_state(PhyKind::Le2M, PhyKind::Le2M, 1),
            HostPhyUpdateState::Verified
        );
        assert_eq!(
            host_phy_update_state(PhyKind::Le2M, PhyKind::Le1M, 1),
            HostPhyUpdateState::Retry
        );
        assert_eq!(
            host_phy_update_state(PhyKind::Le1M, PhyKind::Le2M, 1),
            HostPhyUpdateState::Retry
        );
    }

    #[test]
    fn host_phy_update_stops_retrying_after_bounded_attempts() {
        assert_eq!(
            host_phy_update_state(PhyKind::Le1M, PhyKind::Le1M, super::HOST_PHY_UPDATE_ATTEMPTS - 1),
            HostPhyUpdateState::Retry
        );
        assert_eq!(
            host_phy_update_state(PhyKind::Le1M, PhyKind::Le1M, super::HOST_PHY_UPDATE_ATTEMPTS),
            HostPhyUpdateState::Exhausted
        );
    }

    #[cfg(not(feature = "host_first_split_wake"))]
    #[test]
    fn vial_interactive_connection_params_remove_only_slave_latency() {
        let idle = super::host_connection_params(Duration::from_micros(7500), super::HOST_IDLE_MAX_LATENCY);
        let interactive =
            super::host_connection_params(Duration::from_micros(7500), super::HOST_INTERACTIVE_MAX_LATENCY);

        assert!(idle.is_valid());
        assert!(interactive.is_valid());
        assert_eq!(idle.min_connection_interval, interactive.min_connection_interval);
        assert_eq!(idle.max_connection_interval, interactive.max_connection_interval);
        assert_eq!(idle.max_latency, 30);
        assert_eq!(interactive.max_latency, 0);
        assert_eq!(idle.supervision_timeout, interactive.supervision_timeout);
        assert_eq!(idle.supervision_timeout, Duration::from_secs(5));
    }

    #[test]
    fn host_bootstrap_requests_apple_safe_then_fast_interval() {
        let [apple_safe, fast] = super::host_bootstrap_connection_requests();

        assert_eq!(apple_safe, (Duration::from_millis(15), 30, Duration::from_secs(6)));
        assert_eq!(fast, (Duration::from_micros(7500), 60, Duration::from_secs(6)));
    }

    #[test]
    fn host_parameter_fallback_only_when_fast_interval_was_not_applied() {
        use super::{HostConnParamsSnapshot, host_requires_apple_safe_fallback};

        assert!(!host_requires_apple_safe_fallback(Some(HostConnParamsSnapshot {
            interval: Duration::from_micros(7500),
            latency: 60,
        })));
        assert!(host_requires_apple_safe_fallback(Some(HostConnParamsSnapshot {
            interval: Duration::from_millis(15),
            latency: 30,
        })));
        assert!(host_requires_apple_safe_fallback(None));
    }

    #[cfg(feature = "host_first_split_wake")]
    #[test]
    fn interactive_target_uses_zero_latency_and_the_host_supported_interval() {
        use super::{HostConnParamsSnapshot, host_interactive_target};

        assert_eq!(
            host_interactive_target(Some(HostConnParamsSnapshot {
                interval: Duration::from_micros(7500),
                latency: 60,
            })),
            HostConnParamsSnapshot {
                interval: Duration::from_micros(7500),
                latency: 0,
            }
        );
        assert_eq!(
            host_interactive_target(Some(HostConnParamsSnapshot {
                interval: Duration::from_millis(15),
                latency: 30,
            })),
            HostConnParamsSnapshot {
                interval: Duration::from_millis(15),
                latency: 0,
            }
        );
    }

    #[cfg(feature = "host_first_split_wake")]
    #[test]
    fn active_parameters_require_zero_latency_and_no_slower_interval() {
        use super::{HostConnParamsSnapshot, host_active_params_confirmed};

        let target = HostConnParamsSnapshot {
            interval: Duration::from_millis(15),
            latency: 0,
        };
        assert!(host_active_params_confirmed(target, target));
        assert!(host_active_params_confirmed(
            target,
            HostConnParamsSnapshot {
                interval: Duration::from_micros(7500),
                latency: 0,
            }
        ));
        assert!(!host_active_params_confirmed(
            target,
            HostConnParamsSnapshot {
                interval: Duration::from_millis(15),
                latency: 1,
            }
        ));
    }

    #[cfg(feature = "host_first_split_wake")]
    #[test]
    fn host_input_reconfirms_idle_connection_even_while_vial_is_active() {
        assert!(super::host_input_requires_active_confirmation(true, true));
        assert!(super::host_input_requires_active_confirmation(true, false));
        assert!(!super::host_input_requires_active_confirmation(false, true));
    }

    #[cfg(feature = "host_first_split_wake")]
    #[test]
    fn host_confirmation_and_session_end_open_wake_order_gate() {
        use super::{HostWakeOrderEvent, HostWakeOrderGate, HostWakeOrderSession, next_host_wake_order_gate};

        assert_eq!(
            next_host_wake_order_gate(HostWakeOrderGate::Open, HostWakeOrderEvent::HostEnteredIdle),
            HostWakeOrderGate::Pending
        );
        assert_eq!(
            next_host_wake_order_gate(HostWakeOrderGate::Pending, HostWakeOrderEvent::HostActiveConfirmed),
            HostWakeOrderGate::Open
        );
        let gate: Watch<crate::RawMutex, HostWakeOrderGate, 1> = Watch::new_with(HostWakeOrderGate::Open);
        let mut session = HostWakeOrderSession::new_on(&gate);
        session.close_for_idle();
        assert_eq!(gate.try_get(), Some(HostWakeOrderGate::Pending));
        drop(session);
        assert_eq!(gate.try_get(), Some(HostWakeOrderGate::Open));
    }

    #[cfg(feature = "host_first_split_wake")]
    #[test]
    fn host_confirmation_releases_existing_and_late_split_waiters() {
        use super::{HostWakeOrderGate, wait_for_host_wake_order_gate_on};

        let gate: Watch<crate::RawMutex, HostWakeOrderGate, 2> = Watch::new_with(HostWakeOrderGate::Pending);
        block_on(async {
            let first = wait_for_host_wake_order_gate_on(&gate);
            let second = wait_for_host_wake_order_gate_on(&gate);
            let confirm = async { gate.sender().send(HostWakeOrderGate::Open) };
            join3(first, second, confirm).await;
            wait_for_host_wake_order_gate_on(&gate).await;
        });
    }

    #[cfg(feature = "host_fixed_15ms")]
    #[test]
    fn fixed_diagnostic_active_params_ignore_runtime_latency_requests() {
        let idle = super::host_active_connection_params(super::HOST_IDLE_MAX_LATENCY);
        let interactive = super::host_active_connection_params(super::HOST_INTERACTIVE_MAX_LATENCY);

        for params in [idle, interactive] {
            assert_eq!(params.min_connection_interval, Duration::from_millis(15));
            assert_eq!(params.max_connection_interval, Duration::from_millis(15));
            assert_eq!(params.max_latency, 0);
            assert_eq!(params.supervision_timeout, Duration::from_secs(5));
        }
    }

    #[test]
    fn host_power_transitions_are_deferred_only_for_active_usb_output() {
        assert!(!host_power_transition_allowed(Some(
            rmk_types::connection::ConnectionType::Usb
        )));
        assert!(host_power_transition_allowed(Some(
            rmk_types::connection::ConnectionType::Ble
        )));
        assert!(host_power_transition_allowed(None));
    }

    fn ten_minute_disconnect_timeout() -> u64 {
        10 * 60
    }

    fn one_minute_disconnect_timeout() -> u64 {
        60
    }

    #[test]
    fn host_power_policy_enters_idle_before_full_disconnect() {
        let config = BleHostPowerConfig::new(Duration::from_secs(2 * 60), ten_minute_disconnect_timeout);

        assert_eq!(
            next_host_power_transition(config, false),
            (Duration::from_secs(2 * 60), HostPowerTransition::EnterIdle)
        );
        assert_eq!(
            next_host_power_transition(config, true),
            (Duration::from_secs(10 * 60), HostPowerTransition::Disconnect)
        );
    }

    #[test]
    fn host_power_policy_disconnects_directly_when_timeout_precedes_idle() {
        let config = BleHostPowerConfig::new(Duration::from_secs(2 * 60), one_minute_disconnect_timeout);

        assert_eq!(
            next_host_power_transition(config, false),
            (Duration::from_secs(60), HostPowerTransition::Disconnect)
        );
    }

    #[cfg(not(feature = "host_first_split_wake"))]
    #[test]
    fn low_duty_connection_params_use_a_longer_interval() {
        let active = super::host_connection_params(Duration::from_micros(7500), super::HOST_IDLE_MAX_LATENCY);
        let low_duty = super::host_connection_params(Duration::from_millis(30), super::HOST_IDLE_MAX_LATENCY);

        assert!(active.is_valid());
        assert!(low_duty.is_valid());
        assert!(low_duty.min_connection_interval > active.min_connection_interval);
        assert_eq!(low_duty.max_latency, active.max_latency);
    }

    #[cfg(feature = "host_first_split_wake")]
    #[test]
    fn low_duty_connection_params_keep_active_anchor_and_150ms_cadence() {
        for (interval, expected_latency) in [(Duration::from_micros(7500), 19), (Duration::from_millis(15), 9)] {
            let low_duty = super::host_low_duty_connection_params(interval);

            assert!(low_duty.is_valid());
            assert_eq!(low_duty.min_connection_interval, interval);
            assert_eq!(low_duty.max_connection_interval, interval);
            assert_eq!(low_duty.max_latency, expected_latency);
            assert_eq!(
                low_duty.max_connection_interval.as_micros() * (u64::from(low_duty.max_latency) + 1),
                super::HOST_LOW_DUTY_EFFECTIVE_INTERVAL_US
            );
        }
    }

    #[test]
    fn advertising_mode_snapshot_tracks_latest_state() {
        let _guard = ble_status_test_lock().lock().unwrap();

        set_ble_advertising_mode(BleAdvertisingMode::Pairing);
        assert_eq!(current_ble_advertising_mode(), BleAdvertisingMode::Pairing);

        set_ble_advertising_mode(BleAdvertisingMode::Reconnecting);
        assert_eq!(current_ble_advertising_mode(), BleAdvertisingMode::Reconnecting);
    }

    #[test]
    fn set_ble_state_preserves_current_profile() {
        let _guard = ble_status_test_lock().lock().unwrap();

        set_ble_profile(2, true);
        set_ble_state(BleState::Advertising);

        assert_eq!(
            current_ble_status(),
            BleStatus {
                profile: 2,
                state: BleState::Advertising,
                bonded: true,
            }
        );
    }

    #[test]
    fn set_ble_profile_resets_state_when_profile_changes() {
        let _guard = ble_status_test_lock().lock().unwrap();

        set_ble_profile(1, false);
        set_ble_state(BleState::Connected);
        set_ble_profile(3, true);

        assert_eq!(
            current_ble_status(),
            BleStatus {
                profile: 3,
                state: BleState::Inactive,
                bonded: true,
            }
        );
    }

    #[test]
    fn hid_worker_exit_ends_session_while_background_workers_are_pending() {
        let session_ready: Signal<crate::RawMutex, ()> = Signal::new();
        session_ready.signal(());

        let exit = block_on(run_ble_session_workers(
            &session_ready,
            async { BleKeyboardExit::Disconnected },
            core::pin::pin!(core::future::pending::<()>()),
        ));

        assert_eq!(exit, BleKeyboardExit::Disconnected);
    }

    struct PendingBleHidWriter;

    impl crate::hid::HidWriterTrait for PendingBleHidWriter {
        type ReportType = Report;

        fn write_report(
            &mut self,
            _report: &Self::ReportType,
        ) -> impl core::future::Future<Output = Result<usize, crate::hid::HidError>> {
            core::future::pending()
        }
    }

    fn mouse_report(buttons: u8, x: i16, y: i16, wheel: i16, pan: i16) -> MouseReport {
        MouseReport {
            buttons,
            x,
            y,
            wheel,
            pan,
        }
    }

    #[test]
    fn mouse_coalescer_sums_relative_motion_with_unchanged_buttons() {
        let now = Instant::now();
        let mut accumulated = super::AccumulatedMouseReport::new(mouse_report(1, 40, -30, 2, 0), now);
        assert!(accumulated.can_merge(&mouse_report(1, 50, -20, 3, -4)));
        accumulated.merge(mouse_report(1, 50, -20, 3, -4), now);

        let (chunk, _) = accumulated.take_chunk();
        assert_eq!(chunk.buttons, 1);
        assert_eq!((chunk.x, chunk.y, chunk.wheel, chunk.pan), (90, -50, 5, -4));
        assert!(!accumulated.has_relative_motion());
    }

    #[cfg(feature = "fixed_mouse_pacing_15ms")]
    #[test]
    fn fixed_mouse_pacing_uses_completion_time_and_one_host_event() {
        let write_started = Instant::from_millis(100);
        let completed_at = write_started + Duration::from_millis(38);
        let deadline = super::fixed_mouse_pacing_deadline(completed_at);

        assert!(cfg!(feature = "mouse_vector_preserve"));
        assert_eq!(super::MOUSE_CONTROL_INTERVAL, Duration::from_millis(15));
        assert_eq!(deadline, completed_at + Duration::from_millis(15));
        assert!(deadline > write_started + Duration::from_millis(15));
    }

    #[test]
    fn mouse_coalescer_keeps_button_edges_as_ordering_boundaries() {
        let accumulated = super::AccumulatedMouseReport::new(mouse_report(0, 10, 0, 0, 0), Instant::now());
        assert!(!accumulated.can_merge(&mouse_report(1, 5, 0, 0, 0)));
    }

    #[test]
    fn mouse_coalescer_splits_large_motion_without_losing_distance() {
        let now = Instant::now();
        let mut accumulated = super::AccumulatedMouseReport::new(mouse_report(0, i16::MAX, i16::MIN, 0, 0), now);
        accumulated.merge(mouse_report(0, i16::MAX, i16::MIN, 0, 0), now);
        accumulated.merge(mouse_report(0, 46, -44, 0, 0), now);

        let mut total_x = 0i32;
        let mut total_y = 0i32;
        let mut chunks = 0;
        loop {
            let (chunk, _) = accumulated.take_chunk();
            total_x += i32::from(chunk.x);
            total_y += i32::from(chunk.y);
            chunks += 1;
            if !accumulated.has_relative_motion() {
                break;
            }
        }

        assert_eq!(
            (total_x, total_y),
            (2 * i32::from(i16::MAX) + 46, 2 * i32::from(i16::MIN) - 44)
        );
        assert!(chunks > 1, "a delta past the i16 range needs more than one report");
    }

    #[test]
    fn sixteen_bit_report_carries_a_merged_delta_in_one_write() {
        let now = Instant::now();
        let mut accumulated = super::AccumulatedMouseReport::new(mouse_report(0, -125, 10, 0, 0), now);
        accumulated.merge(mouse_report(0, -125, 10, 0, 0), now);

        let (chunk, diag) = accumulated.take_chunk();

        assert_eq!((chunk.x, chunk.y), (-250, 20));
        assert_eq!((diag.residual_x, diag.residual_y), (0, 0));
        assert!(!accumulated.has_relative_motion());
    }

    #[cfg(feature = "mouse_vector_preserve")]
    #[test]
    fn mouse_vector_chunks_distribute_all_quadrants_without_loss() {
        for (x, y) in [
            (70_000, 21_000),
            (70_000, -21_000),
            (-70_000, 21_000),
            (-70_000, -21_000),
        ] {
            let now = Instant::now();
            let mut accumulated = super::AccumulatedMouseReport {
                buttons: 0,
                x,
                y,
                wheel: 0,
                pan: 0,
                oldest_enqueued_at: now,
                source_reports: 1,
                preserve_vector: true,
            };
            let mut total_x = 0i32;
            let mut total_y = 0i32;
            while accumulated.has_relative_motion() {
                let (chunk, _) = accumulated.take_chunk();
                // Every chunk keeps the direction of the whole motion.
                assert!(i32::from(chunk.x).signum() * x.signum() >= 0);
                assert!(i32::from(chunk.y).signum() * y.signum() >= 0);
                total_x += i32::from(chunk.x);
                total_y += i32::from(chunk.y);
            }
            assert_eq!((total_x, total_y), (x, y));
        }
    }

    // A merged delta past the i16 range used to be cut into i8 chunks for the
    // 16-bit report, so a stalled link drained it in hundreds of reports.
    #[test]
    fn vector_chunks_past_the_i16_range_use_the_whole_report() {
        let mut accumulated = super::AccumulatedMouseReport {
            buttons: 0,
            x: 70_000,
            y: -21_000,
            wheel: 0,
            pan: 0,
            oldest_enqueued_at: Instant::now(),
            source_reports: 1,
            preserve_vector: true,
        };
        let mut chunks = 0;
        while accumulated.has_relative_motion() {
            let (chunk, _) = accumulated.take_chunk();
            assert!(chunk.x > i16::from(i8::MAX));
            chunks += 1;
        }
        assert_eq!(chunks, 3);
    }

    #[test]
    fn one_extreme_wide_queue_item_preserves_the_complete_delta() {
        let mut accumulated = super::AccumulatedMouseReport::new_wide(
            crate::channel::WideMouseReport {
                buttons: 0,
                x: i32::from(i16::MAX),
                y: i32::from(i16::MIN),
                wheel: 0,
                pan: 0,
            },
            Instant::now(),
        );

        let (chunk, _) = accumulated.take_chunk();

        assert_eq!((chunk.x, chunk.y), (i16::MAX, i16::MIN));
        assert!(!accumulated.has_relative_motion());
    }

    #[test]
    fn stalled_hid_write_exits_after_bounded_timeout() {
        let _guard = ble_status_test_lock().lock().unwrap();
        BLE_REPORT_CHANNEL.clear();
        BLE_REPORT_CHANNEL
            .try_send(QueuedReport::new(Report::KeyboardReport(KeyboardReport::default())))
            .expect("BLE report channel should have capacity");

        let mut writer = PendingBleHidWriter;
        let exit = block_on(run_ble_hid_writer(&mut writer, true));

        assert_eq!(exit, BleKeyboardExit::HidWriteStalled);
        assert!(BLE_REPORT_CHANNEL.is_empty());
    }

    #[test]
    fn hid_stall_recovery_discards_stale_reports_and_queues_all_up() {
        let _guard = ble_status_test_lock().lock().unwrap();
        BLE_REPORT_CHANNEL.clear();
        set_ble_state(BleState::Connected);
        BLE_REPORT_CHANNEL
            .try_send(QueuedReport::new(Report::KeyboardReport(KeyboardReport {
                modifier: 0,
                reserved: 0,
                leds: 0,
                keycodes: [4, 0, 0, 0, 0, 0],
            })))
            .expect("BLE report channel should have capacity");

        prepare_hid_write_recovery();

        assert_eq!(current_ble_status().state, BleState::Sleeping);
        // The stale press is gone; every report type is released instead.
        let next = || BLE_REPORT_CHANNEL.try_receive().map(QueuedReport::into_report);
        assert!(matches!(next(), Ok(Report::KeyboardReport(report))
            if report.modifier == 0 && report.keycodes == [0; 6]));
        assert!(matches!(next(), Ok(Report::MouseReport(report)) if report == MouseReport::default()));
        assert!(matches!(next(), Ok(Report::MediaKeyboardReport(report)) if report.usage_id == 0));
        assert!(matches!(next(), Ok(Report::SystemControlReport(report)) if report.usage_id == 0));
        #[cfg(feature = "steno")]
        assert!(matches!(next(), Ok(Report::StenoReport(report)) if report.keys == [0; 8]));
        assert!(BLE_REPORT_CHANNEL.is_empty());

        set_ble_state(BleState::Inactive);
        BLE_REPORT_CHANNEL.clear();
    }

    #[test]
    fn only_hid_output_waits_for_encrypted_session() {
        let _guard = ble_status_test_lock().lock().unwrap();
        let session_ready: Signal<crate::RawMutex, ()> = Signal::new();
        let hid_started = Cell::new(false);
        let led_started = Cell::new(false);
        let host_started = Cell::new(false);

        BLE_REPORT_CHANNEL.clear();
        set_ble_state(BleState::Sleeping);
        assert!(
            BLE_REPORT_CHANNEL
                .try_send(QueuedReport::new(Report::KeyboardReport(KeyboardReport {
                    modifier: 0,
                    reserved: 0,
                    leds: 0,
                    keycodes: [4, 0, 0, 0, 0, 0],
                })))
                .is_ok()
        );
        assert!(
            BLE_REPORT_CHANNEL
                .try_send(QueuedReport::new(Report::KeyboardReport(KeyboardReport::default())))
                .is_ok()
        );

        let ((pressed, released), _) = block_on(async {
            join(
                join_ble_session_workers(
                    &session_ready,
                    async {
                        hid_started.set(true);
                        (BLE_REPORT_CHANNEL.receive().await, BLE_REPORT_CHANNEL.receive().await)
                    },
                    join(async { led_started.set(true) }, async { host_started.set(true) }),
                ),
                async {
                    Timer::after_millis(1).await;
                    assert!(led_started.get(), "LED work must retain its established startup order");
                    assert!(
                        host_started.get(),
                        "host service must retain its established startup order"
                    );
                    assert!(!hid_started.get(), "physical connection must not release HID output");
                    assert_eq!(BLE_REPORT_CHANNEL.len(), 2, "wake press/release must remain queued");

                    mark_ble_session_ready(&session_ready);
                },
            )
            .await
            .0
        });

        assert!(hid_started.get());
        assert!(matches!(pressed.into_report(), Report::KeyboardReport(report) if report.keycodes[0] == 4));
        assert!(matches!(released.into_report(), Report::KeyboardReport(report) if report.keycodes == [0; 6]));
        assert_eq!(current_ble_status().state, BleState::Connected);
        assert!(BLE_REPORT_CHANNEL.is_empty());

        set_ble_state(BleState::Inactive);
    }

    #[test]
    fn gatt_event_pump_runs_while_host_phy_setup_is_pending() {
        let gatt_polled = Cell::new(false);
        let phy_polled = Cell::new(false);

        let exit = block_on(run_ble_communication_tasks(
            async {
                gatt_polled.set(true);
                Timer::after_millis(1).await;
                assert!(phy_polled.get(), "PHY setup must run beside the GATT event pump");
                BleKeyboardExit::Disconnected
            },
            core::future::pending::<BleKeyboardExit>(),
            core::future::pending::<()>(),
            async {
                assert!(gatt_polled.get(), "GATT event consumption must start before PHY setup");
                phy_polled.set(true);
                core::future::pending::<()>().await;
            },
        ));

        assert_eq!(exit, BleKeyboardExit::Disconnected);
        assert!(gatt_polled.get());
        assert!(phy_polled.get());
    }

    #[test]
    fn physical_disconnect_recovers_when_connection_event_is_missing() {
        let physically_connected = Cell::new(true);

        let exit = block_on(async {
            join(
                run_until_physical_disconnect(core::future::pending::<BleKeyboardExit>(), || {
                    physically_connected.get()
                }),
                async {
                    Timer::after_millis(super::HOST_CONNECTION_LIVENESS_POLL_MS + 1).await;
                    physically_connected.set(false);
                },
            )
            .await
            .0
        });

        assert_eq!(exit, BleKeyboardExit::Disconnected);
    }

    #[test]
    fn wake_activity_ignores_noise_and_accepts_real_pointing() {
        let _guard = ble_status_test_lock().lock().unwrap();

        block_on(async {
            let woke = core::cell::Cell::new(false);
            let wake = async {
                wait_for_input_activity().await;
                woke.set(true);
            };
            join(wake, async {
                Timer::after_millis(1).await;
                publish_event(PointingEvent {
                    device_id: 0,
                    axes: [
                        AxisEvent {
                            typ: AxisValType::Rel,
                            axis: Axis::X,
                            value: 1,
                        },
                        AxisEvent {
                            typ: AxisValType::Rel,
                            axis: Axis::Y,
                            value: 0,
                        },
                        AxisEvent {
                            typ: AxisValType::Rel,
                            axis: Axis::Z,
                            value: 0,
                        },
                    ],
                });
                Timer::after_millis(1).await;
                assert!(!woke.get(), "PMW3610 settling noise must not wake BLE");

                publish_event(PointingEvent {
                    device_id: 0,
                    axes: [
                        AxisEvent {
                            typ: AxisValType::Rel,
                            axis: Axis::X,
                            value: 2,
                        },
                        AxisEvent {
                            typ: AxisValType::Rel,
                            axis: Axis::Y,
                            value: 0,
                        },
                        AxisEvent {
                            typ: AxisValType::Rel,
                            axis: Axis::Z,
                            value: 0,
                        },
                    ],
                });
            })
            .await;
            assert!(woke.get());
        });
    }

    #[test]
    fn cached_battery_level_is_seeded_into_gatt_server() {
        use rmk_types::battery::{BatteryStatus, ChargeState};

        use crate::input_device::battery::publish_battery_status;

        let server = super::Server::new_default("test").unwrap();
        let level = server.battery_service.level;

        publish_battery_status(BatteryStatus::Available {
            charge_state: ChargeState::Discharging,
            level: Some(87),
        });
        server.refresh_battery_level(level.handle);
        assert_eq!(server.get(&level).unwrap(), 87);

        publish_battery_status(BatteryStatus::Unavailable);
        server.refresh_battery_level(level.handle);
        assert_eq!(server.get(&level).unwrap(), 87);

        publish_battery_status(BatteryStatus::Available {
            charge_state: ChargeState::Discharging,
            level: Some(0),
        });
        server.refresh_battery_level(level.handle);
        assert_eq!(server.get(&level).unwrap(), 0);
    }
}
