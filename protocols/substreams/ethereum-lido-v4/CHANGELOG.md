# Changelog

## v0.1.0

Initial Lido integration, on the storage layout Lido core v4.0.0 introduced at block 25603297.
Indexes two components from raw stETH storage slots:

- `stETH` (`0xae7a...fE84`) — ETH staking. One-directional: unstaking runs through the
  asynchronous withdrawal queue, so there is no stETH -> ETH quote.
- `wstETH` (`0x7f39...2Ca0`) — stETH wrap and unwrap, plus ETH -> wstETH: the wrapper's
  `receive()` stakes through `stETH.submit` and mints the shares in one call, which saves the
  hop through stETH. That direction is bounded by the same stake limit as a plain submit, so the
  component tracks the stake limit too.

Both contracts predate the package, so the module graph does not discover them from a creation
event. The manifest carries a state snapshot in `params` and the components are created at
`start_block`; regenerate the snapshot for a different start block with
`scripts/compute_initial_state.sh`. The snapshot has to be taken at or after block 25603297:
Lido v4 (Staking Router v3, LIP-35) moved the pooled-ether accounting from validator counts to
balances and zeroed the slots the previous layout used.

Component balances are reported as absolute values on every transaction that moves one of the
inputs:

- the stETH component reports `getTotalPooledEther()` in ETH: `bufferedEther +
  clValidatorsBalance + clPendingBalance + depositedPostReport`, plus the ether backing the
  external (stVaults) shares at the same share rate;
- the wstETH component reports the stETH locked in the wrapper
  (`sharesOf(wstETH) * totalPooledEther / totalShares`, i.e. `stETH.balanceOf(wstETH)`), which
  is its tradable liquidity. Reporting the pool total here would overstate it ~2x and
  double-count the protocol's TVL.

Carrying those inputs across blocks needs a `store_balance_slots` store module: a block that
touches one of the tracked slots usually leaves the others untouched.

The integration test keeps `skip_balance_check`: the stETH component's balance is protocol
accounting, not the stETH contract's own ETH balance (which only holds the buffered ether).
