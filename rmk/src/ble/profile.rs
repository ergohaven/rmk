//! Manage BLE profiles and bonding information

#[cfg(feature = "_ble")]
use bt_hci::{cmd::le::LeSetPhy, controller::ControllerCmdAsync};
use embassy_futures::select::{Either, select, select_array};
use embassy_sync::signal::Signal;
use embassy_sync::watch::Watch;
use trouble_host::prelude::*;
use trouble_host::{BondInformation, LongTermKey};

use super::ble_server::CCCD_TABLE_SIZE;
use crate::NUM_BLE_PROFILE;
use crate::channel::BLE_PROFILE_CHANNEL;
use crate::state::{current_profile, set_ble_bonded, set_ble_profile};
#[cfg(feature = "storage")]
use crate::storage::{StorageItem, StorageKey, StorageValue, read, store};

/// The dedicated dongle bond slot: the slot number after the normal profiles,
/// so pairing a dongle never touches a host bond. Selected only via the
/// `SwitchToDongle` key, never by profile cycling.
#[cfg(feature = "dongle")]
pub(crate) const DONGLE_PROFILE: u8 = NUM_BLE_PROFILE as u8;

/// Bond slots kept by the profile manager: the host profiles, plus the
/// dedicated dongle slot on `dongle` builds.
pub(crate) const BOND_SLOTS: usize = NUM_BLE_PROFILE + cfg!(feature = "dongle") as usize;

/// A fixed profile address that preserves the base address's random-address type bits.
pub(crate) fn profile_address(base: [u8; 6], profile: u8) -> [u8; 6] {
    let mut addr = base;
    addr[3] = addr[3].wrapping_add(profile);
    addr
}

/// BLE profile info
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ProfileInfo {
    pub(crate) slot_num: u8,
    pub(crate) removed: bool,
    pub(crate) info: BondInformation,
    /// Raw bytes of the trouble-host `ClientAttTable` for this peer.
    /// Reconstructed via `ClientAttTableView::try_from_raw` when applied to the stack.
    pub(crate) cccd_table: heapless::Vec<u8, CCCD_TABLE_SIZE>,
}

/// Returns the maximum number of bytes required to encode T.
pub const fn varint_max<T: Sized>() -> usize {
    const BITS_PER_BYTE: usize = 8;
    const BITS_PER_VARINT_BYTE: usize = 7;

    // How many data bits do we need for this type?
    let bits = core::mem::size_of::<T>() * BITS_PER_BYTE;

    // We add (BITS_PER_VARINT_BYTE - 1), to ensure any integer divisions
    // with a remainder will always add exactly one full byte, but
    // an evenly divided number of bits will be the same
    let roundup_bits = bits + (BITS_PER_VARINT_BYTE - 1);

    // Apply division, using normal "round down" integer division
    roundup_bits / BITS_PER_VARINT_BYTE
}

// Manual MaxSize implementation
impl postcard::experimental::max_size::MaxSize for ProfileInfo {
    const POSTCARD_MAX_SIZE: usize = varint_max::<Self>();
}

impl Default for ProfileInfo {
    fn default() -> Self {
        Self {
            slot_num: 0,
            removed: false,
            info: BondInformation::new(
                Identity {
                    addr: Address::default(),
                    irk: None,
                },
                LongTermKey(0),
                SecurityLevel::NoEncryption,
                false,
            ),
            cccd_table: heapless::Vec::new(),
        }
    }
}

/// BLE profile switch action
#[derive(Debug)]
pub(crate) enum BleProfileAction {
    Switch(u8),
    Previous,
    Next,
    ClearBond,
    /// Clear the bond for an explicit slot, regardless of which slot is
    /// currently active. Rynk's `Cmd::ClearBleProfile` issues this so a host
    /// tool can wipe any bond without first switching to it.
    ClearSlot(u8),
}

/// Manage BLE profiles and bonding information
///
/// ProfileManager is responsible for:
/// 1. Managing multiple BLE profiles, allowing users to switch between multiple devices
/// 2. Storing and loading bonding information for each profile
/// 3. Keeping stored security identities in the BLE stack, separate from profile admission
/// 4. Handling profile switch, clear, and save operations
///
/// `SLOTS` sizes the cache to the role: [`BOND_SLOTS`] for a keyboard, a single
/// slot for a dongle.
#[cfg(feature = "_ble")]
pub(crate) struct ProfileManager<
    'b,
    's,
    C: Controller + ControllerCmdAsync<LeSetPhy>,
    P: PacketPool,
    const SLOTS: usize,
> where
    's: 'b,
{
    profiles: [Watch<crate::RawMutex, ProfileInfo, 1>; SLOTS],
    updated_active_profile: Signal<crate::RawMutex, ()>,
    /// BLE stack
    stack: &'b Stack<'s, C, P>,
}

#[cfg(feature = "_ble")]
impl<'b, 's, C: Controller + ControllerCmdAsync<LeSetPhy>, P: PacketPool, const SLOTS: usize>
    ProfileManager<'b, 's, C, P, SLOTS>
where
    's: 'b,
{
    /// Create a new profile manager
    pub(crate) fn new(stack: &'b Stack<'s, C, P>) -> Self {
        Self {
            profiles: [const { Watch::new() }; SLOTS],
            updated_active_profile: Signal::new(),
            stack,
        }
    }

    /// Load stored bonding information
    #[cfg(feature = "storage")]
    pub(crate) async fn load_bonded_devices(&mut self) {
        for (slot, state) in self.profiles.iter_mut().enumerate() {
            *state = match read(StorageKey::BondInfo(slot as u8)).await {
                Ok(Some(StorageValue::BondInfo(info))) if !info.removed => Watch::new_with(info),
                _ => Watch::new(),
            };
        }

        let profile =
            if let Ok(Some(StorageValue::ActiveBleProfile(profile))) = read(StorageKey::ActiveBleProfile).await {
                debug!("Loaded active profile: {}", profile);
                profile
            } else {
                debug!("Loaded default active profile",);
                0
            };
        set_ble_profile(profile, self.bond_info(profile).is_some());
    }

    /// Read the live bond stored in `profile`.
    pub(crate) fn bond_info(&self, profile: u8) -> Option<ProfileInfo> {
        self.profiles.get(profile as usize)?.try_get_and(|info| !info.removed)
    }

    /// Whether this peer may use the selected profile.
    ///
    /// Security keys remain cached for all known hosts, but that does not grant
    /// them access to another slot. Only a genuinely unknown peer may pair into
    /// an empty profile.
    pub(crate) fn accepts_peer(&self, profile: u8, identity: &Identity) -> bool {
        if let Some(bond) = self.bond_info(profile) {
            return bond.info.identity.match_identity(identity);
        }
        !(0..SLOTS).any(|slot| {
            self.bond_info(slot as u8)
                .is_some_and(|bond| bond.info.identity.match_identity(identity))
        })
    }

    /// Check if the `identity` is the bonded dongle's identity.
    ///
    /// This function is used when searching for BLE host,
    /// the connected dongle(in the DONGLE_PROFILE) should be excluded.
    #[cfg(feature = "dongle")]
    pub(crate) fn is_bonded_dongle(&self, identity: &Identity) -> bool {
        self.bond_info(DONGLE_PROFILE)
            .is_some_and(|info| info.info.identity.match_identity(identity))
    }

    /// Keep known security identities available independently of profile admission.
    ///
    /// Empty profiles share the same local BLE identity as bonded profiles. An
    /// inactive host must be refused by the connection owner, not told that its
    /// saved key disappeared. Prefer the selected slot if legacy slots contain
    /// different keys for the same peer identity.
    pub(crate) fn update_stack_bonds(&self) {
        let active = self.bond_info(current_profile());
        set_ble_bonded(active.is_some());
        let mut desired: heapless::Vec<BondInformation, SLOTS> = heapless::Vec::new();
        if let Some(info) = active {
            let _ = desired.push(info.info);
        }
        for slot in 0..SLOTS {
            if let Some(info) = self.bond_info(slot as u8)
                && !desired
                    .iter()
                    .any(|bond| bond.identity.match_identity(&info.info.identity))
            {
                let _ = desired.push(info.info);
            }
        }

        // Reconcile rather than clear/re-add every key: unchanged profiles
        // must not enqueue resolving-list churn during every reconnect.
        loop {
            let stale = self.stack.with_bond_information(|bonds| {
                bonds
                    .iter()
                    .find(|bond| !desired.iter().any(|wanted| wanted == *bond))
                    .map(|bond| bond.identity)
            });
            let Some(identity) = stale else { break };
            if let Err(e) = self.stack.remove_bond_information(identity) {
                debug!("Remove bond info error: {:?}", e);
                break;
            }
        }
        for info in desired {
            if self
                .stack
                .with_bond_information(|bonds| bonds.iter().any(|bond| bond == &info))
            {
                continue;
            }
            if let Err(e) = self.stack.add_bond_information(info) {
                debug!("Add bond info error: {:?}", e);
            }
        }
    }

    /// Set a slot's bond and initial CCCD state.
    pub(crate) fn set_bond(&self, info: ProfileInfo) {
        let Some(state) = self.profiles.get(info.slot_num as usize) else {
            return;
        };
        let old = state.try_get();
        if old.as_ref() == Some(&info) {
            return;
        }
        let bond_changed = old.as_ref().is_none_or(|old| old.removed || old.info != info.info);
        state.sender().send(info);
        if bond_changed {
            self.update_stack_bonds();
        }
    }

    pub(crate) fn update_profile_cccd_table(&self, slot: u8, table: heapless::Vec<u8, CCCD_TABLE_SIZE>) {
        if let Some(state) = self.profiles.get(slot as usize) {
            state.sender().send_if_modified(|info| {
                if let Some(info) = info
                    && !info.removed
                    && info.cccd_table != table
                {
                    info.cccd_table = table.clone();
                    return true;
                }
                false
            });
        }
    }

    /// One persistence receiver per slot, retained across connection changes.
    pub(crate) async fn run(&self) -> ! {
        let mut receivers = self
            .profiles
            .each_ref()
            .map(|state| state.receiver().expect("one profile saver"));
        loop {
            let item = match select(
                select_array(receivers.each_mut().map(|receiver| receiver.changed())),
                self.updated_active_profile.wait(),
            )
            .await
            {
                Either::First((info, _)) => StorageItem::BondInfo(info),
                Either::Second(()) => StorageItem::ActiveBleProfile(current_profile()),
            };
            if store(item).await.is_err() {
                error!("Failed to save BLE profile");
            }
        }
    }

    /// Clear bonding information of the specified slot.
    pub(crate) fn clear_bond(&self, slot: u8) {
        if let Some(state) = self.profiles.get(slot as usize) {
            state.sender().send(ProfileInfo {
                slot_num: slot,
                removed: true,
                ..Default::default()
            });
            self.update_stack_bonds();
        }
    }

    /// Apply one profile action, then let the connection loop restart.
    pub(crate) async fn update_profile(&self) {
        loop {
            let action = BLE_PROFILE_CHANNEL.receive().await;
            let current = current_profile();
            let profile = match action {
                BleProfileAction::Switch(profile) if profile == current => {
                    crate::state::notify_connection_status();
                    continue;
                }
                BleProfileAction::Switch(profile) => profile,
                BleProfileAction::Previous => {
                    if current == 0 {
                        NUM_BLE_PROFILE as u8 - 1
                    } else {
                        current - 1
                    }
                }
                BleProfileAction::Next => {
                    let next = current + 1;
                    if next >= NUM_BLE_PROFILE as u8 { 0 } else { next }
                }
                BleProfileAction::ClearBond => {
                    self.clear_bond(current);
                    return;
                }
                BleProfileAction::ClearSlot(slot) => {
                    self.clear_bond(slot);
                    return;
                }
            };
            if profile != current {
                set_ble_profile(profile, self.bond_info(profile).is_some());
                // The connection owner applies this selection only after the old
                // ACL/GATT session is down. Its security state must remain valid
                // while the connection is being torn down.
                self.updated_active_profile.signal(());
                info!("Switched to BLE profile: {}", profile);
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use bt_hci::controller::ExternalController;
    use bt_hci::transport::{PacketToController, PacketToHost, Transport};

    use super::{DefaultPacketPool, HostResources, LongTermKey, ProfileInfo, ProfileManager, profile_address};
    use crate::test_support::test_block_on;

    #[test]
    fn profile_addresses_are_stable_and_distinct() {
        let base = [1, 2, 3, 4, 5, 0xC6];
        assert_eq!(profile_address(base, 0), base);
        assert_eq!(profile_address(base, 2), [1, 2, 3, 6, 5, 0xC6]);
        for byte in [0, 0xFF] {
            let base = [1, 2, 3, byte, 5, 0xC6];
            for profile in 0..=crate::NUM_BLE_PROFILE as u8 {
                for other in 0..profile {
                    assert_ne!(profile_address(base, profile), profile_address(base, other));
                }
            }
        }
        assert_eq!(profile_address([1, 2, 3, 0xFF, 5, 0xC6], 1), [1, 2, 3, 0, 5, 0xC6]);
    }

    #[test]
    fn switching_to_empty_profile_preserves_known_host_security() {
        use crate::channel::BLE_PROFILE_CHANNEL;
        use crate::state::{current_ble_status, current_profile};

        crate::test_support::reset_connection_status();
        with_manager::<2>(|manager| {
            let mut host = ProfileInfo::default();
            host.info.identity.addr = super::Address::random([1, 2, 3, 4, 5, 0xC6]);
            host.info.ltk = LongTermKey(42);
            manager.set_bond(host.clone());
            manager.update_stack_bonds();

            test_block_on(async {
                BLE_PROFILE_CHANNEL.send(super::BleProfileAction::Switch(1)).await;
                manager.update_profile().await;
            });

            assert_eq!(current_profile(), 1);
            assert!(!current_ble_status().bonded);
            assert!(manager.bond_info(1).is_none());
            assert_eq!(manager.bond_info(0), Some(host.clone()));
            manager.stack.with_bond_information(|bonds| {
                assert!(
                    bonds.iter().any(|bond| bond == &host.info),
                    "switching to an empty profile must not make the known host's key disappear"
                );
            });

            // The connection owner reconciles bonds after the old ACL is down.
            // Known security identities must survive the new empty-profile
            // advertising window too; admission is a separate profile check.
            manager.update_stack_bonds();
            manager.stack.with_bond_information(|bonds| {
                assert!(
                    bonds.iter().any(|bond| bond == &host.info),
                    "empty-profile advertising must retain known host security identities"
                );
            });
        });
    }

    #[test]
    fn empty_profile_refuses_a_host_already_bonded_to_another_slot() {
        crate::test_support::reset_connection_status();
        with_manager::<2>(|manager| {
            let mut host = ProfileInfo::default();
            host.info.identity.addr = super::Address::random([1, 2, 3, 4, 5, 0xC6]);
            host.info.ltk = LongTermKey(42);
            let unknown = super::Identity {
                addr: super::Address::random([9, 8, 7, 6, 5, 0xC6]),
                irk: None,
            };
            manager.set_bond(host.clone());

            // Existing controls: the bonded slot accepts only its own host,
            // and an empty slot still accepts genuinely new pairing peers.
            assert!(manager.accepts_peer(0, &host.info.identity));
            assert!(!manager.accepts_peer(0, &unknown));
            assert!(manager.accepts_peer(1, &unknown));
            assert!(manager.bond_info(1).is_none());
            assert!(
                !manager.accepts_peer(1, &host.info.identity),
                "an inactive bonded host must not enter an empty profile as a new pairing peer"
            );
        });
    }

    #[test]
    fn known_host_cache_and_empty_profile_admission_follow_explicit_bond_clear() {
        use crate::channel::BLE_PROFILE_CHANNEL;

        crate::test_support::reset_connection_status();
        with_manager::<3>(|manager| {
            let mut first = ProfileInfo::default();
            first.info.identity.addr = super::Address::random([1, 2, 3, 4, 5, 0xC6]);
            first.info.ltk = LongTermKey(41);
            let mut second = ProfileInfo {
                slot_num: 1,
                ..Default::default()
            };
            second.info.identity.addr = super::Address::random([9, 8, 7, 6, 5, 0xC6]);
            second.info.ltk = LongTermKey(42);
            manager.set_bond(first.clone());
            manager.set_bond(second.clone());
            test_block_on(async {
                BLE_PROFILE_CHANNEL.send(super::BleProfileAction::Switch(2)).await;
                manager.update_profile().await;
            });
            manager.update_stack_bonds();
            manager.stack.with_bond_information(|bonds| {
                assert_eq!(bonds.len(), 2);
                assert!(bonds.contains(&first.info));
                assert!(bonds.contains(&second.info));
            });
            assert!(!manager.accepts_peer(2, &first.info.identity));
            assert!(!manager.accepts_peer(2, &second.info.identity));

            // Clearing one slot must not evict the other host. The explicitly
            // forgotten peer is now eligible for genuinely new pairing.
            manager.clear_bond(0);
            manager.stack.with_bond_information(|bonds| {
                assert_eq!(bonds.len(), 1);
                assert_eq!(bonds[0], second.info);
            });
            assert!(manager.accepts_peer(2, &first.info.identity));
            assert!(!manager.accepts_peer(2, &second.info.identity));
            assert_eq!(manager.bond_info(1), Some(second));
            assert!(manager.bond_info(2).is_none());
        });
    }

    struct TestTransport;

    impl embedded_io_async::ErrorType for TestTransport {
        type Error = bt_hci::ReadHciError<core::convert::Infallible>;
    }

    impl Transport for TestTransport {
        async fn read<'a, P: PacketToHost<'a>>(&self, _: &'a mut [u8]) -> Result<P, Self::Error> {
            panic!("unexpected controller read");
        }

        async fn write<P: PacketToController>(&self, _: &P) -> Result<(), Self::Error> {
            panic!("unexpected controller write");
        }
    }

    type TestManager<'b, 's, const SLOTS: usize> =
        ProfileManager<'b, 's, ExternalController<TestTransport, 1>, DefaultPacketPool, SLOTS>;

    fn with_manager<const SLOTS: usize>(test: impl FnOnce(&mut TestManager<'_, '_, SLOTS>)) {
        crate::ble::rng::seed([7; 32]);
        let mut resources = HostResources::<DefaultPacketPool, 1, 1, 1, SLOTS>::new();
        let stack = trouble_host::new(ExternalController::<_, 1>::new(TestTransport), &mut resources).build();
        let mut manager = ProfileManager::new(&stack);
        test(&mut manager);
    }

    #[test]
    fn bond_and_cccd_updates_target_the_requested_profile() {
        with_manager::<2>(|manager| {
            let first = ProfileInfo::default();
            let mut second = ProfileInfo {
                slot_num: 1,
                ..Default::default()
            };
            second.info.ltk = LongTermKey(1);
            manager.set_bond(first.clone());
            manager.set_bond(second.clone());

            second.cccd_table = heapless::Vec::from_slice(&[1, 2]).unwrap();
            manager.update_profile_cccd_table(1, second.cccd_table.clone());
            assert_eq!(manager.bond_info(0), Some(first));
            assert_eq!(manager.bond_info(1), Some(second));
        });
    }

    #[test]
    fn profile_actions_update_selection_and_stack_bonds() {
        use super::BleProfileAction::{ClearBond, Next, Previous, Switch};
        use crate::channel::BLE_PROFILE_CHANNEL;
        use crate::state::{current_ble_status, current_profile};

        with_manager::<2>(|manager| {
            for slot in 0..2 {
                let mut info = ProfileInfo {
                    slot_num: slot,
                    ..Default::default()
                };
                info.info.ltk = LongTermKey(slot.into());
                manager.set_bond(info);
            }
            test_block_on(async {
                for (action, selected) in [(Switch(1), 1), (Previous, 0), (Next, 1)] {
                    let previous = current_profile();
                    BLE_PROFILE_CHANNEL.send(action).await;
                    manager.update_profile().await;
                    assert_eq!(current_profile(), selected);
                    assert!(current_ble_status().bonded);
                    manager.stack.with_bond_information(|bonds| {
                        assert_eq!(bonds.len(), 1);
                        assert_eq!(bonds[0].ltk, LongTermKey(previous.into()));
                    });
                    // The connection owner reconciles only after old-session
                    // teardown, then the selected slot wins duplicate identities.
                    manager.update_stack_bonds();
                    manager.stack.with_bond_information(|bonds| {
                        assert_eq!(bonds.len(), 1);
                        assert_eq!(bonds[0].ltk, LongTermKey(selected.into()));
                    });
                }
                BLE_PROFILE_CHANNEL.send(ClearBond).await;
                manager.update_profile().await;
            });
            assert_eq!(current_profile(), 1);
            assert!(!current_ble_status().bonded);
            assert!(manager.bond_info(1).is_none());
            assert!(manager.bond_info(0).is_some());
            manager.stack.with_bond_information(|bonds| {
                assert_eq!(bonds.len(), 1);
                assert_eq!(bonds[0].ltk, LongTermKey(0));
                assert!(!manager.accepts_peer(1, &bonds[0].identity));
            });
        });
    }

    #[test]
    fn profile_state_is_restored_from_storage() {
        use embassy_futures::select::{select, select3};

        use crate::channel::BLE_PROFILE_CHANNEL;
        use crate::config::StorageConfig;
        use crate::core_traits::Runnable;
        use crate::storage::{Storage, StorageKey, StorageValue, async_flash_wrapper, read};
        use crate::test_support::{InMemoryFlash, clear_flash_channel, reset_connection_status};

        with_manager::<2>(|manager| {
            clear_flash_channel();
            let mut host = ProfileInfo {
                slot_num: 1,
                ..Default::default()
            };
            host.info.ltk = LongTermKey(42);
            host.cccd_table = heapless::Vec::from_slice(&[1, 2]).unwrap();
            manager.set_bond(ProfileInfo::default());
            manager.set_bond(host.clone());
            manager.clear_bond(0);
            test_block_on(async {
                BLE_PROFILE_CHANNEL.send(super::BleProfileAction::Switch(1)).await;
                manager.update_profile().await;
                let flash = InMemoryFlash::<16_384, 4_096, 1>::new();
                let reboot = flash.clone();
                let mut storage = Box::pin(Storage::new(async_flash_wrapper(flash), &StorageConfig::default())).await;
                let saved = async {
                    for slot in 0..2 {
                        loop {
                            if let Some(StorageValue::BondInfo(info)) = read(StorageKey::BondInfo(slot)).await.unwrap()
                            {
                                if slot == 0 {
                                    assert!(info.removed);
                                } else {
                                    assert_eq!(info, host);
                                }
                                break;
                            }
                        }
                    }
                    loop {
                        if let Some(StorageValue::ActiveBleProfile(active)) =
                            read(StorageKey::ActiveBleProfile).await.unwrap()
                        {
                            assert_eq!(active, 1);
                            break;
                        }
                    }
                };
                select3(manager.run(), storage.run(), saved).await;
                drop(storage);

                clear_flash_channel();
                reset_connection_status();
                let mut restored: TestManager<'_, '_, 2> = ProfileManager::new(manager.stack);
                let mut storage = Box::pin(Storage::new(async_flash_wrapper(reboot), &StorageConfig::default())).await;
                select(restored.load_bonded_devices(), storage.run()).await;
                assert!(restored.bond_info(0).is_none());
                assert_eq!(restored.bond_info(1), Some(host));
                assert_eq!(crate::state::current_profile(), 1);
                assert!(crate::state::current_ble_status().bonded);
            });
        });
    }
}
