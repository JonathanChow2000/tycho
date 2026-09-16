//! Pauses both components when a tracked proxy changes implementation.
//!
//! The slots this package reads were verified against the implementations the manifest records.
//! An upgrade may move or repurpose them while the old positions keep decoding to plausible
//! numbers, so the block that installs a different implementation pauses the components. They
//! stay paused until someone re-verifies the slots, records the new implementations and
//! re-releases.

use anyhow::{anyhow, Result};
use substreams_ethereum::pb::eth::v2::{Block, TransactionTrace};

use crate::{
    constants::{EIP1967_IMPLEMENTATION_POSITION, TRACKED_PROXIES},
    state::InitialState,
};

/// A tracked proxy delegating to an implementation other than the recorded one.
#[derive(Debug, PartialEq, Eq)]
pub struct Upgrade {
    pub label: &'static str,
    pub proxy: [u8; 20],
    pub recorded: [u8; 20],
    pub installed: [u8; 20],
}

/// Every upgrade in `block`, each with the transaction that installed it.
///
/// All four proxies are EIP-1967: an upgrade is a write to the implementation slot on the proxy
/// itself. A write that lands on the recorded implementation is not an upgrade.
pub fn detect_upgrades<'a>(
    block: &'a Block,
    initial_state: &InitialState,
) -> Result<Vec<(&'a TransactionTrace, Upgrade)>> {
    let mut upgrades = Vec::new();
    for tx in block.transactions() {
        for call in tx
            .calls
            .iter()
            .filter(|call| !call.state_reverted)
        {
            for change in &call.storage_changes {
                if change.key != EIP1967_IMPLEMENTATION_POSITION {
                    continue;
                }
                let Some(proxy) = TRACKED_PROXIES
                    .iter()
                    .find(|proxy| change.address == proxy.proxy)
                else {
                    continue;
                };
                let installed = address_in_word(&change.new_value)?;
                let recorded = initial_state.implementation_of(proxy)?;
                if installed != recorded {
                    upgrades.push((
                        tx,
                        Upgrade { label: proxy.label, proxy: proxy.proxy, recorded, installed },
                    ));
                }
            }
        }
    }
    Ok(upgrades)
}

/// The address in a 32-byte storage word.
fn address_in_word(word: &[u8]) -> Result<[u8; 20]> {
    let (zeroes, address) = split_word(word)?;
    if zeroes.iter().any(|byte| *byte != 0) {
        return Err(anyhow!("implementation slot does not hold an address: {word:02x?}"));
    }
    Ok(address)
}

fn split_word(word: &[u8]) -> Result<([u8; 12], [u8; 20])> {
    let word: [u8; 32] = word
        .try_into()
        .map_err(|_| anyhow!("implementation slot value is {} bytes, not a word", word.len()))?;
    let mut prefix = [0u8; 12];
    let mut address = [0u8; 20];
    prefix.copy_from_slice(&word[..12]);
    address.copy_from_slice(&word[12..]);
    Ok((prefix, address))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use substreams::hex;
    use substreams_ethereum::pb::eth::v2::{Call, StorageChange, TransactionTraceStatus};

    use super::*;
    use crate::{constants::RATE_LIMITER_ADDRESS, state::tests::snapshot};

    /// The rate limiter's implementation at block 25940000.
    pub(crate) const RATE_LIMITER_V1: [u8; 20] = hex!("9ea4d0fd09b628e23b1998f2153e27e5261b1b67");
    pub(crate) const OTHER: [u8; 20] = hex!("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");

    pub(crate) fn initial_state() -> InitialState {
        snapshot()
    }

    fn word(address: [u8; 20]) -> Vec<u8> {
        let mut word = vec![0u8; 12];
        word.extend_from_slice(&address);
        word
    }

    /// A write of `implementation` to `proxy`'s EIP-1967 implementation slot.
    pub(crate) fn upgrade_write(proxy: [u8; 20], implementation: [u8; 20]) -> StorageChange {
        StorageChange {
            address: proxy.to_vec(),
            key: EIP1967_IMPLEMENTATION_POSITION.to_vec(),
            new_value: word(implementation),
            ..Default::default()
        }
    }

    pub(crate) fn rate_limiter_upgrade_to(implementation: [u8; 20]) -> StorageChange {
        upgrade_write(RATE_LIMITER_ADDRESS, implementation)
    }

    /// A block whose only successful transaction (index 7) made `storage_changes` in one call.
    pub(crate) fn block_with(storage_changes: Vec<StorageChange>, state_reverted: bool) -> Block {
        Block {
            number: 25_940_000,
            transaction_traces: vec![TransactionTrace {
                index: 7,
                status: TransactionTraceStatus::Succeeded as i32,
                calls: vec![Call { storage_changes, state_reverted, ..Default::default() }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fixtures::*, *};
    use crate::constants::{
        EETH_ADDRESS, LIQUIDITY_POOL_ADDRESS, RATE_LIMITER_ADDRESS, REDEMPTION_MANAGER_ADDRESS,
    };

    #[test]
    fn writing_the_recorded_implementation_is_not_an_upgrade() {
        let block = block_with(vec![rate_limiter_upgrade_to(RATE_LIMITER_V1)], false);
        assert!(detect_upgrades(&block, &initial_state())
            .expect("detect")
            .is_empty());
    }

    #[test]
    fn writing_another_implementation_is_an_upgrade() {
        let block = block_with(vec![rate_limiter_upgrade_to(OTHER)], false);

        let upgrades = detect_upgrades(&block, &initial_state()).expect("detect");

        let [(tx, upgrade)] = upgrades.as_slice() else {
            panic!("expected one upgrade, got {upgrades:?}");
        };
        assert_eq!(tx.index, 7);
        assert_eq!(
            *upgrade,
            Upgrade {
                label: "rate_limiter",
                proxy: RATE_LIMITER_ADDRESS,
                recorded: RATE_LIMITER_V1,
                installed: OTHER,
            }
        );
    }

    /// The escrow migration at block 25533308 upgraded three proxies in one block; each is
    /// reported.
    #[test]
    fn every_tracked_proxy_is_watched() {
        let block = block_with(
            vec![
                upgrade_write(LIQUIDITY_POOL_ADDRESS, OTHER),
                upgrade_write(EETH_ADDRESS, OTHER),
                upgrade_write(REDEMPTION_MANAGER_ADDRESS, OTHER),
                upgrade_write(RATE_LIMITER_ADDRESS, OTHER),
            ],
            false,
        );

        let upgrades = detect_upgrades(&block, &initial_state()).expect("detect");

        let labels: Vec<_> = upgrades
            .iter()
            .map(|(_, upgrade)| upgrade.label)
            .collect();
        assert_eq!(labels, ["liquidity_pool", "eeth", "redemption_manager", "rate_limiter"]);
    }

    /// weETH is a proxy too, but none of its own storage is tracked.
    #[test]
    fn an_untracked_proxy_is_ignored() {
        let block = block_with(vec![upgrade_write(crate::constants::WEETH_ADDRESS, OTHER)], false);
        assert!(detect_upgrades(&block, &initial_state())
            .expect("detect")
            .is_empty());
    }

    /// The same word written to any other slot on a tracked proxy is ordinary state.
    #[test]
    fn another_slot_on_a_tracked_proxy_is_ignored() {
        let mut change = rate_limiter_upgrade_to(OTHER);
        change.key = vec![0u8; 32];
        let block = block_with(vec![change], false);
        assert!(detect_upgrades(&block, &initial_state())
            .expect("detect")
            .is_empty());
    }

    #[test]
    fn a_reverted_call_installs_nothing() {
        let block = block_with(vec![rate_limiter_upgrade_to(OTHER)], true);
        assert!(detect_upgrades(&block, &initial_state())
            .expect("detect")
            .is_empty());
    }

    #[test]
    fn a_slot_value_that_is_not_an_address_is_an_error() {
        let mut change = rate_limiter_upgrade_to(OTHER);
        change.new_value = vec![1u8; 32];
        let block = block_with(vec![change], false);
        assert!(detect_upgrades(&block, &initial_state()).is_err());
    }
}
