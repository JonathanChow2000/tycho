use std::collections::HashMap;

use alloy::primitives::U256;
use tycho_client::feed::{synchronizer::ComponentWithState, BlockHeader};
use tycho_common::{models::token::Token, Bytes};

use super::state::{
    LidoV4State, StakingState, BUFFERED_ETHER_ATTR, CL_PENDING_BALANCE_ATTR,
    CL_VALIDATORS_BALANCE_ATTR, DEPOSITED_POST_REPORT_ATTR, EXTERNAL_SHARES_ATTR,
    MAX_STAKE_LIMIT_ATTR, MAX_STAKE_LIMIT_GROWTH_BLOCKS_ATTR, PREV_STAKE_BLOCK_NUMBER_ATTR,
    PREV_STAKE_LIMIT_ATTR, STETH_COMPONENT_ID, TOTAL_SHARES_ATTR, WSTETH_SHARES_ATTR,
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
            .eq_ignore_ascii_case(STETH_COMPONENT_ID)
        {
            return Err(InvalidSnapshotError::ValueError(format!(
                "unknown Lido V4 component id {}",
                snapshot.component.id
            )));
        }

        let value = |name: &str| -> Result<U256, InvalidSnapshotError> {
            snapshot
                .state
                .attributes
                .get(name)
                .ok_or_else(|| InvalidSnapshotError::MissingAttribute(name.to_string()))
                .map(|value| U256::from_be_slice(value))
        };

        let staking_state = StakingState::new(
            value(PREV_STAKE_BLOCK_NUMBER_ATTR)?.to::<u32>(),
            value(PREV_STAKE_LIMIT_ATTR)?,
            value(MAX_STAKE_LIMIT_GROWTH_BLOCKS_ATTR)?.to::<u32>(),
            value(MAX_STAKE_LIMIT_ATTR)?,
        );

        // Seeded from the observed header; `apply_block` moves it to the execution block before
        // the state is quoted.
        Ok(LidoV4State::new(
            block.number,
            value(TOTAL_SHARES_ATTR)?,
            value(EXTERNAL_SHARES_ATTR)?,
            value(BUFFERED_ETHER_ATTR)?,
            value(DEPOSITED_POST_REPORT_ATTR)?,
            value(CL_VALIDATORS_BALANCE_ATTR)?,
            value(CL_PENDING_BALANCE_ATTR)?,
            staking_state,
            value(WSTETH_SHARES_ATTR)?,
        ))
    }
}
