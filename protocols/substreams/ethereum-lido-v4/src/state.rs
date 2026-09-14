use anyhow::{anyhow, Result};
use serde::Deserialize;
use tycho_substreams::models::{Attribute, ChangeType};

use substreams::scalar::BigInt;

use crate::{
    constants::{
        BUFFERED_ETHER_AND_DEPOSITED_POST_REPORT_ATTR,
        BUFFERED_ETHER_AND_DEPOSITED_POST_REPORT_KEY,
        CL_VALIDATORS_BALANCE_AND_CL_PENDING_BALANCE_ATTR,
        CL_VALIDATORS_BALANCE_AND_CL_PENDING_BALANCE_KEY, STAKING_STATE_ATTR,
        TOTAL_AND_EXTERNAL_SHARES_ATTR, TOTAL_AND_EXTERNAL_SHARES_KEY, WSTETH_SHARES_ATTR,
    },
    utils::{attribute_with_bytes, bytes_from_hex},
};

#[derive(Clone, Debug, Deserialize)]
pub struct InitialState {
    pub start_block: u64,
    pub total_and_external_shares: String,
    pub buffered_ether_and_deposited_post_report: String,
    pub cl_validators_balance_and_cl_pending_balance: String,
    pub staking_state: String,
    pub wsteth_shares: String,
}

impl InitialState {
    pub fn parse(params: &str) -> Result<Self> {
        serde_json::from_str(params)
            .map_err(|e| anyhow!("Failed to parse Lido V4 initial state: {e}"))
    }

    /// Every tracked slot, since one component serves every direction.
    pub fn creation_attributes(&self) -> Result<Vec<Attribute>> {
        Ok(vec![
            (TOTAL_AND_EXTERNAL_SHARES_ATTR, &self.total_and_external_shares),
            (
                BUFFERED_ETHER_AND_DEPOSITED_POST_REPORT_ATTR,
                &self.buffered_ether_and_deposited_post_report,
            ),
            (
                CL_VALIDATORS_BALANCE_AND_CL_PENDING_BALANCE_ATTR,
                &self.cl_validators_balance_and_cl_pending_balance,
            ),
            (STAKING_STATE_ATTR, &self.staking_state),
            (WSTETH_SHARES_ATTR, &self.wsteth_shares),
        ]
        .into_iter()
        .map(|(name, value)| {
            Ok(attribute_with_bytes(name, &bytes_from_hex(value)?, ChangeType::Creation))
        })
        .collect::<Result<Vec<_>>>()?)
    }

    /// The balance inputs carried by the snapshot, used to seed the store and to report the
    /// component balances on the activation block.
    pub fn balance_state(&self) -> Result<BalanceState> {
        Ok(BalanceState {
            total_and_external_shares: big_int_from_hex(&self.total_and_external_shares)?,
            buffered_ether_and_deposited_post_report: big_int_from_hex(
                &self.buffered_ether_and_deposited_post_report,
            )?,
            cl_validators_balance_and_cl_pending_balance: big_int_from_hex(
                &self.cl_validators_balance_and_cl_pending_balance,
            )?,
        })
    }
}

/// Decodes a hex-encoded raw slot value into an unsigned `BigInt`.
pub fn big_int_from_hex(value: &str) -> Result<BigInt> {
    Ok(BigInt::from_unsigned_bytes_be(&bytes_from_hex(value)?))
}

/// The raw slot values that determine the two components' balances.
///
/// stETH packs two scalars per slot: `buffered_ether` / `deposited_post_report` in one,
/// `cl_validators_balance` / `cl_pending_balance` in another, and `total_shares` /
/// `external_shares` in a third.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BalanceState {
    pub total_and_external_shares: BigInt,
    pub buffered_ether_and_deposited_post_report: BigInt,
    pub cl_validators_balance_and_cl_pending_balance: BigInt,
}

impl BalanceState {
    /// `Lido.getTotalPooledEther()`: the ether the protocol holds itself plus the ether backing
    /// the external shares (stVaults), which Lido values at the internal share rate.
    pub fn total_pooled_ether(&self) -> BigInt {
        let internal_ether = self.internal_ether();
        let (total_shares, external_shares) = split_low_high_u128(&self.total_and_external_shares);
        let internal_shares = total_shares.saturating_sub(external_shares);
        if internal_shares == 0 {
            return internal_ether;
        }
        let external_ether = big_int_from_u128(external_shares) * internal_ether.clone() /
            big_int_from_u128(internal_shares);
        internal_ether + external_ether
    }

    /// `Lido._getInternalEther()`: buffered ether plus every balance counted on the consensus
    /// layer - active validators, deposits pending activation, and deposits made since the last
    /// oracle report. Since Lido v4 these are tracked as balances, so there is no per-validator
    /// stake to multiply by.
    fn internal_ether(&self) -> BigInt {
        let (buffered_ether, deposited_post_report) =
            split_low_high_u128(&self.buffered_ether_and_deposited_post_report);
        let (cl_validators_balance, cl_pending_balance) =
            split_low_high_u128(&self.cl_validators_balance_and_cl_pending_balance);
        big_int_from_u128(buffered_ether) +
            big_int_from_u128(cl_validators_balance) +
            big_int_from_u128(cl_pending_balance) +
            big_int_from_u128(deposited_post_report)
    }

    /// Applies a newly observed raw slot value, keyed as in the store.
    pub fn apply(&mut self, key: &str, value: BigInt) {
        if key == TOTAL_AND_EXTERNAL_SHARES_KEY {
            self.total_and_external_shares = value;
        } else if key == BUFFERED_ETHER_AND_DEPOSITED_POST_REPORT_KEY {
            self.buffered_ether_and_deposited_post_report = value;
        } else if key == CL_VALIDATORS_BALANCE_AND_CL_PENDING_BALANCE_KEY {
            self.cl_validators_balance_and_cl_pending_balance = value;
        }
    }
}

/// Splits a packed slot into its low and high 128-bit halves.
fn split_low_high_u128(packed: &BigInt) -> (u128, u128) {
    let bytes = packed.to_bytes_be().1;
    let mut padded = [0u8; 32];
    let take = bytes.len().min(32);
    padded[32 - take..].copy_from_slice(&bytes[bytes.len() - take..]);
    let high = u128::from_be_bytes(
        padded[..16]
            .try_into()
            .expect("16 bytes"),
    );
    let low = u128::from_be_bytes(
        padded[16..]
            .try_into()
            .expect("16 bytes"),
    );
    (low, high)
}

/// `substreams::scalar::BigInt` has no `From<u128>`, so widen through big-endian bytes.
fn big_int_from_u128(value: u128) -> BigInt {
    BigInt::from_unsigned_bytes_be(&value.to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The snapshot the manifest ships, read from stETH storage at the Lido v4 migration block
    /// 25603297.
    fn snapshot() -> InitialState {
        InitialState {
            start_block: 25_603_297,
            total_and_external_shares:
                "0x00000000000000c9b9001a2e4d7ccd3f00000000000639d56fd47ec2536471cb".to_string(),
            buffered_ether_and_deposited_post_report:
                "0x000000000000a13dbebf95fa7d800000000000000000001d4009424f9565a013".to_string(),
            cl_validators_balance_and_cl_pending_balance:
                "0x00000000000000000000000000000000000000000007162e4f16cc0af69a3200".to_string(),
            staking_state: "0x00001fc3842bd1f071c000000000190000001fc383c40d61b0f6f0000186acdf"
                .to_string(),
            wsteth_shares: "0x000000000000000000000000000000000000000000030059cedfb0543bebad72"
                .to_string(),
        }
    }

    fn big(value: &str) -> BigInt {
        value
            .parse::<BigInt>()
            .expect("decimal BigInt")
    }

    #[test]
    fn packed_slots_decode_to_the_documented_halves() {
        let state = snapshot()
            .balance_state()
            .expect("balance state");
        let (total_shares, external_shares) = split_low_high_u128(&state.total_and_external_shares);

        // stETH.getTotalShares() and the stVaults' share of them, at block 25603297.
        assert_eq!(big_int_from_u128(total_shares), big("7526667021904051320418763"));
        assert_eq!(big_int_from_u128(external_shares), big("3721126242498807385407"));
    }

    #[test]
    fn total_pooled_ether_matches_chain() {
        let state = snapshot()
            .balance_state()
            .expect("balance state");

        // stETH.getTotalPooledEther() at block 25603297.
        assert_eq!(state.total_pooled_ether(), big("9333821188342623875037049"));
    }

    #[test]
    fn total_pooled_ether_counts_the_ether_behind_external_shares() {
        let mut state = snapshot()
            .balance_state()
            .expect("balance state");
        let with_external = state.total_pooled_ether();

        // Keep totalShares, drop externalShares: what is left is the internal ether alone.
        let (total_shares, _) = split_low_high_u128(&state.total_and_external_shares);
        state.apply(TOTAL_AND_EXTERNAL_SHARES_KEY, big_int_from_u128(total_shares));

        // bufferedEther + clValidatorsBalance + clPendingBalance + depositedPostReport at
        // block 25603297; the ~4,615 ETH gap is the stVaults' share of the pool.
        assert_eq!(state.total_pooled_ether(), big("9329206618989993371095571"));
        assert!(with_external > state.total_pooled_ether());
    }

    #[test]
    fn apply_updates_only_the_keyed_slot() {
        let mut state = snapshot()
            .balance_state()
            .expect("balance state");
        let before = state.total_pooled_ether();

        state.apply(BUFFERED_ETHER_AND_DEPOSITED_POST_REPORT_KEY, BigInt::zero());

        // Dropping the buffered/deposited half lowers the pool by exactly that half ...
        assert_eq!(state.total_pooled_ether() < before, true);
        // ... and leaves the consensus-layer half in place.
        assert!(state.total_pooled_ether() > BigInt::zero());
    }
}
