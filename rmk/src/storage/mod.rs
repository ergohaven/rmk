#[cfg(feature = "host")]
use core::cell::RefCell;
use core::fmt::Debug;

use embassy_embedded_hal::adapter::BlockingAsync;
#[cfg(feature = "host")]
use embassy_futures::select::{Either5, select5};
#[cfg(feature = "host")]
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::signal::Signal;
#[cfg(feature = "host")]
use embassy_time::{Duration, Instant, Timer};
use embedded_storage::nor_flash::NorFlash;
use embedded_storage_async::nor_flash::NorFlash as AsyncNorFlash;
use postcard::experimental::max_size::MaxSize;
use rmk_types::connection::ConnectionType;
use rmk_types::morse::MorseProfile;
use sequential_storage::Error as SSError;
use sequential_storage::cache::NoCache;
use sequential_storage::map::{Key, MapConfig, MapStorage, PostcardValue, SerializationError};
#[cfg(feature = "host")]
use {
    crate::{MACRO_SPACE_SIZE, keyboard::combo::ComboConfig},
    rmk_types::action::{EncoderAction, KeyAction},
    rmk_types::fork::Fork,
    rmk_types::morse::Morse,
};

#[cfg(feature = "_ble")]
use crate::ble::profile::ProfileInfo;
use crate::channel::FLASH_CHANNEL;
#[cfg(feature = "host")]
use crate::channel::MACRO_FLASH_SIGNAL;
use crate::config::StorageConfig;
#[cfg(all(feature = "_ble", feature = "split"))]
use crate::split::ble::PeerAddress;
use crate::{BUILD_HASH, config};

#[cfg(feature = "host")]
const KEYMAP_STORAGE_SCHEMA_VERSION: u16 = 2;
/// Quiet period before persisting the latest complete macro snapshot. This is
/// longer than Entropy's 2.5 s BLE reply timeout, so consecutive wireless
/// editor saves collapse into one flash write without changing radio timing.
#[cfg(feature = "host")]
const MACRO_FLASH_SETTLE_TIMEOUT: Duration = Duration::from_secs(3);
/// Quiet period before draining keymap mutations to flash. Host replies are
/// sent as soon as the live keymap is updated; persistence happens here and
/// can no longer back-pressure the serial Vial service.
#[cfg(feature = "host")]
const KEYMAP_FLASH_SETTLE_TIMEOUT: Duration = Duration::from_millis(500);

#[cfg(feature = "host")]
struct PendingKeymapWrites<const N: usize> {
    actions: [Option<KeyAction>; N],
}

#[cfg(feature = "host")]
impl<const N: usize> PendingKeymapWrites<N> {
    const fn new() -> Self {
        Self { actions: [None; N] }
    }

    fn insert(&mut self, index: usize, action: KeyAction) -> bool {
        let Some(slot) = self.actions.get_mut(index) else {
            return false;
        };
        *slot = Some(action);
        true
    }

    fn take_next(&mut self) -> Option<(usize, KeyAction)> {
        self.actions
            .iter_mut()
            .enumerate()
            .find_map(|(index, action)| action.take().map(|action| (index, action)))
    }

    fn is_empty(&self) -> bool {
        self.actions.iter().all(Option::is_none)
    }

    fn clear(&mut self) {
        self.actions.fill(None);
    }
}

#[cfg(feature = "host")]
static KEYMAP_FLASH_PENDING: Mutex<
    crate::RawMutex,
    RefCell<PendingKeymapWrites<{ crate::KEYMAP_STORAGE_ENTRY_COUNT }>>,
> = Mutex::new(RefCell::new(PendingKeymapWrites::new()));

#[cfg(feature = "host")]
static KEYMAP_FLASH_SIGNAL: Signal<crate::RawMutex, ()> = Signal::new();

#[cfg(feature = "host")]
fn keymap_flat_index(layer: u8, row: u8, col: u8) -> Option<usize> {
    let (layer, row, col) = (layer as usize, row as usize, col as usize);
    (layer < crate::KEYMAP_LAYERS && row < crate::KEYMAP_ROWS && col < crate::KEYMAP_COLS)
        .then_some(layer * crate::KEYMAP_ROWS * crate::KEYMAP_COLS + row * crate::KEYMAP_COLS + col)
}

#[cfg(feature = "host")]
fn keymap_position_from_flat(index: usize) -> (u8, u8, u8) {
    let layer_size = crate::KEYMAP_ROWS * crate::KEYMAP_COLS;
    let layer = index / layer_size;
    let layer_offset = index % layer_size;
    let row = layer_offset / crate::KEYMAP_COLS;
    let col = layer_offset - row * crate::KEYMAP_COLS;
    (layer as u8, row as u8, col as u8)
}

/// Stage the latest action for one key without waiting for flash capacity.
/// Different positions are retained independently; repeated writes to the
/// same position collapse to the latest action.
#[cfg(feature = "host")]
pub(crate) fn queue_keymap_flash_write(layer: u8, row: u8, col: u8, action: KeyAction) -> bool {
    let Some(index) = keymap_flat_index(layer, row, col) else {
        error!("Invalid keymap flash position: layer {} ({},{})", layer, row, col);
        return false;
    };
    let queued = KEYMAP_FLASH_PENDING.lock(|pending| pending.borrow_mut().insert(index, action));
    if queued {
        KEYMAP_FLASH_SIGNAL.signal(());
    }
    queued
}

#[cfg(feature = "host")]
fn take_pending_keymap_flash_write() -> Option<FlashOperationMessage> {
    KEYMAP_FLASH_PENDING.lock(|pending| {
        let (index, action) = pending.borrow_mut().take_next()?;
        let (layer, row, col) = keymap_position_from_flat(index);
        Some(FlashOperationMessage::KeymapKey {
            layer,
            row,
            col,
            action,
        })
    })
}

#[cfg(feature = "host")]
fn keymap_flash_writes_pending() -> bool {
    KEYMAP_FLASH_PENDING.lock(|pending| !pending.borrow().is_empty())
}

#[cfg(feature = "host")]
fn clear_pending_keymap_flash_writes() {
    KEYMAP_FLASH_SIGNAL.reset();
    KEYMAP_FLASH_PENDING.lock(|pending| pending.borrow_mut().clear());
}

/// Signal to synchronize the flash operation status, usually used outside of the flash task.
/// True if the flash operation is finished correctly, false if the flash operation is finished with error.
pub(crate) static FLASH_OPERATION_FINISHED: Signal<crate::RawMutex, bool> = Signal::new();

// Request/response over `FLASH_CHANNEL`. One `Signal` per read variant; the
// storage task fires the matching one once it has the result.
static LAYOUT_OPTIONS_RESPONSE: Signal<crate::RawMutex, Option<u32>> = Signal::new();
#[cfg(feature = "_ble")]
static BOND_INFO_RESPONSE: Signal<crate::RawMutex, Option<ProfileInfo>> = Signal::new();
#[cfg(all(feature = "_ble", feature = "split"))]
static PEER_ADDRESS_RESPONSE: Signal<crate::RawMutex, Option<PeerAddress>> = Signal::new();
#[cfg(feature = "_ble")]
static CONNECTION_TYPE_RESPONSE: Signal<crate::RawMutex, Option<ConnectionType>> = Signal::new();
#[cfg(feature = "_ble")]
static ACTIVE_BLE_PROFILE_RESPONSE: Signal<crate::RawMutex, Option<u8>> = Signal::new();

async fn request_read<T: Send>(msg: FlashOperationMessage, response: &Signal<crate::RawMutex, T>) -> T {
    response.reset();
    FLASH_CHANNEL.send(msg).await;
    response.wait().await
}

pub(crate) async fn read_layout_options() -> Option<u32> {
    request_read(FlashOperationMessage::ReadLayoutOptions, &LAYOUT_OPTIONS_RESPONSE).await
}

#[cfg(feature = "_ble")]
pub(crate) async fn read_bond_info(slot_num: u8) -> Option<ProfileInfo> {
    request_read(FlashOperationMessage::ReadBleBondInfo(slot_num), &BOND_INFO_RESPONSE).await
}

#[cfg(all(feature = "_ble", feature = "split"))]
pub(crate) async fn read_peer_address(peer_id: u8) -> Option<PeerAddress> {
    request_read(FlashOperationMessage::ReadPeerAddress(peer_id), &PEER_ADDRESS_RESPONSE).await
}

#[cfg(feature = "_ble")]
pub(crate) async fn read_connection_type() -> Option<ConnectionType> {
    request_read(FlashOperationMessage::ReadConnectionType, &CONNECTION_TYPE_RESPONSE).await
}

#[cfg(feature = "_ble")]
pub(crate) async fn read_active_ble_profile() -> Option<u8> {
    request_read(
        FlashOperationMessage::ReadActiveBleProfile,
        &ACTIVE_BLE_PROFILE_RESPONSE,
    )
    .await
}

/// Send a peer address to be persisted; wait for the storage task to finish.
/// Returns `true` if the write completed successfully.
#[cfg(all(feature = "_ble", feature = "split"))]
pub(crate) async fn write_peer_address(addr: PeerAddress) -> bool {
    FLASH_OPERATION_FINISHED.reset();
    FLASH_CHANNEL.send(FlashOperationMessage::PeerAddress(addr)).await;
    FLASH_OPERATION_FINISHED.wait().await
}

// Message send from other tasks, which will do saving or clearing operation
#[derive(Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum FlashOperationMessage {
    #[cfg(feature = "_ble")]
    // BLE profile info to be saved
    ProfileInfo(ProfileInfo),
    #[cfg(feature = "_ble")]
    // Current active BLE profile number
    ActiveBleProfile(u8),
    #[cfg(all(feature = "_ble", feature = "split"))]
    // Peer address
    PeerAddress(PeerAddress),
    // Clear the storage
    Reset,
    // Clear the layout info
    ResetLayout,
    #[cfg(feature = "_ble")]
    // Clear info of given slot number
    ClearSlot(u8),
    // Layout option
    LayoutOptions(u32),
    // Default layer number
    DefaultLayer(u8),
    #[cfg(feature = "host")]
    KeymapKey {
        layer: u8,
        row: u8,
        col: u8,
        action: KeyAction,
    },
    #[cfg(feature = "host")]
    Encoder {
        layer: u8,
        idx: u8,
        action: EncoderAction,
    },
    #[cfg(feature = "host")]
    Combo {
        idx: u8,
        config: ComboConfig,
    },
    #[cfg(feature = "host")]
    Fork {
        idx: u8,
        fork: Fork,
    },
    #[cfg(feature = "host")]
    Morse {
        idx: u8,
        morse: Morse,
    },
    // Current saved connection type
    ConnectionType(ConnectionType),
    // Timeout time for combos
    ComboTimeout(u16),
    // Timeout time for one-shot keys
    OneShotTimeout(u16),
    // Interval for tap actions
    TapInterval(u16),
    // Interval for tapping capslock
    TapCapslockInterval(u16),
    // The prior-idle-time in ms used for in flow tap
    PriorIdleTime(u16),
    // Default morse profile containing all morse/tap-hold settings (mode, timeouts, unilateral_tap)
    MorseDefaultProfile(MorseProfile),
    #[cfg(all(feature = "host", feature = "vial"))]
    // Keyboard-specific Vial settings
    DeviceSettings(config::VialDeviceSettingsData),
    // Read persisted Vial layout options; storage replies via `LAYOUT_OPTIONS_RESPONSE`.
    ReadLayoutOptions,
    #[cfg(feature = "_ble")]
    // Read bond info for the given slot; storage task replies via `BOND_INFO_RESPONSE`.
    ReadBleBondInfo(u8),
    #[cfg(all(feature = "_ble", feature = "split"))]
    // Read peer address for the given peer id; storage task replies via `PEER_ADDRESS_RESPONSE`.
    ReadPeerAddress(u8),
    #[cfg(feature = "_ble")]
    // Read the persisted `ConnectionType`; storage task replies via `CONNECTION_TYPE_RESPONSE`.
    ReadConnectionType,
    #[cfg(feature = "_ble")]
    // Read the persisted active BLE profile number; storage task replies via `ACTIVE_BLE_PROFILE_RESPONSE`.
    ReadActiveBleProfile,
    #[cfg(feature = "universal_symbols")]
    UniversalSymbolsPlatform(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum StorageKey {
    StorageConfig,
    LayoutConfig,
    BehaviorConfig,
    ConnectionType,
    #[cfg(feature = "host")]
    MacroData,
    #[cfg(feature = "host")]
    Keymap {
        layer: u8,
        row: u8,
        col: u8,
    },
    #[cfg(feature = "host")]
    Encoder {
        layer: u8,
        idx: u8,
    },
    #[cfg(feature = "host")]
    Combo(u8),
    #[cfg(feature = "host")]
    Fork(u8),
    #[cfg(feature = "host")]
    Morse(u8),
    #[cfg(all(feature = "host", feature = "vial"))]
    DeviceSettings,
    #[cfg(all(feature = "_ble", feature = "split"))]
    PeerAddress(u8),
    #[cfg(feature = "_ble")]
    ActiveBleProfile,
    #[cfg(feature = "_ble")]
    BondInfo(u8),
    #[cfg(feature = "host")]
    KeymapSchemaVersion,
    // Keep the legacy key variants above intact: postcard encodes enum
    // discriminants by position, so moving or reusing them would make old
    // records deserialize as unrelated settings.
    #[cfg(feature = "host")]
    KeymapV2 {
        layer: u8,
        row: u8,
        col: u8,
    },
    #[cfg(feature = "host")]
    EncoderV2 {
        layer: u8,
        idx: u8,
    },
    // Optional tail namespace: profiles can replace legacy default actions
    // above a layer boundary while preserving stored lower-layer actions.
    #[cfg(feature = "host")]
    KeymapTailV3 {
        layer: u8,
        row: u8,
        col: u8,
    },
    #[cfg(feature = "host")]
    EncoderTailV3 {
        layer: u8,
        idx: u8,
    },
    // Append only: postcard discriminants of all old keys remain unchanged.
    #[cfg(feature = "universal_symbols")]
    UniversalSymbolsPlatform,
}

impl StorageKey {
    #[cfg(feature = "host")]
    pub(crate) const fn keymap(layer: u8, row: u8, col: u8) -> Self {
        Self::KeymapV2 { layer, row, col }
    }

    #[cfg(feature = "_ble")]
    pub(crate) const fn bond_info(slot_num: u8) -> Self {
        Self::BondInfo(slot_num)
    }

    #[cfg(feature = "host")]
    pub(crate) const fn combo(idx: u8) -> Self {
        Self::Combo(idx)
    }

    #[cfg(feature = "host")]
    pub(crate) const fn encoder(idx: u8, layer: u8) -> Self {
        Self::EncoderV2 { layer, idx }
    }

    #[cfg(feature = "host")]
    pub(crate) const fn fork(idx: u8) -> Self {
        Self::Fork(idx)
    }

    #[cfg(all(feature = "_ble", feature = "split"))]
    pub(crate) const fn peer_address(peer_id: u8) -> Self {
        Self::PeerAddress(peer_id)
    }

    #[cfg(feature = "host")]
    pub(crate) const fn morse(idx: u8) -> Self {
        Self::Morse(idx)
    }
}

impl Key for StorageKey {
    fn serialize_into(&self, buffer: &mut [u8]) -> Result<usize, SerializationError> {
        postcard::to_slice(self, buffer)
            .map(|used| used.len())
            .map_err(Into::into)
    }

    fn deserialize_from(buffer: &[u8]) -> Result<(Self, usize), SerializationError> {
        let (key, rest): (Self, &[u8]) = postcard::take_from_bytes(buffer).map_err(SerializationError::from)?;
        Ok((key, buffer.len() - rest.len()))
    }

    fn get_len(buffer: &[u8]) -> Result<usize, SerializationError> {
        Self::deserialize_from(buffer).map(|(_, len)| len)
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum StorageData {
    StorageConfig(LocalStorageConfig),
    LayoutConfig(LayoutConfig),
    BehaviorConfig(BehaviorConfig),
    ConnectionType(ConnectionType),
    #[cfg(feature = "host")]
    MacroData(#[serde(with = "crate::host::storage::macro_bytes_serde")] [u8; MACRO_SPACE_SIZE]),
    #[cfg(feature = "host")]
    KeyAction(KeyAction),
    #[cfg(feature = "host")]
    EncoderAction(EncoderAction),
    #[cfg(feature = "host")]
    Combo(ComboConfig),
    #[cfg(feature = "host")]
    Fork(Fork),
    #[cfg(feature = "host")]
    Morse(Morse),
    #[cfg(all(feature = "host", feature = "vial"))]
    DeviceSettings(config::VialDeviceSettingsData),
    #[cfg(all(feature = "_ble", feature = "split"))]
    PeerAddress(PeerAddress),
    #[cfg(feature = "_ble")]
    BondInfo(ProfileInfo),
    #[cfg(feature = "_ble")]
    ActiveBleProfile(u8),
    #[cfg(feature = "host")]
    KeymapSchemaVersion(u16),
    // Dedicated record, not part of versioned generic/device settings.
    #[cfg(feature = "universal_symbols")]
    UniversalSymbolsPlatform(bool),
}

impl<'a> PostcardValue<'a> for StorageData {}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, MaxSize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct LocalStorageConfig {
    enable: bool,
    build_hash: u32,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, MaxSize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct LayoutConfig {
    pub(crate) default_layer: u8,
    layout_option: u32,
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, MaxSize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct BehaviorConfig {
    // The prior-idle-time in ms used for in flow tap
    pub(crate) prior_idle_time: u16,
    // Default morse profile containing mode, timeouts, and unilateral_tap settings
    pub(crate) morse_default_profile: MorseProfile,

    // Timeout time for combos
    pub(crate) combo_timeout: u16,
    // Timeout time for one-shot keys
    pub(crate) one_shot_timeout: u16,
    // Interval for tap actions
    pub(crate) tap_interval: u16,
    // Interval for tapping capslock.
    // macOS has special processing of capslock, when tapping capslock, the tap interval should be another value
    pub(crate) tap_capslock_interval: u16,
}

impl From<LocalStorageConfig> for StorageData {
    fn from(config: LocalStorageConfig) -> Self {
        Self::StorageConfig(config)
    }
}

impl From<LayoutConfig> for StorageData {
    fn from(config: LayoutConfig) -> Self {
        Self::LayoutConfig(config)
    }
}

impl From<&config::BehaviorConfig> for StorageData {
    fn from(behavior: &config::BehaviorConfig) -> Self {
        // Note: default_layer persists via LayoutConfig (restored in read_keymap), not this struct.
        Self::BehaviorConfig(BehaviorConfig {
            prior_idle_time: behavior.morse.prior_idle_time.as_millis() as u16,
            morse_default_profile: behavior.morse.default_profile,
            combo_timeout: behavior.combo.timeout.as_millis() as u16,
            one_shot_timeout: behavior.one_shot.timeout.as_millis() as u16,
            tap_interval: behavior.tap.tap_interval,
            tap_capslock_interval: behavior.tap.tap_capslock_interval,
        })
    }
}

pub fn async_flash_wrapper<F: NorFlash>(flash: F) -> BlockingAsync<F> {
    embassy_embedded_hal::adapter::BlockingAsync::new(flash)
}

#[cfg(feature = "split")]
pub async fn new_storage_for_split_peripheral<F: AsyncNorFlash>(
    flash: F,
    storage_config: StorageConfig,
) -> Storage<F, 0, 0, 0, 0> {
    Storage::<F, 0, 0, 0, 0>::new(
        flash,
        #[cfg(feature = "host")]
        &[],
        #[cfg(feature = "host")]
        &None,
        &storage_config,
        &config::BehaviorConfig::default(),
    )
    .await
}

pub struct Storage<
    F: AsyncNorFlash,
    const ROW: usize,
    const COL: usize,
    const NUM_LAYER: usize,
    const NUM_ENCODER: usize = 0,
> {
    pub(crate) flash: MapStorage<StorageKey, F, NoCache>,
    pub(crate) buffer: [u8; get_buffer_size()],
    no_action_layer_start: Option<u8>,
}

/// Read out storage config, update and then save back.
/// This macro applies to only some of the configs.
macro_rules! update_storage_field {
    ($f: expr, $buf: expr, $key:ident, $field:ident) => {{
        let key = StorageKey::$key;
        if let Ok(Some(StorageData::$key(mut saved))) = $f.fetch_item($buf, &key).await {
            saved.$field = $field;
            $f.store_item($buf, &key, &StorageData::$key(saved)).await
        } else {
            Ok(())
        }
    }};
}

impl<F: AsyncNorFlash, const ROW: usize, const COL: usize, const NUM_LAYER: usize, const NUM_ENCODER: usize>
    Storage<F, ROW, COL, NUM_LAYER, NUM_ENCODER>
{
    #[cfg(feature = "host")]
    pub(crate) fn uses_tail_key_namespace(&self, layer: u8) -> bool {
        self.no_action_layer_start.is_some_and(|start| layer >= start)
    }

    #[cfg(feature = "host")]
    pub(crate) fn tail_key_namespace_start(&self) -> Option<u8> {
        self.no_action_layer_start
    }

    #[cfg(feature = "host")]
    fn keymap_storage_key(&self, layer: u8, row: u8, col: u8) -> StorageKey {
        if self.uses_tail_key_namespace(layer) {
            StorageKey::KeymapTailV3 { layer, row, col }
        } else {
            StorageKey::KeymapV2 { layer, row, col }
        }
    }

    #[cfg(feature = "host")]
    fn encoder_storage_key(&self, idx: u8, layer: u8) -> StorageKey {
        if self.uses_tail_key_namespace(layer) {
            StorageKey::EncoderTailV3 { layer, idx }
        } else {
            StorageKey::EncoderV2 { layer, idx }
        }
    }

    async fn fetch_data(&mut self, key: StorageKey) -> Option<StorageData> {
        match self.flash.fetch_item(&mut self.buffer, &key).await {
            Ok(data) => data,
            Err(e) => {
                print_storage_error::<F>(e);
                None
            }
        }
    }

    async fn store_data(&mut self, key: StorageKey, data: &StorageData) -> Result<(), SSError<F::Error>> {
        self.flash.store_item(&mut self.buffer, &key, data).await
    }

    pub async fn new(
        flash: F,
        #[cfg(feature = "host")] keymap: &[[[KeyAction; COL]; ROW]; NUM_LAYER],
        #[cfg(feature = "host")] encoder_map: &Option<&mut [[EncoderAction; NUM_ENCODER]; NUM_LAYER]>,
        storage_config: &StorageConfig,
        behavior_config: &config::BehaviorConfig,
    ) -> Self {
        // Check storage setting
        assert!(
            storage_config.num_sectors >= 2,
            "Number of used sector for storage must larger than 1"
        );

        // If config.start_addr == 0:
        // - For nRF chips: use sectors starting at 0x0006_0000
        // - For other chips: use the last `num_sectors` sectors
        // Otherwise, use storage config setting
        // When DFU is active the storage partition already sits at the correct
        // offset — the _nrf_ble special case (0x60000) only applies without DFU.
        #[cfg(all(feature = "_nrf_ble", not(any(feature = "dfu_rp", feature = "dfu_nrf"))))]
        let start_addr = if storage_config.start_addr == 0 {
            0x0006_0000
        } else {
            storage_config.start_addr
        };

        #[cfg(not(all(feature = "_nrf_ble", not(any(feature = "dfu_rp", feature = "dfu_nrf")))))]
        let start_addr = storage_config.start_addr;
        // Check storage setting
        info!(
            "Flash capacity {} KB, RMK use {} KB({} sectors) starting from 0x{:X} as storage",
            flash.capacity() / 1024,
            (F::ERASE_SIZE * storage_config.num_sectors as usize) / 1024,
            storage_config.num_sectors,
            storage_config.start_addr,
        );

        let storage_range = if start_addr == 0 {
            (flash.capacity() - storage_config.num_sectors as usize * F::ERASE_SIZE) as u32..flash.capacity() as u32
        } else {
            assert!(
                start_addr.is_multiple_of(F::ERASE_SIZE),
                "Storage's start addr MUST BE a multiplier of sector size"
            );
            start_addr as u32..(start_addr + storage_config.num_sectors as usize * F::ERASE_SIZE) as u32
        };

        let mut storage = Self {
            flash: MapStorage::new(flash, MapConfig::new(storage_range), NoCache::new()),
            buffer: [0; get_buffer_size()],
            no_action_layer_start: storage_config
                .no_action_layer_start
                .filter(|start| usize::from(*start) < NUM_LAYER),
        };

        // Check whether keymap and configs have been storaged in flash
        if !storage.check_enable().await || storage_config.clear_storage {
            // Clear storage first
            debug!("Clearing storage!");
            let _ = storage.flash.erase_all().await;

            // Initialize storage from keymap and config
            if storage
                .initialize_storage_with_config(
                    #[cfg(feature = "host")]
                    keymap,
                    #[cfg(feature = "host")]
                    encoder_map,
                    behavior_config,
                )
                .await
                .is_err()
            {
                // When there's an error, `enable: false` should be saved back to storage, preventing partial initialization of storage
                storage
                    .store_data(
                        StorageKey::StorageConfig,
                        &StorageData::from(LocalStorageConfig {
                            enable: false,
                            build_hash: BUILD_HASH,
                        }),
                    )
                    .await
                    .ok();
            }
        } else {
            #[cfg(feature = "host")]
            {
                let schema_is_current = storage.keymap_schema_is_current().await;
                if storage_config.clear_layout {
                    debug!("clear_layout=true; overwriting layout items without erase.");
                    let encoder_map = encoder_map.as_ref().map(|m| &**m);
                    if let Err(e) = storage.reset_layout_only(keymap, &encoder_map, behavior_config).await {
                        print_storage_error::<F>(e);
                    }
                } else if !schema_is_current {
                    warn!("Stored keymap schema is incompatible; activating the factory keymap.");
                    // Legacy key records use their original key namespace and
                    // remain readable, but `read_runtime_state` deliberately
                    // ignores them. The compile-time keymap is already the
                    // factory layout, so migration only needs this marker.
                    // Avoiding 960 individual writes keeps K:04 startup from
                    // stalling for tens of seconds before matrix/BLE tasks run.
                    if let Err(e) = storage.mark_keymap_schema_current().await {
                        print_storage_error::<F>(e);
                    }
                }
            }
        }

        #[cfg(feature = "universal_symbols")]
        {
            let mac = matches!(
                storage.fetch_data(StorageKey::UniversalSymbolsPlatform).await,
                Some(StorageData::UniversalSymbolsPlatform(true))
            );
            crate::universal_symbols::restore_platform(mac);
            let options = match storage.fetch_data(StorageKey::LayoutConfig).await {
                Some(StorageData::LayoutConfig(config)) => config.layout_option,
                _ => 0,
            };
            crate::universal_symbols::set_layout_options(options);
        }

        storage
    }

    #[cfg(all(feature = "host", feature = "vial"))]
    pub(crate) async fn read_device_settings(&mut self) -> Result<Option<config::VialDeviceSettingsData>, ()> {
        match self
            .flash
            .fetch_item(&mut self.buffer, &StorageKey::DeviceSettings)
            .await
            .map_err(|e| print_storage_error::<F>(e))?
        {
            Some(StorageData::DeviceSettings(data)) => Ok(Some(data)),
            _ => Ok(None),
        }
    }

    async fn initialize_storage_with_config(
        &mut self,
        #[cfg(feature = "host")] keymap: &[[[KeyAction; COL]; ROW]; NUM_LAYER],
        #[cfg(feature = "host")] encoder_map: &Option<&mut [[EncoderAction; NUM_ENCODER]; NUM_LAYER]>,
        behavior: &config::BehaviorConfig,
    ) -> Result<(), ()> {
        // Save storage config
        self.store_data(
            StorageKey::StorageConfig,
            &StorageData::from(LocalStorageConfig {
                enable: true,
                build_hash: BUILD_HASH,
            }),
        )
        .await
        .map_err(|e| print_storage_error::<F>(e))?;

        // Save layout config
        self.store_data(
            StorageKey::LayoutConfig,
            &StorageData::from(LayoutConfig {
                default_layer: 0,
                layout_option: 0,
            }),
        )
        .await
        .map_err(|e| print_storage_error::<F>(e))?;

        // Save behavior config
        self.store_data(StorageKey::BehaviorConfig, &StorageData::from(behavior))
            .await
            .map_err(|e| print_storage_error::<F>(e))?;

        #[cfg(feature = "host")]
        for (layer, layer_data) in keymap.iter().enumerate() {
            for (row, row_data) in layer_data.iter().enumerate() {
                for (col, action) in row_data.iter().enumerate() {
                    let key = self.keymap_storage_key(layer as u8, row as u8, col as u8);
                    self.store_data(key, &StorageData::KeyAction(*action))
                        .await
                        .map_err(|e| print_storage_error::<F>(e))?;
                }
            }
        }

        // Save encoder configurations
        #[cfg(feature = "host")]
        if let Some(encoder_map) = encoder_map {
            for (layer, layer_data) in encoder_map.iter().enumerate() {
                for (idx, action) in layer_data.iter().enumerate() {
                    let key = self.encoder_storage_key(idx as u8, layer as u8);
                    self.store_data(key, &StorageData::EncoderAction(*action))
                        .await
                        .map_err(|e| print_storage_error::<F>(e))?;
                }
            }
        }

        #[cfg(feature = "host")]
        self.store_data(
            StorageKey::KeymapSchemaVersion,
            &StorageData::KeymapSchemaVersion(KEYMAP_STORAGE_SCHEMA_VERSION),
        )
        .await
        .map_err(|e| print_storage_error::<F>(e))?;

        Ok(())
    }

    #[cfg(feature = "host")]
    async fn reset_layout_only(
        &mut self,
        keymap: &[[[KeyAction; COL]; ROW]; NUM_LAYER],
        encoder_map: &Option<&[[EncoderAction; NUM_ENCODER]; NUM_LAYER]>,
        behavior: &config::BehaviorConfig,
    ) -> Result<(), SSError<F::Error>> {
        self.store_data(
            StorageKey::LayoutConfig,
            &StorageData::from(LayoutConfig {
                default_layer: 0,
                layout_option: 0,
            }),
        )
        .await?;
        self.store_data(StorageKey::BehaviorConfig, &StorageData::from(behavior))
            .await?;

        self.restore_factory_keymap(keymap, encoder_map).await
    }

    #[cfg(feature = "host")]
    async fn restore_factory_keymap(
        &mut self,
        keymap: &[[[KeyAction; COL]; ROW]; NUM_LAYER],
        encoder_map: &Option<&[[EncoderAction; NUM_ENCODER]; NUM_LAYER]>,
    ) -> Result<(), SSError<F::Error>> {
        // TODO: Generic reset for vial and other hosts
        for (layer, layer_data) in keymap.iter().enumerate() {
            for (row, row_data) in layer_data.iter().enumerate() {
                for (col, action) in row_data.iter().enumerate() {
                    let key = self.keymap_storage_key(layer as u8, row as u8, col as u8);
                    self.store_data(key, &StorageData::KeyAction(*action)).await?;
                }
            }
        }

        // TODO: Generic reset for vial and other hosts
        if let Some(encoder_map) = encoder_map {
            for (layer, layer_data) in encoder_map.iter().enumerate() {
                for (idx, action) in layer_data.iter().enumerate() {
                    let key = self.encoder_storage_key(idx as u8, layer as u8);
                    self.store_data(key, &StorageData::EncoderAction(*action)).await?;
                }
            }
        }

        // Write the marker last. If restoring the layout is interrupted,
        // startup retries it on the next boot.
        self.mark_keymap_schema_current().await?;

        Ok(())
    }

    #[cfg(feature = "host")]
    async fn mark_keymap_schema_current(&mut self) -> Result<(), SSError<F::Error>> {
        self.store_data(
            StorageKey::KeymapSchemaVersion,
            &StorageData::KeymapSchemaVersion(KEYMAP_STORAGE_SCHEMA_VERSION),
        )
        .await
    }

    #[cfg(feature = "host")]
    async fn keymap_schema_is_current(&mut self) -> bool {
        matches!(
            self.fetch_data(StorageKey::KeymapSchemaVersion).await,
            Some(StorageData::KeymapSchemaVersion(KEYMAP_STORAGE_SCHEMA_VERSION))
        )
    }

    async fn check_enable(&mut self) -> bool {
        let Some(StorageData::StorageConfig(mut config)) = self.fetch_data(StorageKey::StorageConfig).await else {
            return false;
        };
        if !config.enable {
            return false;
        }

        // A firmware build is not a storage schema version. BUILD_HASH changes
        // for every compiled artifact, so treating a mismatch as invalid
        // storage erased the Vial keymap and rewrote every key before runtime
        // tasks could start. Keep compatible data and only refresh the marker;
        // malformed records are still handled by the normal read error path.
        if config.build_hash != BUILD_HASH {
            config.build_hash = BUILD_HASH;
            if let Err(e) = self
                .store_data(StorageKey::StorageConfig, &StorageData::StorageConfig(config))
                .await
            {
                print_storage_error::<F>(e);
            }
        }

        true
    }

    /// Read all peripheral addresses from flash at startup, returning a `RefCell`
    /// suitable for sharing with `scan_peripherals` and `run_peripheral_manager`.
    ///
    /// Must be called before the storage task starts; once it is running it owns
    /// `&mut Storage` and no other reader can hold it.
    #[cfg(all(feature = "_ble", feature = "split"))]
    pub async fn read_peripheral_addresses<const PERI_NUM: usize>(
        &mut self,
    ) -> core::cell::RefCell<heapless::Vec<Option<[u8; 6]>, PERI_NUM>> {
        let mut peripheral_addresses: heapless::Vec<Option<[u8; 6]>, PERI_NUM> = heapless::Vec::new();
        for id in 0..PERI_NUM {
            let entry = match self.fetch_data(StorageKey::peer_address(id as u8)).await {
                Some(StorageData::PeerAddress(addr)) if addr.is_valid => Some(addr.address),
                _ => None,
            };
            peripheral_addresses.push(entry).unwrap();
        }
        core::cell::RefCell::new(peripheral_addresses)
    }
}

impl<F: AsyncNorFlash, const ROW: usize, const COL: usize, const NUM_LAYER: usize, const NUM_ENCODER: usize>
    crate::core_traits::Runnable for Storage<F, ROW, COL, NUM_LAYER, NUM_ENCODER>
{
    async fn run(&mut self) -> ! {
        #[cfg(feature = "host")]
        let mut pending_macro: Option<[u8; MACRO_SPACE_SIZE]> = None;
        #[cfg(feature = "host")]
        let mut macro_deadline = Instant::now();
        #[cfg(feature = "host")]
        let mut keymap_deadline: Option<Instant> = None;

        loop {
            #[cfg(feature = "host")]
            let info = match select5(
                FLASH_CHANNEL.receive(),
                MACRO_FLASH_SIGNAL.wait(),
                KEYMAP_FLASH_SIGNAL.wait(),
                Timer::at(if pending_macro.is_some() {
                    macro_deadline
                } else {
                    Instant::MAX
                }),
                Timer::at(keymap_deadline.unwrap_or(Instant::MAX)),
            )
            .await
            {
                Either5::First(info) => Some(info),
                Either5::Second(data) => {
                    pending_macro = Some(data);
                    macro_deadline = Instant::now() + MACRO_FLASH_SETTLE_TIMEOUT;
                    None
                }
                Either5::Third(()) => {
                    keymap_deadline = Some(Instant::now() + KEYMAP_FLASH_SETTLE_TIMEOUT);
                    None
                }
                Either5::Fourth(()) => {
                    let data = pending_macro.take().expect("pending macro snapshot");
                    let result = self
                        .store_data(StorageKey::MacroData, &StorageData::MacroData(data))
                        .await;
                    report_storage_result::<F>(result);
                    None
                }
                Either5::Fifth(()) => {
                    let info = take_pending_keymap_flash_write();
                    keymap_deadline = keymap_flash_writes_pending().then(Instant::now);
                    info
                }
            };

            #[cfg(not(feature = "host"))]
            let info = Some(FLASH_CHANNEL.receive().await);

            let Some(info) = info else {
                continue;
            };
            debug!("Flash operation: {:?}", info);

            let write_result: Result<(), SSError<F::Error>> = match info {
                #[cfg(feature = "universal_symbols")]
                FlashOperationMessage::UniversalSymbolsPlatform(mac) => {
                    self.store_data(
                        StorageKey::UniversalSymbolsPlatform,
                        &StorageData::UniversalSymbolsPlatform(mac),
                    )
                    .await
                }
                FlashOperationMessage::ReadLayoutOptions => {
                    let resp = match self.fetch_data(StorageKey::LayoutConfig).await {
                        Some(StorageData::LayoutConfig(config)) => Some(config.layout_option),
                        _ => None,
                    };
                    LAYOUT_OPTIONS_RESPONSE.signal(resp);
                    continue;
                }
                #[cfg(feature = "_ble")]
                FlashOperationMessage::ReadBleBondInfo(slot_num) => {
                    let resp = match self.fetch_data(StorageKey::bond_info(slot_num)).await {
                        Some(StorageData::BondInfo(info)) => Some(info),
                        _ => None,
                    };
                    BOND_INFO_RESPONSE.signal(resp);
                    continue;
                }
                #[cfg(all(feature = "_ble", feature = "split"))]
                FlashOperationMessage::ReadPeerAddress(peer_id) => {
                    let resp = match self.fetch_data(StorageKey::peer_address(peer_id)).await {
                        Some(StorageData::PeerAddress(addr)) => Some(addr),
                        _ => None,
                    };
                    PEER_ADDRESS_RESPONSE.signal(resp);
                    continue;
                }
                #[cfg(feature = "_ble")]
                FlashOperationMessage::ReadConnectionType => {
                    let resp = match self.fetch_data(StorageKey::ConnectionType).await {
                        Some(StorageData::ConnectionType(v)) => Some(v),
                        _ => None,
                    };
                    CONNECTION_TYPE_RESPONSE.signal(resp);
                    continue;
                }
                #[cfg(feature = "_ble")]
                FlashOperationMessage::ReadActiveBleProfile => {
                    let resp = match self.fetch_data(StorageKey::ActiveBleProfile).await {
                        Some(StorageData::ActiveBleProfile(v)) => Some(v),
                        _ => None,
                    };
                    ACTIVE_BLE_PROFILE_RESPONSE.signal(resp);
                    continue;
                }

                FlashOperationMessage::LayoutOptions(layout_option) => {
                    update_storage_field!(&mut self.flash, &mut self.buffer, LayoutConfig, layout_option)
                }
                FlashOperationMessage::Reset => {
                    #[cfg(feature = "host")]
                    {
                        pending_macro = None;
                        MACRO_FLASH_SIGNAL.reset();
                        keymap_deadline = None;
                        clear_pending_keymap_flash_writes();
                    }
                    self.flash.erase_all().await
                }
                FlashOperationMessage::ResetLayout => {
                    #[cfg(feature = "host")]
                    {
                        keymap_deadline = None;
                        clear_pending_keymap_flash_writes();
                    }
                    info!("Ignoring ResetLayout at runtime (handled at startup via clear_layout).");
                    Ok(())
                }
                FlashOperationMessage::DefaultLayer(default_layer) => {
                    update_storage_field!(&mut self.flash, &mut self.buffer, LayoutConfig, default_layer)
                }
                #[cfg(feature = "host")]
                FlashOperationMessage::KeymapKey {
                    layer,
                    row,
                    col,
                    action,
                } => {
                    let key = self.keymap_storage_key(layer, row, col);
                    self.store_data(key, &StorageData::KeyAction(action)).await
                }
                #[cfg(feature = "host")]
                FlashOperationMessage::Encoder { layer, idx, action } => {
                    let key = self.encoder_storage_key(idx, layer);
                    self.store_data(key, &StorageData::EncoderAction(action)).await
                }
                #[cfg(feature = "host")]
                FlashOperationMessage::Combo { idx, config } => {
                    self.store_data(StorageKey::combo(idx), &StorageData::Combo(config))
                        .await
                }
                #[cfg(feature = "host")]
                FlashOperationMessage::Fork { idx, fork } => {
                    self.store_data(StorageKey::fork(idx), &StorageData::Fork(fork)).await
                }
                #[cfg(feature = "host")]
                FlashOperationMessage::Morse { idx, morse } => {
                    self.store_data(StorageKey::morse(idx), &StorageData::Morse(morse))
                        .await
                }
                #[cfg(all(feature = "host", feature = "vial"))]
                FlashOperationMessage::DeviceSettings(data) => {
                    self.store_data(StorageKey::DeviceSettings, &StorageData::DeviceSettings(data))
                        .await
                }
                FlashOperationMessage::ConnectionType(ty) => {
                    self.store_data(StorageKey::ConnectionType, &StorageData::ConnectionType(ty))
                        .await
                }
                #[cfg(all(feature = "_ble", feature = "split"))]
                FlashOperationMessage::PeerAddress(peer) => {
                    self.store_data(StorageKey::peer_address(peer.peer_id), &StorageData::PeerAddress(peer))
                        .await
                }
                #[cfg(feature = "_ble")]
                FlashOperationMessage::ActiveBleProfile(profile) => {
                    self.store_data(StorageKey::ActiveBleProfile, &StorageData::ActiveBleProfile(profile))
                        .await
                }
                #[cfg(feature = "_ble")]
                FlashOperationMessage::ClearSlot(slot_num) => {
                    use trouble_host::prelude::SecurityLevel;
                    use trouble_host::{Address, BondInformation, Identity, LongTermKey};

                    info!("Clearing bond info slot_num: {}", slot_num);
                    // Remove item in `sequential-storage` is quite expensive, so just override the item with `removed = true`
                    let empty = ProfileInfo {
                        removed: true,
                        slot_num,
                        info: BondInformation::new(
                            Identity {
                                addr: Address::default(),
                                irk: None,
                            },
                            LongTermKey::from_le_bytes([0; 16]),
                            SecurityLevel::NoEncryption,
                            false,
                        ),
                        cccd_table: heapless::Vec::new(),
                    };
                    self.store_data(StorageKey::bond_info(slot_num), &StorageData::BondInfo(empty))
                        .await
                }
                #[cfg(feature = "_ble")]
                FlashOperationMessage::ProfileInfo(b) => {
                    debug!("Saving profile info: {:?}", b);
                    self.store_data(StorageKey::bond_info(b.slot_num), &StorageData::BondInfo(b))
                        .await
                }
                FlashOperationMessage::ComboTimeout(combo_timeout) => {
                    update_storage_field!(&mut self.flash, &mut self.buffer, BehaviorConfig, combo_timeout)
                }
                FlashOperationMessage::OneShotTimeout(one_shot_timeout) => {
                    update_storage_field!(&mut self.flash, &mut self.buffer, BehaviorConfig, one_shot_timeout)
                }
                FlashOperationMessage::TapInterval(tap_interval) => {
                    update_storage_field!(&mut self.flash, &mut self.buffer, BehaviorConfig, tap_interval)
                }
                FlashOperationMessage::TapCapslockInterval(tap_capslock_interval) => {
                    update_storage_field!(&mut self.flash, &mut self.buffer, BehaviorConfig, tap_capslock_interval)
                }
                FlashOperationMessage::PriorIdleTime(prior_idle_time) => {
                    update_storage_field!(&mut self.flash, &mut self.buffer, BehaviorConfig, prior_idle_time)
                }
                FlashOperationMessage::MorseDefaultProfile(morse_default_profile) => {
                    update_storage_field!(&mut self.flash, &mut self.buffer, BehaviorConfig, morse_default_profile)
                }
            };

            report_storage_result::<F>(write_result);
        }
    }
}

fn report_storage_result<F: AsyncNorFlash>(result: Result<(), SSError<F::Error>>) {
    match result {
        Ok(()) => FLASH_OPERATION_FINISHED.signal(true),
        Err(e) => {
            print_storage_error::<F>(e);
            FLASH_OPERATION_FINISHED.signal(false);
        }
    }
}

pub(crate) fn print_storage_error<F: AsyncNorFlash>(e: SSError<F::Error>) {
    match e {
        #[cfg(feature = "defmt")]
        SSError::Storage { value: e } => error!("Flash error: {:?}", defmt::Debug2Format(&e)),
        #[cfg(not(feature = "defmt"))]
        SSError::Storage { value: _e } => error!("Flash error"),
        SSError::FullStorage => error!("Storage is full"),
        SSError::Corrupted {} => error!("Storage is corrupted"),
        SSError::BufferTooBig => error!("Buffer too big"),
        SSError::BufferTooSmall(x) => error!("Buffer too small, needs {} bytes", x),
        SSError::SerializationError(e) => error!("Map value error: {}", e),
        SSError::ItemTooBig => error!("Item too big"),
        _ => error!("Unknown storage error"),
    }
}

const fn get_buffer_size() -> usize {
    #[cfg(feature = "host")]
    {
        // The buffer size needed = size_of(StorageData) = MACRO_SPACE_SIZE + 8(generally)
        // According to doc of `sequential-storage`, for some flashes it should be aligned in 32 bytes
        // To make sure the buffer works, do this alignment always
        let buffer_size = if crate::MACRO_SPACE_SIZE < 248 {
            256
        } else {
            crate::MACRO_SPACE_SIZE + 8
        };

        // Efficiently round up to the nearest multiple of 32 using bit manipulation.
        (buffer_size + 31) & !31
    }

    #[cfg(not(feature = "host"))]
    256
}

#[cfg(test)]
mod tests {
    use sequential_storage::cache::NoCache;
    use sequential_storage::map::{MapConfig, MapStorage};

    use super::*;
    use crate::config::{BehaviorConfig as RuntimeBehaviorConfig, StorageConfig as RuntimeStorageConfig};
    use crate::test_support::test_block_on as block_on;
    #[cfg(feature = "universal_symbols_iso")]
    use crate::universal_symbols::tests::ISO_LAYOUT_OPTION_MASK;

    #[derive(Debug, Clone, Copy)]
    struct TestFlashError;

    impl embedded_storage_async::nor_flash::NorFlashError for TestFlashError {
        fn kind(&self) -> embedded_storage_async::nor_flash::NorFlashErrorKind {
            embedded_storage_async::nor_flash::NorFlashErrorKind::Other
        }
    }

    struct TestFlash<const SIZE: usize, const ERASE_SIZE: usize, const WRITE_SIZE: usize> {
        bytes: [u8; SIZE],
    }

    impl<const SIZE: usize, const ERASE_SIZE: usize, const WRITE_SIZE: usize> TestFlash<SIZE, ERASE_SIZE, WRITE_SIZE> {
        fn new() -> Self {
            Self { bytes: [0xFF; SIZE] }
        }
    }

    impl<const SIZE: usize, const ERASE_SIZE: usize, const WRITE_SIZE: usize> embedded_storage::nor_flash::ErrorType
        for TestFlash<SIZE, ERASE_SIZE, WRITE_SIZE>
    {
        type Error = TestFlashError;
    }

    impl<const SIZE: usize, const ERASE_SIZE: usize, const WRITE_SIZE: usize> embedded_storage::nor_flash::ReadNorFlash
        for TestFlash<SIZE, ERASE_SIZE, WRITE_SIZE>
    {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            let end = start + bytes.len();
            bytes.copy_from_slice(&self.bytes[start..end]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            SIZE
        }
    }

    impl<const SIZE: usize, const ERASE_SIZE: usize, const WRITE_SIZE: usize> embedded_storage::nor_flash::NorFlash
        for TestFlash<SIZE, ERASE_SIZE, WRITE_SIZE>
    {
        const WRITE_SIZE: usize = WRITE_SIZE;
        const ERASE_SIZE: usize = ERASE_SIZE;

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            self.bytes[from as usize..to as usize].fill(0xFF);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            let end = start + bytes.len();
            for (dst, src) in self.bytes[start..end].iter_mut().zip(bytes.iter()) {
                *dst &= *src;
            }
            Ok(())
        }
    }

    impl<const SIZE: usize, const ERASE_SIZE: usize, const WRITE_SIZE: usize>
        embedded_storage_async::nor_flash::ReadNorFlash for TestFlash<SIZE, ERASE_SIZE, WRITE_SIZE>
    {
        const READ_SIZE: usize = 1;

        async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            embedded_storage::nor_flash::ReadNorFlash::read(self, offset, bytes)
        }

        fn capacity(&self) -> usize {
            SIZE
        }
    }

    impl<const SIZE: usize, const ERASE_SIZE: usize, const WRITE_SIZE: usize>
        embedded_storage_async::nor_flash::NorFlash for TestFlash<SIZE, ERASE_SIZE, WRITE_SIZE>
    {
        const WRITE_SIZE: usize = WRITE_SIZE;
        const ERASE_SIZE: usize = ERASE_SIZE;

        async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            embedded_storage::nor_flash::NorFlash::erase(self, from, to)
        }

        async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            embedded_storage::nor_flash::NorFlash::write(self, offset, bytes)
        }
    }

    #[cfg(all(feature = "host", feature = "universal_symbols_iso"))]
    #[test]
    fn universal_host_layout_option_write_updates_resolver_before_flash() {
        block_on(async {
            use crate::universal_symbols::{Command, State, USER_SYMBOL_START, USER_TOGGLE_MACOS};
            let mut data = crate::keymap::KeymapData::new([[[KeyAction::No; 1]; 1]; 1]);
            let mut behavior = RuntimeBehaviorConfig::default();
            let positional = crate::config::PositionalConfig::<1, 1>::default();
            let keymap = crate::keymap::KeyMap::new(&mut data, &mut behavior, &positional).await;
            let ctx = crate::host::context::KeyboardContext::new(&keymap);
            FLASH_CHANNEL.clear();
            let mut s = State::default();
            s.handle(USER_TOGGLE_MACOS, None);
            let legacy_bits = 7 & !ISO_LAYOUT_OPTION_MASK;
            let iso_options = legacy_bits | ISO_LAYOUT_OPTION_MASK;
            ctx.set_layout_options(iso_options).await;
            let Some(Command::Type(grave)) = s.handle(USER_SYMBOL_START + 7, None) else {
                panic!()
            };
            assert_eq!(grave.stroke.keycode, rmk_types::keycode::HidKeyCode::NonusBackslash);
            assert!(matches!(
                FLASH_CHANNEL.try_receive(),
                Ok(FlashOperationMessage::LayoutOptions(options)) if options == iso_options
            ));
            ctx.set_layout_options(legacy_bits).await;
            let Some(Command::Type(grave)) = s.handle(USER_SYMBOL_START + 7, None) else {
                panic!()
            };
            assert_eq!(grave.stroke.keycode, rmk_types::keycode::HidKeyCode::Grave);
            assert!(matches!(
                FLASH_CHANNEL.try_receive(),
                Ok(FlashOperationMessage::LayoutOptions(options)) if options == legacy_bits
            ));
        });
    }

    #[cfg(all(feature = "host", feature = "universal_symbols_iso"))]
    #[test]
    fn universal_flash_roundtrip_restores_platform_geometry_without_keymap_reset() {
        block_on(async {
            use embassy_futures::select::{Either, select};

            use crate::core_traits::Runnable;
            use crate::universal_symbols::{Command, Platform, State, USER_SYMBOL_START};
            type Flash = TestFlash<16_384, 4_096, 1>;
            let keys = [[[KeyAction::No; 1]; 1]; 1];
            let encoders: Option<&mut [[EncoderAction; 0]; 1]> = None;
            let config = RuntimeStorageConfig::default();
            let behavior = RuntimeBehaviorConfig::default();
            let mut storage =
                Storage::<Flash, 1, 1, 1, 0>::new(Flash::new(), &keys, &encoders, &config, &behavior).await;
            assert_eq!(State::default().platform(), Platform::Pc);
            let legacy_bits = 7 & !ISO_LAYOUT_OPTION_MASK;
            // Sentinel user action and default layer must survive both boots.
            storage
                .store_data(
                    StorageKey::keymap(0, 0, 0),
                    &StorageData::KeyAction(KeyAction::Transparent),
                )
                .await
                .unwrap();
            storage
                .store_data(
                    StorageKey::LayoutConfig,
                    &StorageData::LayoutConfig(LayoutConfig {
                        default_layer: 7,
                        layout_option: legacy_bits,
                    }),
                )
                .await
                .unwrap();
            for (mac, options) in [(true, legacy_bits | ISO_LAYOUT_OPTION_MASK), (false, legacy_bits)] {
                FLASH_CHANNEL.clear();
                let scenario = async {
                    FLASH_CHANNEL
                        .send(FlashOperationMessage::UniversalSymbolsPlatform(mac))
                        .await;
                    FLASH_CHANNEL.send(FlashOperationMessage::LayoutOptions(options)).await;
                    // Read response is a FIFO barrier after both real writes.
                    assert_eq!(read_layout_options().await, Some(options));
                };
                assert!(matches!(select(storage.run(), scenario).await, Either::Second(())));
                assert!(
                    matches!(storage.fetch_data(StorageKey::UniversalSymbolsPlatform).await, Some(StorageData::UniversalSymbolsPlatform(v)) if v == mac)
                );
                let (flash, _) = storage.flash.destroy();
                // Simulate a fresh runtime, but keep the actual flash bytes.
                crate::universal_symbols::restore_platform(!mac);
                crate::universal_symbols::set_layout_options(options ^ ISO_LAYOUT_OPTION_MASK);
                storage = Storage::<Flash, 1, 1, 1, 0>::new(flash, &keys, &encoders, &config, &behavior).await;
                assert_eq!(
                    State::default().platform(),
                    if mac { Platform::Mac } else { Platform::Pc }
                );
                let Some(Command::Type(grave)) = State::default().handle(USER_SYMBOL_START + 7, None) else {
                    panic!()
                };
                assert_eq!(
                    grave.stroke.keycode,
                    if mac {
                        rmk_types::keycode::HidKeyCode::NonusBackslash
                    } else {
                        rmk_types::keycode::HidKeyCode::Grave
                    }
                );
                // Check restored geometry independently of the restored platform:
                // PC Grave alone cannot distinguish ANSI from ISO.
                let mut mac_state = State::default();
                if mac_state.platform() == Platform::Pc {
                    mac_state.handle(crate::universal_symbols::USER_TOGGLE_MACOS, None);
                }
                let Some(Command::Type(mac_grave)) = mac_state.handle(USER_SYMBOL_START + 7, None) else {
                    panic!()
                };
                assert_eq!(
                    mac_grave.stroke.keycode,
                    if options & ISO_LAYOUT_OPTION_MASK != 0 {
                        rmk_types::keycode::HidKeyCode::NonusBackslash
                    } else {
                        rmk_types::keycode::HidKeyCode::Grave
                    }
                );
                assert!(matches!(
                    storage.fetch_data(StorageKey::keymap(0, 0, 0)).await,
                    Some(StorageData::KeyAction(KeyAction::Transparent))
                ));
                assert!(
                    matches!(storage.fetch_data(StorageKey::LayoutConfig).await, Some(StorageData::LayoutConfig(LayoutConfig { default_layer: 7, layout_option })) if layout_option == options)
                );
            }
        });
    }

    #[cfg(all(feature = "host", feature = "universal_symbols_iso"))]
    #[test]
    fn universal_legacy_absent_record_defaults_pc_ansi_without_writes() {
        block_on(async {
            let legacy_bits = 7 & !ISO_LAYOUT_OPTION_MASK;
            type Flash = TestFlash<16_384, 4_096, 1>;
            let keys = [[[KeyAction::No; 1]; 1]; 1];
            let encoders: Option<&mut [[EncoderAction; 0]; 1]> = None;
            let config = RuntimeStorageConfig::default();
            let behavior = RuntimeBehaviorConfig::default();
            let mut storage =
                Storage::<Flash, 1, 1, 1, 0>::new(Flash::new(), &keys, &encoders, &config, &behavior).await;
            storage
                .store_data(
                    StorageKey::LayoutConfig,
                    &StorageData::LayoutConfig(LayoutConfig {
                        default_layer: 0,
                        layout_option: legacy_bits,
                    }),
                )
                .await
                .unwrap();
            let (flash, _) = storage.flash.destroy();
            let before = flash.bytes;
            crate::universal_symbols::restore_platform(true);
            crate::universal_symbols::set_layout_options(ISO_LAYOUT_OPTION_MASK);
            let mut storage = Storage::<Flash, 1, 1, 1, 0>::new(flash, &keys, &encoders, &config, &behavior).await;
            assert_eq!(
                crate::universal_symbols::State::default().platform(),
                crate::universal_symbols::Platform::Pc
            );
            assert!(storage.fetch_data(StorageKey::UniversalSymbolsPlatform).await.is_none());
            let mut s = crate::universal_symbols::State::default();
            s.handle(crate::universal_symbols::USER_TOGGLE_MACOS, None);
            let Some(crate::universal_symbols::Command::Type(grave)) =
                s.handle(crate::universal_symbols::USER_SYMBOL_START + 7, None)
            else {
                panic!()
            };
            assert_eq!(grave.stroke.keycode, rmk_types::keycode::HidKeyCode::Grave);
            let (flash, _) = storage.flash.destroy();
            assert_eq!(
                before, flash.bytes,
                "old flash must not be rewritten just to add defaults"
            );
        });
    }

    #[cfg(all(feature = "host", feature = "universal_symbols"))]
    #[test]
    fn universal_storage_namespace_is_append_only() {
        let mut buf = [0u8; 32];
        let last_key = postcard::to_slice(&StorageKey::EncoderTailV3 { layer: 0, idx: 0 }, &mut buf).unwrap()[0];
        let new_key: StorageKey = postcard::from_bytes(&[last_key + 1]).expect("new key must be appended");
        assert_eq!(std::format!("{new_key:?}"), "UniversalSymbolsPlatform");
        let last_data = postcard::to_slice(&StorageData::KeymapSchemaVersion(0), &mut buf).unwrap()[0];
        let new_data: StorageData = postcard::from_bytes(&[last_data + 1, 1]).expect("new data must be appended");
        assert_eq!(std::format!("{new_data:?}"), "UniversalSymbolsPlatform(true)");
    }

    #[test]
    fn storage_key_round_trip() {
        let cases = [
            StorageKey::StorageConfig,
            StorageKey::LayoutConfig,
            StorageKey::BehaviorConfig,
            StorageKey::ConnectionType,
            #[cfg(feature = "host")]
            StorageKey::MacroData,
            #[cfg(feature = "host")]
            StorageKey::Keymap {
                layer: 2,
                row: 3,
                col: 4,
            },
            #[cfg(feature = "host")]
            StorageKey::Encoder { layer: 1, idx: 5 },
            #[cfg(feature = "host")]
            StorageKey::Combo(6),
            #[cfg(feature = "host")]
            StorageKey::Fork(7),
            #[cfg(feature = "host")]
            StorageKey::Morse(8),
            #[cfg(all(feature = "_ble", feature = "split"))]
            StorageKey::PeerAddress(0),
            #[cfg(feature = "_ble")]
            StorageKey::ActiveBleProfile,
            #[cfg(feature = "_ble")]
            StorageKey::BondInfo(0),
            #[cfg(feature = "host")]
            StorageKey::KeymapSchemaVersion,
            #[cfg(feature = "host")]
            StorageKey::KeymapV2 {
                layer: 9,
                row: 10,
                col: 11,
            },
            #[cfg(feature = "host")]
            StorageKey::EncoderV2 { layer: 12, idx: 13 },
            #[cfg(feature = "host")]
            StorageKey::KeymapTailV3 {
                layer: 14,
                row: 15,
                col: 16,
            },
            #[cfg(feature = "host")]
            StorageKey::EncoderTailV3 { layer: 17, idx: 18 },
        ];

        let mut buffer = [0u8; 64];
        for key in cases {
            let size = <StorageKey as Key>::serialize_into(&key, &mut buffer).unwrap();
            let (decoded, used) = <StorageKey as Key>::deserialize_from(&buffer[..size]).unwrap();
            assert_eq!(decoded, key);
            assert_eq!(used, size);
        }
    }

    #[cfg(feature = "host")]
    #[test]
    fn pending_keymap_writes_keep_distinct_positions_and_latest_value() {
        let mut pending = PendingKeymapWrites::<4>::new();

        assert!(pending.insert(1, KeyAction::No));
        assert!(pending.insert(3, KeyAction::Transparent));
        assert!(pending.insert(1, KeyAction::Transparent));

        assert_eq!(pending.take_next(), Some((1, KeyAction::Transparent)));
        assert_eq!(pending.take_next(), Some((3, KeyAction::Transparent)));
        assert!(pending.take_next().is_none());
        assert!(pending.is_empty());

        assert!(pending.insert(0, KeyAction::No));
        pending.clear();
        assert!(pending.is_empty());
    }

    #[cfg(feature = "host")]
    #[test]
    fn pending_keymap_write_rejects_only_out_of_range_index() {
        let mut pending = PendingKeymapWrites::<2>::new();

        assert!(pending.insert(0, KeyAction::No));
        assert!(pending.insert(1, KeyAction::No));
        assert!(!pending.insert(2, KeyAction::No));
    }

    #[test]
    fn build_hash_mismatch_preserves_storage_and_updates_marker() {
        block_on(async {
            type Flash = TestFlash<16_384, 4_096, 1>;

            let storage_range = (16_384 - 2 * 4_096) as u32..16_384u32;
            let mut map =
                MapStorage::<StorageKey, _, _>::new(Flash::new(), MapConfig::new(storage_range), NoCache::new());
            let mut buffer = [0u8; 256];

            map.store_item(
                &mut buffer,
                &StorageKey::StorageConfig,
                &StorageData::StorageConfig(LocalStorageConfig {
                    enable: true,
                    build_hash: BUILD_HASH.wrapping_sub(1),
                }),
            )
            .await
            .unwrap();
            map.store_item(
                &mut buffer,
                &StorageKey::LayoutConfig,
                &StorageData::LayoutConfig(LayoutConfig {
                    default_layer: 7,
                    layout_option: 42,
                }),
            )
            .await
            .unwrap();
            #[cfg(feature = "host")]
            map.store_item(
                &mut buffer,
                &StorageKey::KeymapSchemaVersion,
                &StorageData::KeymapSchemaVersion(KEYMAP_STORAGE_SCHEMA_VERSION),
            )
            .await
            .unwrap();

            let (flash, _) = map.destroy();
            #[cfg(feature = "host")]
            let keymap = [[[KeyAction::No; 1]; 1]; 1];
            #[cfg(feature = "host")]
            let encoder_map: Option<&mut [[EncoderAction; 0]; 1]> = None;

            let mut storage = Storage::<Flash, 1, 1, 1, 0>::new(
                flash,
                #[cfg(feature = "host")]
                &keymap,
                #[cfg(feature = "host")]
                &encoder_map,
                &RuntimeStorageConfig::default(),
                &RuntimeBehaviorConfig::default(),
            )
            .await;

            let stored_layout = storage.fetch_data(StorageKey::LayoutConfig).await.unwrap();
            let stored_config = storage.fetch_data(StorageKey::StorageConfig).await.unwrap();

            assert!(matches!(
                stored_layout,
                StorageData::LayoutConfig(LayoutConfig {
                    default_layer: 7,
                    layout_option: 42,
                })
            ));
            assert!(matches!(
                stored_config,
                StorageData::StorageConfig(LocalStorageConfig {
                    enable: true,
                    build_hash: BUILD_HASH,
                })
            ));
        });
    }

    #[cfg(feature = "host")]
    #[test]
    fn legacy_keymap_schema_activates_factory_without_rewriting_layout() {
        block_on(async {
            use rmk_types::action::{Action, KeyAction};
            use rmk_types::keycode::HidKeyCode;
            use rmk_types::modifier::ModifierCombination;

            type Flash = TestFlash<16_384, 4_096, 1>;

            let storage_range = (16_384 - 2 * 4_096) as u32..16_384u32;
            let mut map =
                MapStorage::<StorageKey, _, _>::new(Flash::new(), MapConfig::new(storage_range), NoCache::new());
            let mut buffer = [0u8; 256];

            map.store_item(
                &mut buffer,
                &StorageKey::StorageConfig,
                &StorageData::StorageConfig(LocalStorageConfig {
                    enable: true,
                    build_hash: BUILD_HASH,
                }),
            )
            .await
            .unwrap();
            map.store_item(
                &mut buffer,
                &StorageKey::LayoutConfig,
                &StorageData::LayoutConfig(LayoutConfig {
                    default_layer: 7,
                    layout_option: 42,
                }),
            )
            .await
            .unwrap();

            // This is how legacy `SHIFTED(Kc2)` data appears after the old
            // KeyCode-wrapped payload is decoded as the current compact type.
            let corrupted = KeyAction::Single(Action::KeyWithModifier(
                HidKeyCode::No,
                ModifierCombination::from_bits(0x1F),
            ));
            map.store_item(
                &mut buffer,
                &StorageKey::Keymap {
                    layer: 0,
                    row: 0,
                    col: 0,
                },
                &StorageData::KeyAction(corrupted),
            )
            .await
            .unwrap();

            let (flash, _) = map.destroy();
            let factory = KeyAction::Single(Action::KeyWithModifier(HidKeyCode::Kc2, ModifierCombination::LSHIFT));
            let keymap = [[[factory; 1]; 1]; 1];
            let encoder_map: Option<&mut [[EncoderAction; 0]; 1]> = None;

            let mut storage = Storage::<Flash, 1, 1, 1, 0>::new(
                flash,
                &keymap,
                &encoder_map,
                &RuntimeStorageConfig::default(),
                &RuntimeBehaviorConfig::default(),
            )
            .await;

            assert!(storage.fetch_data(StorageKey::keymap(0, 0, 0)).await.is_none());

            let mut runtime_data = crate::keymap::KeymapData::new(keymap);
            let mut runtime_behavior = RuntimeBehaviorConfig::default();
            storage
                .read_runtime_state(&mut runtime_data, &mut runtime_behavior)
                .await
                .unwrap();
            assert_eq!(runtime_data.keymap[0][0][0], factory);
            assert!(matches!(
                storage.fetch_data(StorageKey::LayoutConfig).await,
                Some(StorageData::LayoutConfig(LayoutConfig {
                    default_layer: 7,
                    layout_option: 42,
                }))
            ));
            assert!(matches!(
                storage.fetch_data(StorageKey::KeymapSchemaVersion).await,
                Some(StorageData::KeymapSchemaVersion(KEYMAP_STORAGE_SCHEMA_VERSION))
            ));
        });
    }

    #[cfg(feature = "host")]
    #[test]
    fn rc36_schema_marker_does_not_reactivate_legacy_key_records() {
        block_on(async {
            use rmk_types::action::{Action, KeyAction};
            use rmk_types::keycode::HidKeyCode;
            use rmk_types::modifier::ModifierCombination;

            type Flash = TestFlash<16_384, 4_096, 1>;

            let storage_range = (16_384 - 2 * 4_096) as u32..16_384u32;
            let mut map =
                MapStorage::<StorageKey, _, _>::new(Flash::new(), MapConfig::new(storage_range), NoCache::new());
            let mut buffer = [0u8; 256];

            map.store_item(
                &mut buffer,
                &StorageKey::StorageConfig,
                &StorageData::StorageConfig(LocalStorageConfig {
                    enable: true,
                    build_hash: BUILD_HASH,
                }),
            )
            .await
            .unwrap();
            map.store_item(
                &mut buffer,
                &StorageKey::KeymapSchemaVersion,
                &StorageData::KeymapSchemaVersion(1),
            )
            .await
            .unwrap();

            let legacy = KeyAction::Single(Action::Key(rmk_types::keycode::KeyCode::Hid(HidKeyCode::Kc0)));
            map.store_item(
                &mut buffer,
                &StorageKey::Keymap {
                    layer: 0,
                    row: 0,
                    col: 0,
                },
                &StorageData::KeyAction(legacy),
            )
            .await
            .unwrap();

            let (flash, _) = map.destroy();
            let factory = KeyAction::Single(Action::KeyWithModifier(HidKeyCode::Kc2, ModifierCombination::LSHIFT));
            let keymap = [[[factory; 1]; 1]; 1];
            let encoder_map: Option<&mut [[EncoderAction; 0]; 1]> = None;
            let mut storage = Storage::<Flash, 1, 1, 1, 0>::new(
                flash,
                &keymap,
                &encoder_map,
                &RuntimeStorageConfig::default(),
                &RuntimeBehaviorConfig::default(),
            )
            .await;

            let mut runtime_data = crate::keymap::KeymapData::new(keymap);
            let mut runtime_behavior = RuntimeBehaviorConfig::default();
            storage
                .read_runtime_state(&mut runtime_data, &mut runtime_behavior)
                .await
                .unwrap();

            assert_eq!(runtime_data.keymap[0][0][0], factory);
            assert!(matches!(
                storage.fetch_data(StorageKey::KeymapSchemaVersion).await,
                Some(StorageData::KeymapSchemaVersion(KEYMAP_STORAGE_SCHEMA_VERSION))
            ));

            storage
                .store_data(StorageKey::keymap(0, 0, 0), &StorageData::KeyAction(legacy))
                .await
                .unwrap();
            let mut updated_runtime_data = crate::keymap::KeymapData::new(keymap);
            storage
                .read_runtime_state(&mut updated_runtime_data, &mut runtime_behavior)
                .await
                .unwrap();
            assert_eq!(updated_runtime_data.keymap[0][0][0], legacy);
        });
    }

    #[cfg(feature = "host")]
    #[test]
    fn tail_namespace_preserves_lower_layers_and_replaces_legacy_tail() {
        block_on(async {
            use rmk_types::action::{Action, EncoderAction, KeyAction};
            use rmk_types::keycode::{HidKeyCode, KeyCode};

            type Flash = TestFlash<16_384, 4_096, 1>;

            let key = |keycode| KeyAction::Single(Action::Key(KeyCode::Hid(keycode)));
            let lower_key = key(HidKeyCode::A);
            let legacy_tail_key = key(HidKeyCode::B);
            let new_tail_key = key(HidKeyCode::C);
            let lower_encoder = EncoderAction::new(key(HidKeyCode::D), key(HidKeyCode::E));
            let legacy_tail_encoder = EncoderAction::new(key(HidKeyCode::F), key(HidKeyCode::G));
            let new_tail_encoder = EncoderAction::new(key(HidKeyCode::H), key(HidKeyCode::I));

            let storage_range = (16_384 - 2 * 4_096) as u32..16_384u32;
            let mut map =
                MapStorage::<StorageKey, _, _>::new(Flash::new(), MapConfig::new(storage_range), NoCache::new());
            let mut buffer = [0u8; 256];

            map.store_item(
                &mut buffer,
                &StorageKey::StorageConfig,
                &StorageData::StorageConfig(LocalStorageConfig {
                    enable: true,
                    build_hash: BUILD_HASH,
                }),
            )
            .await
            .unwrap();
            map.store_item(
                &mut buffer,
                &StorageKey::KeymapSchemaVersion,
                &StorageData::KeymapSchemaVersion(KEYMAP_STORAGE_SCHEMA_VERSION),
            )
            .await
            .unwrap();
            map.store_item(
                &mut buffer,
                &StorageKey::KeymapV2 {
                    layer: 4,
                    row: 0,
                    col: 0,
                },
                &StorageData::KeyAction(lower_key),
            )
            .await
            .unwrap();
            map.store_item(
                &mut buffer,
                &StorageKey::KeymapV2 {
                    layer: 5,
                    row: 0,
                    col: 0,
                },
                &StorageData::KeyAction(legacy_tail_key),
            )
            .await
            .unwrap();
            map.store_item(
                &mut buffer,
                &StorageKey::EncoderV2 { layer: 4, idx: 0 },
                &StorageData::EncoderAction(lower_encoder),
            )
            .await
            .unwrap();
            map.store_item(
                &mut buffer,
                &StorageKey::EncoderV2 { layer: 5, idx: 0 },
                &StorageData::EncoderAction(legacy_tail_encoder),
            )
            .await
            .unwrap();

            let (flash, _) = map.destroy();
            let factory_keymap = [[[KeyAction::No; 1]; 1]; 6];
            let factory_encoder_map = [[EncoderAction::default(); 1]; 6];
            let encoder_map: Option<&mut [[EncoderAction; 1]; 6]> = None;
            let storage_config = RuntimeStorageConfig {
                no_action_layer_start: Some(5),
                ..RuntimeStorageConfig::default()
            };
            let mut storage = Storage::<Flash, 1, 1, 6, 1>::new(
                flash,
                &factory_keymap,
                &encoder_map,
                &storage_config,
                &RuntimeBehaviorConfig::default(),
            )
            .await;

            let mut runtime_data = crate::keymap::KeymapData::new_with_encoder(factory_keymap, factory_encoder_map);
            let mut runtime_behavior = RuntimeBehaviorConfig::default();
            storage
                .read_runtime_state(&mut runtime_data, &mut runtime_behavior)
                .await
                .unwrap();

            assert_eq!(runtime_data.keymap[4][0][0], lower_key);
            assert_eq!(runtime_data.keymap[5][0][0], KeyAction::No);
            assert_eq!(runtime_data.encoder_map[4][0], lower_encoder);
            assert_eq!(runtime_data.encoder_map[5][0], EncoderAction::default());

            let tail_key = storage.keymap_storage_key(5, 0, 0);
            let tail_encoder_key = storage.encoder_storage_key(0, 5);
            assert!(matches!(tail_key, StorageKey::KeymapTailV3 { .. }));
            assert!(matches!(tail_encoder_key, StorageKey::EncoderTailV3 { .. }));
            storage
                .store_data(tail_key, &StorageData::KeyAction(new_tail_key))
                .await
                .unwrap();
            storage
                .store_data(tail_encoder_key, &StorageData::EncoderAction(new_tail_encoder))
                .await
                .unwrap();

            let mut updated_runtime_data =
                crate::keymap::KeymapData::new_with_encoder(factory_keymap, factory_encoder_map);
            storage
                .read_runtime_state(&mut updated_runtime_data, &mut runtime_behavior)
                .await
                .unwrap();

            assert_eq!(updated_runtime_data.keymap[4][0][0], lower_key);
            assert_eq!(updated_runtime_data.keymap[5][0][0], new_tail_key);
            assert_eq!(updated_runtime_data.encoder_map[4][0], lower_encoder);
            assert_eq!(updated_runtime_data.encoder_map[5][0], new_tail_encoder);
        });
    }
}
