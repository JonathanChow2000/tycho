use std::collections::HashMap;

use alloy::primitives::U256;
use tycho_client::feed::{synchronizer::ComponentWithState, BlockHeader};
use tycho_common::{models::token::Token, Bytes};

use super::state::{
    LidoV4State, StakingState, BUFFERED_ETHER_AND_DEPOSITED_POST_REPORT_ATTR,
    CL_VALIDATORS_BALANCE_AND_CL_PENDING_BALANCE_ATTR, COMPONENT_ID, STAKING_STATE_ATTR,
    TOTAL_AND_EXTERNAL_SHARES_ATTR, WSTETH_SHARES_ATTR,
};
use crate::protocol::{
    errors::InvalidSnapshotError,
    models::{DecoderContext, TryFromWithBlock},
};

impl TryFromWithBlock<ComponentWithState, BlockHeader> for LidoV4State {
    type Error = InvalidSnapshotError;

    async fn try_from_with_header(
        snapshot: ComponentWithState,
        block: BlockHeader,
        _account_balances: &HashMap<Bytes, HashMap<Bytes, Bytes>>,
        _all_tokens: &HashMap<Bytes, Token>,
        _decoder_context: &DecoderContext,
    ) -> Result<Self, Self::Error> {
        if !snapshot
            .component
            .id
            .eq_ignore_ascii_case(COMPONENT_ID)
        {
            return Err(InvalidSnapshotError::ValueError(format!(
                "unknown Lido V4 component id {}",
                snapshot.component.id
            )));
        }

        let word = |name: &str| -> Result<U256, InvalidSnapshotError> {
            snapshot
                .state
                .attributes
                .get(name)
                .ok_or_else(|| InvalidSnapshotError::MissingAttribute(name.to_string()))
                .map(|value| U256::from_be_slice(value))
        };

        let (total_shares, external_shares) =
            LidoV4State::split_low_high_u128(word(TOTAL_AND_EXTERNAL_SHARES_ATTR)?);
        let (buffered_ether, deposited_post_report) =
            LidoV4State::split_low_high_u128(word(BUFFERED_ETHER_AND_DEPOSITED_POST_REPORT_ATTR)?);
        let (cl_validators_balance, cl_pending_balance) = LidoV4State::split_low_high_u128(word(
            CL_VALIDATORS_BALANCE_AND_CL_PENDING_BALANCE_ATTR,
        )?);

        Ok(LidoV4State::new(
            block.number,
            block.timestamp,
            total_shares,
            external_shares,
            buffered_ether,
            deposited_post_report,
            cl_validators_balance,
            cl_pending_balance,
            StakingState::from_u256(word(STAKING_STATE_ATTR)?),
            word(WSTETH_SHARES_ATTR)?,
        ))
    }
}
