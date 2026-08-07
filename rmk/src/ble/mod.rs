use bt_hci::cmd::le::{LeReadLocalSupportedFeatures, LeReadPhy, LeSetPhy};
use bt_hci::controller::{ControllerCmdAsync, ControllerCmdSync};
use embassy_futures::join::join3;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_sync::mutex::Mutex;
#[cfg(feature = "host")]
use embassy_sync::signal::Signal;
#[cfg(feature = "ble_zero_latency")]
use embassy_time::Instant;
use embassy_time::{Duration, Timer, with_timeout};
use rmk_types::battery::BatteryStatus;
use rmk_types::ble::BleState;
use rmk_types::connection::ConnectionType;
use rmk_types::led_indicator::LedIndicator;
use trouble_host::prelude::appearance::human_interface_device::KEYBOARD;
use trouble_host::prelude::service::{BATTERY, HUMAN_INTERFACE_DEVICE};
use trouble_host::prelude::*;

use crate::ble::battery_service::BleBatteryServer;
use crate::ble::ble_server::{BleHidServer, Server};
use crate::ble::device_info::{PnPID, VidSource};
use crate::ble::led::BleLedReader;
#[cfg(feature = "passkey_entry")]
use crate::ble::passkey::{PasskeyInputState, next_gatt_event};
use crate::ble::profile::{ProfileInfo, ProfileManager, UPDATED_CCCD_TABLE, UPDATED_PROFILE};
use crate::ble::sleep::{report_activity, request_sleep};
use crate::channel::{BLE_REPORT_CHANNEL, LED_SIGNAL};
use crate::config::{BleBatteryConfig, RmkConfig};
use crate::core_traits::Runnable;
use crate::event::{BleAdvertisingMode, SubscribableEvent};
use crate::hid::{HidWriterTrait, Report, run_led_reader};
use crate::state::set_ble_state;

pub(crate) mod battery_service;
pub(crate) mod ble_server;
pub(crate) mod device_info;
pub(crate) mod led;
#[cfg(feature = "_nrf_ble")]
pub(crate) mod nrf;
pub mod passkey;
pub(crate) mod profile;
pub(crate) mod sleep;

/// Max number of connections
pub(crate) const CONNECTIONS_MAX: usize = crate::SPLIT_PERIPHERALS_NUM + 1;

/// Max number of L2CAP channels
pub(crate) const L2CAP_CHANNELS_MAX: usize = CONNECTIONS_MAX * 4; // Signal + att + smp + hid

const DIRECTED_RECONNECT_WINDOW_MS: u64 = 1_300;
const FAST_ADVERTISING_TIMEOUT_SECS: u64 = 30;
const HOST_PHY_UPDATE_ATTEMPTS: u8 = 3;
const HOST_PHY_UPDATE_SETTLE_MS: u64 = 80;
#[cfg(not(feature = "ble_zero_latency"))]
const HOST_IDLE_MAX_LATENCY: u16 = 30;
#[cfg(feature = "ble_zero_latency")]
const HOST_IDLE_MAX_LATENCY: u16 = 0;
const HOST_INTERACTIVE_MAX_LATENCY: u16 = 0;
const VIAL_LINK_IDLE_TIMEOUT_SECS: u64 = 30;
const HCI_LINK_UPDATE_ATTEMPTS: u8 = 12;
const HCI_LINK_UPDATE_RETRY_MS: u64 = 20;
// A HID notification can enter the BLE stack much faster than the radio can
// put it on air. Give 125 Hz pointing sources one sample period to accumulate
// here, before that hidden FIFO, so a 15 ms host link receives one fresh summed
// delta instead of replaying two increasingly stale 8 ms deltas.
#[cfg(feature = "ble_zero_latency")]
const BLE_MOUSE_COALESCE_WINDOW: Duration = Duration::from_millis(8);

// The controller accepts only one link-control procedure at a time. Host PHY
// updates and one or more split links share it, so serialize our commands
// before handling controller-level collisions from procedures started by the
// peer or stack itself.
static BLE_HCI_LINK_UPDATE_MUTEX: Mutex<crate::RawMutex, ()> = Mutex::new(());

#[cfg(feature = "host")]
static VIAL_BLE_ACTIVITY: Signal<crate::RawMutex, ()> = Signal::new();

/// Build the BLE stack.
pub async fn build_ble_stack<'a, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool>(
    controller: C,
    host_address: [u8; 6],
    resources: &'a mut HostResources<P, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX>,
) -> Stack<'a, C, P> {
    // Initialize trouble host stack
    trouble_host::new(controller, resources)
        .set_random_address(Address::random(host_address))
        .build()
}

/// BLE transport runnable. Owns the trouble-host server and profile manager;
/// `run` joins the background `ble_task` runner with the advertise→connect→serve
/// loop and runs forever.
//
pub struct BleTransport<'b, 's, C>
where
    's: 'b,
    C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>,
{
    stack: &'b Stack<'s, C, DefaultPacketPool>,
    server: Server<'static>,
    profile_manager: ProfileManager<'b, 's, C, DefaultPacketPool>,
    product_name: &'static str,
    config: BleBatteryConfig<'b>,
}

impl<'b, 's, C> BleTransport<'b, 's, C>
where
    's: 'b,
    C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>,
{
    pub async fn new(stack: &'b Stack<'s, C, DefaultPacketPool>, rmk_config: RmkConfig<'static>) -> Self {
        #[cfg(feature = "_nrf_ble")]
        let serial_number = crate::ble::nrf::get_serial_number();
        #[cfg(not(feature = "_nrf_ble"))]
        let serial_number = rmk_config.device_config.serial_number;

        let profile_manager = ProfileManager::new(stack);

        info!("Starting advertising and GATT service");
        let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
            name: rmk_config.device_config.product_name,
            appearance: &appearance::human_interface_device::KEYBOARD,
        }))
        .unwrap();

        server
            .set(
                &server.device_config_service.pnp_id,
                &PnPID {
                    vid_source: VidSource::UsbIF,
                    vendor_id: rmk_config.device_config.vid,
                    product_id: rmk_config.device_config.pid,
                    product_version: 0x0001,
                },
            )
            .unwrap();
        // The serial number characteristic is length limited, so truncate at a char
        // boundary instead of panicking when the configured serial is too long.
        let mut serial_number_trimmed = heapless::String::new();
        for c in serial_number.chars() {
            if serial_number_trimmed.push(c).is_err() {
                break;
            }
        }
        server
            .set(&server.device_config_service.serial_number, &serial_number_trimmed)
            .unwrap();
        server
            .set(
                &server.device_config_service.manufacturer_name,
                &heapless::String::try_from(rmk_config.device_config.manufacturer).unwrap(),
            )
            .unwrap();

        Self {
            stack,
            server,
            profile_manager,
            product_name: rmk_config.device_config.product_name,
            config: rmk_config.ble_battery_config,
        }
    }
}

impl<'b, 's, C> Runnable for BleTransport<'b, 's, C>
where
    's: 'b,
    C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>,
{
    async fn run(&mut self) -> ! {
        // Load the preferred connection from storage
        let preferred = crate::state::load_preferred_connection().await;
        crate::state::set_preferred_connection(preferred);
        // Load the bonded devices from storage
        #[cfg(feature = "storage")]
        self.profile_manager.load_bonded_devices().await;
        self.profile_manager.update_stack_bonds();

        // Copy the &Stack reference so it doesn't tie a borrow to &mut self.
        let stack: &'b Stack<'s, C, DefaultPacketPool> = self.stack;
        let mut peripheral = stack.peripheral();
        let runner = stack.runner();

        let server = &self.server;
        let profile_manager = &mut self.profile_manager;
        let product_name = self.product_name;

        let connection_loop = async {
            loop {
                #[cfg(feature = "split")]
                if let Either::Second(()) = select(
                    crate::split::ble::central::wait_for_split_connection_window(),
                    profile_manager.update_profile(),
                )
                .await
                {
                    continue;
                }

                #[cfg(feature = "storage")]
                let active_bond_info = profile_manager.active_bond_info();
                #[cfg(feature = "storage")]
                let active_peer = active_bond_info.as_ref().map(|info| info.info.identity.addr);
                #[cfg(not(feature = "storage"))]
                let active_peer = None;

                match select(
                    advertise(product_name, &mut peripheral, server, active_peer),
                    profile_manager.update_profile(),
                )
                .await
                {
                    Either::First(Ok(conn)) => {
                        // Do NOT emit BleState::Connected here. gatt_events_task emits
                        // Connected when it sees GattConnectionEvent::Encrypted.
                        if let Either::Second(_) = select(
                            run_ble_keyboard(
                                server,
                                &conn,
                                stack,
                                #[cfg(feature = "storage")]
                                active_bond_info,
                                &self.config,
                            ),
                            profile_manager.update_profile(),
                        )
                        .await
                        {
                            // When the profile changes, manually disconnect from the current host
                            if conn.raw().is_connected() {
                                conn.raw().disconnect();
                                loop {
                                    if let GattConnectionEvent::Disconnected { .. } = conn.next().await {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Either::First(Err(BleHostError::BleHost(Error::Timeout))) => {
                        set_ble_state(BleState::Inactive);

                        // A failed BLE host window must not put the whole
                        // keyboard to sleep while another host transport is
                        // still available. This is especially important for a
                        // USB Qube: its BLE stack is also needed for split
                        // links, but the Qube itself is already connected to
                        // the PC over USB.
                        if crate::state::active_transport().is_some() {
                            warn!("Advertising timeout while another transport is active, staying awake");
                            continue;
                        }

                        warn!("Advertising timeout, sleep and wait for any key");
                        request_sleep();

                        // Wake on key or pointing activity after the advertising timeout.
                        let mut key_wake = crate::event::KeyboardEvent::subscriber();
                        let mut pointing_wake = crate::event::PointingEvent::subscriber();
                        let _ = select(key_wake.next_message_pure(), pointing_wake.next_message_pure()).await;

                        report_activity();
                    }
                    Either::First(Err(e)) => {
                        #[cfg(feature = "defmt")]
                        let e = defmt::Debug2Format(&e);
                        error!("Advertise error: {:?}", e);
                        Timer::after_millis(200).await;
                    }
                    Either::Second(()) => {}
                };

                // Skip the Inactive transition if we never moved off Advertising
                if crate::state::current_ble_status().state != BleState::Advertising {
                    set_ble_state(BleState::Inactive);
                }
            }
        };

        // Sleep ownership must outlive every host and split connection. Keeping
        // it beside the BLE runner prevents a disconnected link from leaving
        // the keyboard latched asleep.
        join3(ble_task(runner), connection_loop, sleep::run_sleep_manager()).await;
        unreachable!("BleTransport sub-tasks must run forever")
    }
}

/// This is a background task that is required to run forever alongside any other BLE tasks.
pub(crate) async fn ble_task<C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool>(
    mut runner: Runner<'_, C, P>,
) {
    loop {
        #[cfg(not(feature = "split"))]
        if let Err(_e) = runner.run().await {
            error!("[ble_task] runner.run() error");
            embassy_time::Timer::after_millis(100).await;
        }

        #[cfg(feature = "split")]
        {
            // Signal to indicate the stack is started
            crate::split::ble::central::STACK_STARTED.signal(true);
            if let Err(_e) = runner
                .run_with_handler(&crate::split::ble::central::ScanHandler {})
                .await
            {
                error!("[ble_task] runner.run_with_handler error");
                embassy_time::Timer::after_millis(100).await;
            }
        }
    }
}

/// Stream Events until the connection closes.
///
/// This function will handle the GATT events and process them.
/// This is how we interact with read and write requests.
async fn gatt_events_task(server: &Server<'_>, conn: &GattConnection<'_, '_, DefaultPacketPool>) -> Result<(), Error> {
    let level = server.battery_service.level;
    let output_keyboard = server.hid_service.output_keyboard;
    let hid_control_point = server.hid_service.hid_control_point;
    let input_keyboard = server.hid_service.input_keyboard;
    #[cfg(feature = "host")]
    let (hid_output_host, hid_input_host) = (server.hid_service.vial_output, server.hid_service.vial_input);
    #[cfg(feature = "host")]
    let (gatt_output_host, gatt_input_host) = (server.vial_gatt_service.output, server.vial_gatt_service.input);
    let mouse = server.hid_service.mouse_report;
    let media = server.hid_service.media_report;
    let system_control = server.hid_service.system_report;

    #[cfg(feature = "passkey_entry")]
    let mut passkey_state = PasskeyInputState::new();

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
                let profile = crate::state::current_profile();
                if let Some(bond_info) = bond {
                    let cccd_table = server
                        .get_client_att_table(conn.raw())
                        .and_then(|t| heapless::Vec::from_slice(t.raw()).ok())
                        .unwrap_or_default();
                    let profile_info = ProfileInfo {
                        slot_num: profile,
                        info: bond_info,
                        removed: false,
                        cccd_table,
                    };
                    UPDATED_PROFILE.signal(profile_info);
                }
            }
            GattConnectionEvent::PairingFailed(err) => {
                #[cfg(feature = "passkey_entry")]
                passkey_state.clear();
                error!("[gatt] pairing error: {:?}", err);
            }
            GattConnectionEvent::Encrypted { security_level, .. } => {
                info!("[gatt] encrypted: {:?}", security_level);
                set_ble_state(BleState::Connected);
            }
            GattConnectionEvent::Gatt { event: gatt_event } => {
                let mut cccd_updated = false;
                let result = match &gatt_event {
                    GattEvent::Read(event) => {
                        if event.handle() == level.handle {
                            let value = server.get(&level);
                            debug!("Read GATT Event to Level: {:?}", value);
                        } else {
                            debug!("Read GATT Event to Unknown: {:?}", event.handle());
                        }

                        if conn.raw().security_level()?.encrypted() {
                            None
                        } else {
                            Some(AttErrorCode::INSUFFICIENT_ENCRYPTION)
                        }
                    }
                    GattEvent::Write(event) => {
                        // trouble-host 0.7 exposes written bytes via a closure; copy them out
                        // once so the dispatch below (which awaits) can use them freely.
                        let mut data_buf = [0u8; 32];
                        let data_len = event.with_data(|_, data| {
                            let n = data.len().min(data_buf.len());
                            data_buf[..n].copy_from_slice(&data[..n]);
                            data.len()
                        });
                        let data = &data_buf[..data_len.min(data_buf.len())];

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
                        {
                            cccd_updated = true;
                        } else if event.handle() == hid_control_point.handle {
                            info!("Write GATT Event to Control Point: {:?}", event.handle());
                            // Forward HID suspend/resume to the persistent sleep manager.
                            // HID Class control point opcodes:
                            //   - 0: HID_CTRL_SUSPEND
                            //   - 1: HID_CTRL_EXIT_SUSPEND
                            if data_len == 1 {
                                match data[0] {
                                    0 => request_sleep(),
                                    1 => report_activity(),
                                    _ => {}
                                }
                            }
                        } else {
                            #[cfg(feature = "host")]
                            if event.handle() == hid_output_host.handle || event.handle() == gatt_output_host.handle {
                                debug!("Got host packet: {:?}", data);
                                if data_len == 32 {
                                    VIAL_BLE_ACTIVITY.signal(());
                                    let endpoint = if event.handle() == gatt_output_host.handle {
                                        crate::channel::BleHostTransport::VendorGatt
                                    } else {
                                        crate::channel::BleHostTransport::Hid
                                    };
                                    crate::channel::enqueue_host_request(
                                        crate::channel::HostTransport::Ble(endpoint),
                                        data_buf,
                                    )
                                    .await;
                                } else {
                                    warn!("Wrong host packet data: {:?}", data);
                                }
                            } else if event.handle() == hid_input_host.cccd_handle.expect("No CCCD for HID input host")
                                || event.handle() == gatt_input_host.cccd_handle.expect("No CCCD for GATT input host")
                            {
                                cccd_updated = true;
                            } else {
                                debug!("Write GATT Event to Unknown: {:?}", event.handle());
                            }
                            #[cfg(not(feature = "host"))]
                            debug!("Write GATT Event to Unknown: {:?}", event.handle());
                        }

                        if conn.raw().security_level()?.encrypted() {
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
                match result {
                    Ok(reply) => reply.send().await,
                    Err(e) => warn!("[gatt] error sending response: {:?}", e),
                }

                // Update CCCD table after processing the event
                if cccd_updated {
                    // When macOS wakes up from sleep mode, it won't send EXIT SUSPEND command
                    // So we need to monitor the sleep state by using CCCD write event
                    report_activity();

                    if let Some(table) = server.get_client_att_table(conn.raw())
                        && let Ok(bytes) = heapless::Vec::from_slice(table.raw())
                    {
                        UPDATED_CCCD_TABLE.signal(bytes);
                    }
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
            }
            GattConnectionEvent::RequestConnectionParams(req) => info!(
                "[gatt] RequestConnectionParams: interval: ({:?}, {:?})ms, {:?}, {:?}ms",
                req.params().min_connection_interval.as_millis(),
                req.params().max_connection_interval.as_millis(),
                req.params().max_latency,
                req.params().supervision_timeout.as_millis(),
            ),
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

/// Create an advertiser to use to connect to a BLE Central, and wait for it to connect.
async fn advertise<'a, 'b, C: Controller>(
    name: &'a str,
    peripheral: &mut Peripheral<'a, C, DefaultPacketPool>,
    server: &'b Server<'_>,
    active_peer: Option<Address>,
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
        // The established connection is still upgraded to LE 2M below.
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
    let reconnect_timeout_ms = reconnect_timeout_secs * 1_000;
    let configured_pairing_timeout = u64::from(crate::BLE_PAIRING_TIMEOUT_SECONDS);
    let has_active_peer = active_peer.is_some();
    let pairing_window_secs =
        pairing_window_timeout_secs(has_active_peer, configured_pairing_timeout, reconnect_timeout_secs);

    crate::state::set_ble_advertising_mode(advertising_mode(has_active_peer));
    set_ble_state(BleState::Advertising);

    if let Some(peer) = active_peer {
        let high_duty_window_ms = reconnect_timeout_ms.min(DIRECTED_RECONNECT_WINDOW_MS);
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

        let remaining_reconnect_ms = reconnect_timeout_ms.saturating_sub(high_duty_window_ms);
        if remaining_reconnect_ms > 0 {
            info!("[adv] directed reconnect");
            let advertiser = peripheral
                .advertise(
                    &slow_advertise_config,
                    Advertisement::ConnectableNonscannableDirected { peer },
                )
                .await?;
            match with_timeout(Duration::from_millis(remaining_reconnect_ms), advertiser.accept()).await {
                Ok(conn_res) => {
                    let conn = conn_res?.with_attribute_server(server)?;
                    info!("[adv] directed connection established");
                    if let Err(e) = conn.raw().set_bondable(true) {
                        error!("Set bondable error: {:?}", e);
                    }
                    return Ok(conn);
                }
                Err(_) => info!("[adv] bonded host reconnect timeout"),
            }
        }

        // A bonded profile must never become discoverable for a new host
        // automatically. Opening a pairing window requires an explicit bond
        // clear or switching to an unbonded profile.
        return Err(BleHostError::BleHost(Error::Timeout));
    }

    let Some(undirected_timeout_secs) = pairing_window_secs else {
        return Err(BleHostError::BleHost(Error::Timeout));
    };

    if undirected_timeout_secs == 0 {
        return Err(BleHostError::BleHost(Error::Timeout));
    }

    info!("[adv] fast undirected advertising");
    let advertiser = peripheral
        .advertise(
            &fast_advertise_config,
            Advertisement::ConnectableScannableUndirected {
                adv_data: &advertiser_data[..],
                scan_data: &[],
            },
        )
        .await?;

    let fast_timeout_secs = undirected_timeout_secs.min(FAST_ADVERTISING_TIMEOUT_SECS);
    match with_timeout(Duration::from_secs(fast_timeout_secs), advertiser.accept()).await {
        Ok(conn_res) => {
            let conn = conn_res?.with_attribute_server(server)?;
            info!("[adv] connection established");
            if let Err(e) = conn.raw().set_bondable(true) {
                error!("Set bondable error: {:?}", e);
            }
            Ok(conn)
        }
        Err(_) => {
            let slow_timeout_secs = undirected_timeout_secs.saturating_sub(fast_timeout_secs);
            if slow_timeout_secs == 0 {
                return Err(BleHostError::BleHost(Error::Timeout));
            }
            info!("[adv] slow undirected advertising");
            let advertiser = peripheral
                .advertise(
                    &slow_advertise_config,
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &advertiser_data[..],
                        scan_data: &[],
                    },
                )
                .await?;
            match with_timeout(Duration::from_secs(slow_timeout_secs), advertiser.accept()).await {
                Ok(conn_res) => {
                    let conn = conn_res?.with_attribute_server(server)?;
                    info!("[adv] connection established");
                    if let Err(e) = conn.raw().set_bondable(true) {
                        error!("Set bondable error: {:?}", e);
                    }
                    Ok(conn)
                }
                Err(_) => Err(BleHostError::BleHost(Error::Timeout)),
            }
        }
    }
}

fn advertising_mode(has_active_bond: bool) -> BleAdvertisingMode {
    if has_active_bond {
        BleAdvertisingMode::Reconnecting
    } else {
        BleAdvertisingMode::Pairing
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

pub(crate) async fn set_conn_params<
    'a,
    'b,
    C: Controller + ControllerCmdSync<LeReadLocalSupportedFeatures>,
    P: PacketPool,
>(
    stack: &Stack<'_, C, P>,
    conn: &GattConnection<'a, 'b, P>,
) {
    // Wait for 5 seconds before setting connection parameters to avoid connection drop
    embassy_time::Timer::after_secs(5).await;

    // For macOS/iOS(aka Apple devices), both interval should be set to 15ms
    // Reference: https://developer.apple.com/accessories/Accessory-Design-Guidelines.pdf
    update_conn_params(
        stack,
        conn.raw(),
        &host_connection_params(Duration::from_millis(15), HOST_IDLE_MAX_LATENCY),
    )
    .await;

    embassy_time::Timer::after_secs(5).await;

    // Setting the conn param the second time ensures that we have best performance on all platforms
    update_conn_params(
        stack,
        conn.raw(),
        &host_connection_params(Duration::from_micros(7500), HOST_IDLE_MAX_LATENCY),
    )
    .await;

    #[cfg(all(feature = "host", not(feature = "ble_zero_latency")))]
    loop {
        // Slave latency 30 lets an idle keyboard skip up to 30 connection
        // events, but it also makes every sequential Vial round trip wait up
        // to 232.5 ms. Switch only the configuration session to latency 0;
        // repeated Vial traffic extends the session without polling.
        VIAL_BLE_ACTIVITY.wait().await;
        update_conn_params(
            stack,
            conn.raw(),
            &host_connection_params(Duration::from_micros(7500), HOST_INTERACTIVE_MAX_LATENCY),
        )
        .await;

        while with_timeout(
            Duration::from_secs(VIAL_LINK_IDLE_TIMEOUT_SECS),
            VIAL_BLE_ACTIVITY.wait(),
        )
        .await
        .is_ok()
        {}

        update_conn_params(
            stack,
            conn.raw(),
            &host_connection_params(Duration::from_micros(7500), HOST_IDLE_MAX_LATENCY),
        )
        .await;
    }

    #[cfg(any(not(feature = "host"), feature = "ble_zero_latency"))]
    core::future::pending::<()>().await;
}

/// A contiguous run of relative mouse motion that can be represented by one
/// or more fresh BLE reports instead of replaying every stale queued sample.
/// Button transitions, scroll/pan and direction changes remain ordering
/// barriers and are never folded into the accumulator.
struct BleMouseMotion {
    buttons: u8,
    x: i32,
    y: i32,
}

impl BleMouseMotion {
    fn new(report: &usbd_hid::descriptor::MouseReport) -> Option<Self> {
        if report.wheel != 0 || report.pan != 0 || (report.x == 0 && report.y == 0) {
            return None;
        }
        Some(Self {
            buttons: report.buttons,
            x: i32::from(report.x),
            y: i32::from(report.y),
        })
    }

    fn try_merge(&mut self, report: &usbd_hid::descriptor::MouseReport) -> bool {
        if report.buttons != self.buttons
            || report.wheel != 0
            || report.pan != 0
            || (report.x == 0 && report.y == 0)
            || !same_motion_direction(self.x, i32::from(report.x))
            || !same_motion_direction(self.y, i32::from(report.y))
        {
            return false;
        }
        self.x = self.x.saturating_add(i32::from(report.x));
        self.y = self.y.saturating_add(i32::from(report.y));
        true
    }

    fn next_report(&mut self) -> Option<usbd_hid::descriptor::MouseReport> {
        if self.x == 0 && self.y == 0 {
            return None;
        }
        let x = self.x.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8;
        let y = self.y.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8;
        self.x -= i32::from(x);
        self.y -= i32::from(y);
        Some(usbd_hid::descriptor::MouseReport {
            buttons: self.buttons,
            x,
            y,
            wheel: 0,
            pan: 0,
        })
    }
}

fn same_motion_direction(accumulated: i32, next: i32) -> bool {
    accumulated == 0 || next == 0 || accumulated.is_positive() == next.is_positive()
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

/// Run BLE keyboard for one connection.
///
/// Returns when the GATT events task ends (i.e. the connection drops).
/// `writer_task`, `led_task`, and `host_task` are all infinite, so the outer
/// `select(communication_task, inner)` cancels them as a side-effect of
/// `communication_task` returning. `inner` itself never completes.
fn seed_battery_level(server: &Server<'_>, status: BatteryStatus) {
    if let BatteryStatus::Available { level: Some(level), .. } = status {
        server.set(&server.battery_service.level, &level).unwrap();
    }
}

async fn run_ble_keyboard<
    'a,
    'b,
    C: Controller
        + ControllerCmdAsync<LeSetPhy>
        + ControllerCmdSync<LeReadLocalSupportedFeatures>
        + ControllerCmdSync<LeReadPhy>,
>(
    server: &'b Server<'_>,
    conn: &GattConnection<'a, 'b, DefaultPacketPool>,
    stack: &Stack<'_, C, DefaultPacketPool>,
    #[cfg(feature = "storage")] active_bond_info: Option<crate::ble::profile::ProfileInfo>,
    config: &BleBatteryConfig<'a>,
) {
    #[cfg(feature = "host")]
    VIAL_BLE_ACTIVITY.reset();

    // Seed the readable GATT value before processing host requests. Otherwise
    // Windows can read the characteristic's default 0% before the delayed
    // battery notification publishes the measured level.
    if config.enabled {
        seed_battery_level(server, crate::input_device::battery::current_battery_status());
    }

    let mut ble_hid_server = BleHidServer::new(server, conn);
    let mut ble_led_reader = BleLedReader;
    let mut ble_battery_server = config.enabled.then(|| BleBatteryServer::new(server, conn));

    // CCCD lookup uses cached bond info to avoid a cancellable flash read while
    // this future is racing other arms of an outer `select`.
    #[cfg(feature = "storage")]
    if let Some(bond_info) = active_bond_info
        && bond_info.info.identity.match_identity(&conn.raw().peer_identity())
    {
        info!("Loading CCCD table: {:?}", bond_info.cccd_table);
        match ClientAttTableView::try_from_raw(&bond_info.cccd_table) {
            Ok(view) => server.set_client_att_table(conn.raw(), &view),
            Err(e) => warn!("Invalid stored CCCD table: {:?}", e),
        }
    }

    // Advertising stays on the universally discoverable LE 1M PHY. Verify
    // that the established host link actually upgrades to LE 2M: accepting
    // LE Set PHY only schedules the controller procedure and does not prove
    // that the peer completed it.
    ensure_host_ble_2m_phy(stack, conn.raw()).await;

    let communication_task = async {
        if let Either3::First(e) = select3(
            gatt_events_task(server, conn),
            set_conn_params(stack, conn),
            ble_battery_server.run(),
        )
        .await
        {
            error!("[gatt_events_task] end: {:?}", e)
        }
    };

    let writer_task = async {
        let mut pending_report = None;
        loop {
            let report = match pending_report.take() {
                Some(report) => report,
                None => BLE_REPORT_CHANNEL.receive().await,
            };

            if let Report::MouseReport(mouse) = &report
                && let Some(mut motion) = BleMouseMotion::new(mouse)
            {
                let mut batch = 1usize;

                #[cfg(feature = "ble_zero_latency")]
                {
                    let deadline = Instant::now() + BLE_MOUSE_COALESCE_WINDOW;
                    while pending_report.is_none() && batch < crate::REPORT_CHANNEL_SIZE {
                        match select(Timer::at(deadline), BLE_REPORT_CHANNEL.receive()).await {
                            Either::First(_) => break,
                            Either::Second(Report::MouseReport(candidate)) => {
                                if motion.try_merge(&candidate) {
                                    batch += 1;
                                } else {
                                    pending_report = Some(Report::MouseReport(candidate));
                                }
                            }
                            Either::Second(other) => pending_report = Some(other),
                        }
                    }
                }

                while pending_report.is_none() && batch < crate::REPORT_CHANNEL_SIZE {
                    match BLE_REPORT_CHANNEL.try_receive() {
                        Ok(Report::MouseReport(candidate)) => {
                            if motion.try_merge(&candidate) {
                                batch += 1;
                            } else {
                                pending_report = Some(Report::MouseReport(candidate));
                                break;
                            }
                        }
                        Ok(other) => {
                            pending_report = Some(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }

                if batch > 1 {
                    debug!("[ble_hid] coalesced {} queued mouse reports", batch);
                }
                while let Some(mouse) = motion.next_report() {
                    if let Err(e) = ble_hid_server.write_report(&Report::MouseReport(mouse)).await {
                        error!("Failed to send report: {:?}", e);
                        break;
                    }
                }
                continue;
            }

            if let Err(e) = ble_hid_server.write_report(&report).await {
                error!("Failed to send report: {:?}", e);
            }
        }
    };

    let led_task = run_led_reader(&mut ble_led_reader, ConnectionType::Ble);

    #[cfg(feature = "host")]
    let host_task = crate::host::ble::run_ble_host(server.hid_service.vial_input, server.vial_gatt_service.input, conn);
    #[cfg(not(feature = "host"))]
    let host_task = core::future::pending::<()>();

    let inner = embassy_futures::join::join3(writer_task, led_task, host_task);
    select(communication_task, inner).await;
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

// Update the PHY to 2M
pub(crate) async fn update_ble_phy<P: PacketPool>(
    stack: &Stack<'_, impl Controller + ControllerCmdAsync<LeSetPhy>, P>,
    conn: &Connection<'_, P>,
) {
    let _guard = BLE_HCI_LINK_UPDATE_MUTEX.lock().await;
    for attempt in 1..=HCI_LINK_UPDATE_ATTEMPTS {
        if !conn.is_connected() {
            return;
        }

        match conn.set_phy(stack, PhyKind::Le2M).await {
            Err(BleHostError::BleHost(Error::Hci(error))) => {
                if is_hci_link_update_busy(error.to_status().into_inner()) && attempt < HCI_LINK_UPDATE_ATTEMPTS {
                    info!(
                        "[update_ble_phy] HCI busy, retry {}/{}: {:?}",
                        attempt, HCI_LINK_UPDATE_ATTEMPTS, error
                    );
                    Timer::after_millis(HCI_LINK_UPDATE_RETRY_MS).await;
                    continue;
                } else {
                    error!("[update_ble_phy] HCI error: {:?}", error);
                }
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

// Update the connection parameters
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
                if is_hci_link_update_busy(error.to_status().into_inner()) && attempt < HCI_LINK_UPDATE_ATTEMPTS {
                    info!(
                        "[update_conn_params] HCI busy, retry {}/{}: {:?}",
                        attempt, HCI_LINK_UPDATE_ATTEMPTS, error
                    );
                    Timer::after_millis(HCI_LINK_UPDATE_RETRY_MS).await;
                    continue;
                } else {
                    error!("[update_conn_params] HCI error: {:?}", error);
                    return false;
                }
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

fn is_hci_link_update_busy(status: u8) -> bool {
    // 0x2a: Different Transaction Collision
    // 0x3a: Controller Busy
    matches!(status, 0x2a | 0x3a)
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};

    use embassy_futures::join::join;
    use embassy_futures::select::select;
    use embassy_time::{Duration, Timer};
    use rmk_types::battery::{BatteryStatus, ChargeState};
    use rmk_types::ble::{BleState, BleStatus};
    use trouble_host::Error;
    use trouble_host::prelude::PhyKind;
    use usbd_hid::descriptor::MouseReport;

    use super::{
        BleMouseMotion, HostPhyUpdateState, Server, advertising_mode, directed_reconnect_should_continue,
        host_phy_update_state, is_hci_link_update_busy, pairing_window_timeout_secs, same_motion_direction,
        seed_battery_level,
    };
    use crate::event::{
        Axis, AxisEvent, AxisValType, BleAdvertisingMode, KeyboardEvent, PointingEvent, SubscribableEvent,
        publish_event,
    };
    use crate::state::{
        current_ble_advertising_mode, current_ble_status, set_ble_advertising_mode, set_ble_profile, set_ble_state,
    };
    use crate::test_support::test_block_on as block_on;

    fn ble_status_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn mouse(buttons: u8, x: i8, y: i8, wheel: i8, pan: i8) -> MouseReport {
        MouseReport {
            buttons,
            x,
            y,
            wheel,
            pan,
        }
    }

    #[test]
    fn ble_mouse_motion_coalesces_same_direction_and_preserves_total() {
        let mut motion = BleMouseMotion::new(&mouse(1, 120, -120, 0, 0)).unwrap();
        assert!(motion.try_merge(&mouse(1, 120, -120, 0, 0)));

        let first = motion.next_report().unwrap();
        let second = motion.next_report().unwrap();
        assert_eq!((first.buttons, first.x, first.y), (1, 127, -128));
        assert_eq!((second.buttons, second.x, second.y), (1, 113, -112));
        assert!(motion.next_report().is_none());
    }

    #[test]
    fn ble_mouse_motion_keeps_ordering_barriers() {
        let mut motion = BleMouseMotion::new(&mouse(0, 10, 5, 0, 0)).unwrap();
        assert!(!motion.try_merge(&mouse(1, 2, 1, 0, 0)));
        assert!(!motion.try_merge(&mouse(0, 2, 1, 1, 0)));
        assert!(!motion.try_merge(&mouse(0, -2, 1, 0, 0)));
        assert!(!motion.try_merge(&mouse(0, 0, 0, 0, 0)));
        assert!(same_motion_direction(10, 0));
        assert!(!same_motion_direction(-10, 1));
    }

    #[test]
    fn only_transaction_collision_and_controller_busy_retry_link_updates() {
        assert!(is_hci_link_update_busy(0x2a));
        assert!(is_hci_link_update_busy(0x3a));
        assert!(!is_hci_link_update_busy(0x00));
        assert!(!is_hci_link_update_busy(0x08));
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

    #[test]
    fn host_connection_latency_matches_feature_policy() {
        let idle = super::host_connection_params(Duration::from_micros(7500), super::HOST_IDLE_MAX_LATENCY);
        let interactive =
            super::host_connection_params(Duration::from_micros(7500), super::HOST_INTERACTIVE_MAX_LATENCY);

        assert!(idle.is_valid());
        assert!(interactive.is_valid());
        assert_eq!(idle.min_connection_interval, interactive.min_connection_interval);
        assert_eq!(idle.max_connection_interval, interactive.max_connection_interval);
        #[cfg(not(feature = "ble_zero_latency"))]
        assert_eq!(idle.max_latency, 30);
        #[cfg(feature = "ble_zero_latency")]
        assert_eq!(idle.max_latency, 0);
        assert_eq!(interactive.max_latency, 0);
        assert_eq!(idle.supervision_timeout, interactive.supervision_timeout);
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
    fn cached_battery_level_is_seeded_into_gatt_server() {
        let server = Server::new_default("test").unwrap();

        seed_battery_level(
            &server,
            BatteryStatus::Available {
                charge_state: ChargeState::Discharging,
                level: Some(87),
            },
        );
        assert_eq!(server.get(&server.battery_service.level).unwrap(), 87);

        seed_battery_level(&server, BatteryStatus::Unavailable);
        assert_eq!(server.get(&server.battery_service.level).unwrap(), 87);

        seed_battery_level(
            &server,
            BatteryStatus::Available {
                charge_state: ChargeState::Discharging,
                level: Some(0),
            },
        );
        assert_eq!(server.get(&server.battery_service.level).unwrap(), 0);
    }

    #[test]
    fn set_ble_state_preserves_current_profile() {
        let _guard = ble_status_test_lock().lock().unwrap();

        set_ble_profile(2);
        set_ble_state(BleState::Advertising);

        assert_eq!(
            current_ble_status(),
            BleStatus {
                profile: 2,
                state: BleState::Advertising,
            }
        );
    }

    #[test]
    fn set_ble_profile_resets_state_when_profile_changes() {
        let _guard = ble_status_test_lock().lock().unwrap();

        set_ble_profile(1);
        set_ble_state(BleState::Connected);
        set_ble_profile(3);

        assert_eq!(
            current_ble_status(),
            BleStatus {
                profile: 3,
                state: BleState::Inactive,
            }
        );
    }

    #[test]
    fn wake_activity_includes_pointing_events() {
        let _guard = ble_status_test_lock().lock().unwrap();

        block_on(async {
            let wake = async {
                let mut key_wake = KeyboardEvent::subscriber();
                let mut pointing_wake = PointingEvent::subscriber();
                let _ = select(key_wake.next_message_pure(), pointing_wake.next_message_pure()).await;
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
                })
            })
            .await;
        });
    }
}
