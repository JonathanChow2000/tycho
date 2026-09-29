# Changelog

## v0.1.3

- `map_pools_created` takes a query-string parameter:
  `factory_address=<hex>&protocol_type_name=<name>`. The protocol type name used to be hardcoded
  to `pancakeswap_v3_pool`, so a fork indexed by this package can now emit its own. The
  Ethereum, Base, BSC and Arbitrum manifests pass `protocol_type_name=pancakeswap_v3_pool`, so
  their components are unchanged.
- `map_protocol_changes` takes a query-string parameter. `default_protocol_fee=<n>` sets the
  `protocol_fees/zero2one` and `protocol_fees/one2zero` attributes written on `Initialize` to `n`
  for every fee tier. Without it the module keeps the PancakeSwap V3 defaults, which depend on the
  fee tier (100, 500, 2500 and 10000 only). A pool with any other fee used to panic the module with
  `Unexpected fee value`. It still panics, but the message now names the fee and the parameter. The
  existing manifests pass no value, so their output is unchanged.
