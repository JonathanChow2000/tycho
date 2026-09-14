#!/usr/bin/env bash
# Prints the `params` snapshot for substreams.yaml: the five stETH storage slots the package
# tracks, read at one block. Lido v4 (block 25603297 onwards) moved the pooled-ether accounting
# to new slots, so the snapshot has to be taken at or after that block.
#
# Usage:
#   RPC_URL=<archive-rpc> ./scripts/compute_initial_state.sh [block_number]
#
# Falls back to ETH_RPC_URL, then to a public endpoint, when RPC_URL is unset.

set -euo pipefail

BLOCK_NUMBER=${1:-25603297}

if ! command -v cast >/dev/null 2>&1; then
  echo "Error: 'cast' is required but was not found in PATH." >&2
  exit 1
fi

STETH_PROXY="0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84"

# keccak256 of the names Lido.sol v4.0.0 documents next to each position constant.
TOTAL_AND_EXTERNAL_SHARES_SLOT="0x6038150aecaa250d524370a0fdcdec13f2690e0723eaf277f41d7cae26b359e6"
BUFFERED_ETHER_AND_DEPOSITED_POST_REPORT_SLOT="0x81a11fa1111afa59b50051f60ccf604a39d96acb484dc467ad8eadb4a63f0a5f"
CL_VALIDATORS_BALANCE_AND_CL_PENDING_BALANCE_SLOT="0x096e465397f38e659238ccd5d5a2c434ced54a63fd8d694045bfb058ab9d8112"
STAKING_STATE_SLOT="0xa3678de4a579be090bed1177e0a24f77cc29d181ac22fd7688aca344d8938015"
# shares[wstETH] in stETH's share mapping (mapping slot 0): cast index address <wstETH> 0
WSTETH_SHARES_SLOT="0xf37caed32e4e49c83636e0f1684f3f4a9a23c463a49eb17cd63abd50680b378b"

resolve_rpc_url() {
  local candidates=()

  if [ -n "${RPC_URL:-}" ]; then
    candidates+=("$RPC_URL")
  fi
  if [ -n "${ETH_RPC_URL:-}" ]; then
    candidates+=("$ETH_RPC_URL")
  fi
  candidates+=("https://ethereum-rpc.publicnode.com")

  local candidate
  for candidate in "${candidates[@]}"; do
    if cast block "$BLOCK_NUMBER" --rpc-url "$candidate" >/dev/null 2>&1; then
      echo "$candidate"
      return 0
    fi
  done

  echo "Error: no working Ethereum RPC endpoint (tried RPC_URL, ETH_RPC_URL, public fallback)." >&2
  exit 1
}

read_storage() {
  local contract=$1
  local slot=$2
  cast storage "$contract" "$slot" --block "$BLOCK_NUMBER" --rpc-url "$RPC_URL"
}

RPC_URL=$(resolve_rpc_url)

echo "Reading stETH raw storage at block $BLOCK_NUMBER from $RPC_URL..." >&2

total_and_external_shares=$(read_storage "$STETH_PROXY" "$TOTAL_AND_EXTERNAL_SHARES_SLOT")
buffered_ether_and_deposited_post_report=$(read_storage "$STETH_PROXY" "$BUFFERED_ETHER_AND_DEPOSITED_POST_REPORT_SLOT")
cl_validators_balance_and_cl_pending_balance=$(read_storage "$STETH_PROXY" "$CL_VALIDATORS_BALANCE_AND_CL_PENDING_BALANCE_SLOT")
staking_state=$(read_storage "$STETH_PROXY" "$STAKING_STATE_SLOT")
wsteth_shares=$(read_storage "$STETH_PROXY" "$WSTETH_SHARES_SLOT")

cat <<JSON
{
  "start_block": $BLOCK_NUMBER,
  "total_and_external_shares": "$total_and_external_shares",
  "buffered_ether_and_deposited_post_report": "$buffered_ether_and_deposited_post_report",
  "cl_validators_balance_and_cl_pending_balance": "$cl_validators_balance_and_cl_pending_balance",
  "staking_state": "$staking_state",
  "wsteth_shares": "$wsteth_shares"
}
JSON
