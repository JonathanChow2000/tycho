use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
    sync::LazyLock,
};

use alloy::sol_types::SolValue;
use serde::Deserialize;
use tycho_common::{models::Chain, Bytes};

use crate::encoding::{
    errors::EncodingError,
    evm::{
        constants::{DEFAULT_EXECUTORS_JSON, UNISWAP_V2_FORKS, UNISWAP_V3_FORKS},
        utils::bytes_to_address,
    },
    models::{EncodingContext, Swap},
    swap_encoder::SwapEncoder,
};

/// Static attribute under which fallback components carry their pAMM address — the same
/// attribute the price-level-stream family uses.
const PAMM_ADDRESS_ATTRIBUTE: &str = "pamm_address";

/// The highest Uniswap V2 fee `TychoFallbackRouter` accepts (`feeBps <= 30`).
const MAX_UNISWAP_V2_FEE_BPS: u8 = 30;

/// Slipstream deployments and their forks. The registry encodes them through
/// `SlipstreamsSwapEncoder` because their executor data differs from Uniswap V3's, but the pool
/// itself keeps V3's `swap` ABI and callback, which is all `TychoFallbackRouter` uses.
const SLIPSTREAMS_FORKS: &[&str] =
    &["aerodrome_slipstreams", "velodrome_slipstreams", "up_v3", "ramses_v3"];

/// The protocols a chain's executor config names, keyed by chain. A chain has a protocol's
/// per-chain singleton exactly when it has that protocol's executor, which is also how
/// `deploy-fallback-router.js` decides which singletons the chain's `TychoFallbackRouter` gets.
static EXECUTOR_PROTOCOLS: LazyLock<HashMap<Chain, HashSet<String>>> = LazyLock::new(|| {
    let config: HashMap<Chain, HashMap<String, String>> =
        serde_json::from_str(DEFAULT_EXECUTORS_JSON)
            // Embedded at compile time and parsed by every registry test, so a failure here
            // is a broken build, not a runtime condition.
            .expect("config/executor_addresses.json is valid");
    config
        .into_iter()
        .map(|(chain, executors)| (chain, executors.into_keys().collect()))
        .collect()
});

/// A protocol `TychoFallbackRouter` can fall back on, one per variant of the contract's
/// `FallbackProtocol` enum. The discriminant is the wire format's protocol byte, so the two enums
/// keep the same order.
///
/// The solver names one per swap in `user_data`; [`FallbackProtocol::from_protocol_system`]
/// says which one a Tycho component encodes as, and [`FallbackProtocol::supported_on`] whether
/// a chain's deployment can run it. [`PROTOCOLS`] holds the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FallbackProtocol {
    UniswapV2,
    UniswapV3,
    UniswapV4,
    Curve,
    FluidV1,
    AerodromeV1,
}

/// Everything the encoder knows about one fallback protocol besides how to pack its data.
struct FallbackProtocolInfo {
    /// The protocol this row describes. Its discriminant is the row's index in [`PROTOCOLS`].
    protocol: FallbackProtocol,
    /// The `fallback_protocol` value naming the protocol in a swap's `user_data`. Serde's
    /// snake-case rename of the protocol's [`FallbackSwapData`] variant must spell the same name,
    /// which is how [`FallbackSwap::from_user_data`] pairs the two.
    user_data_name: &'static str,
    /// The other Tycho `protocol_system` names that encode as this protocol, grouped as the fork
    /// lists they come in.
    protocol_systems: &'static [&'static [&'static str]],
    /// The executor a chain must have for its `TychoFallbackRouter` to run this protocol: the
    /// protocol calls a per-chain singleton the router takes as a constructor immutable, and
    /// `deploy-fallback-router.js` reads that singleton out of this executor's deployment entry.
    /// `None` for a protocol whose pool the swap addresses directly, which needs nothing from the
    /// deployment.
    required_executor: Option<&'static str>,
}

/// One row per [`FallbackProtocol`], in protocol-byte order.
///
/// # Adding a fallback protocol
///
/// 1. Add the [`FallbackProtocol`] variant last, matching the contract enum's new last variant.
/// 2. Add its row here, also last.
/// 3. Add the [`FallbackSwapData`] variant holding the fields the contract decodes, named so
///    serde's snake case spells the row's `user_data_name`.
/// 4. Add that variant's arm to [`FallbackSwapData::encode`], packing the fields in the order the
///    contract reads them.
static PROTOCOLS: &[FallbackProtocolInfo] = &[
    FallbackProtocolInfo {
        protocol: FallbackProtocol::UniswapV2,
        user_data_name: "uniswap_v2",
        // The forks share the constant-fee `swap(amount0Out, amount1Out, to, data)` pool.
        protocol_systems: &[UNISWAP_V2_FORKS],
        required_executor: None,
    },
    FallbackProtocolInfo {
        protocol: FallbackProtocol::UniswapV3,
        user_data_name: "uniswap_v3",
        // The forks and the Slipstream deployments share V3's `swap` and callback, which
        // `TychoFallbackRouter` answers whatever selector the fork renamed it to.
        protocol_systems: &[UNISWAP_V3_FORKS, SLIPSTREAMS_FORKS],
        required_executor: None,
    },
    FallbackProtocolInfo {
        protocol: FallbackProtocol::UniswapV4,
        user_data_name: "uniswap_v4",
        protocol_systems: &[&["uniswap_v4_hooks"]],
        // The PoolManager.
        required_executor: Some("uniswap_v4"),
    },
    FallbackProtocolInfo {
        protocol: FallbackProtocol::Curve,
        user_data_name: "curve",
        protocol_systems: &[&["vm:curve"]],
        required_executor: None,
    },
    FallbackProtocolInfo {
        protocol: FallbackProtocol::FluidV1,
        user_data_name: "fluid_v1",
        protocol_systems: &[],
        // The liquidity layer.
        required_executor: Some("fluid_v1"),
    },
    FallbackProtocolInfo {
        protocol: FallbackProtocol::AerodromeV1,
        user_data_name: "aerodrome_v1",
        protocol_systems: &[],
        required_executor: None,
    },
];

impl FallbackProtocolInfo {
    /// Whether a Tycho `protocol_system` encodes as this row's protocol.
    fn matches(&self, protocol_system: &str) -> bool {
        protocol_system == self.user_data_name ||
            self.protocol_systems
                .iter()
                .any(|names| names.contains(&protocol_system))
    }
}

impl FallbackProtocol {
    /// Every protocol, in protocol-byte order.
    pub fn all() -> impl Iterator<Item = Self> {
        PROTOCOLS
            .iter()
            .map(|info| info.protocol)
    }

    /// The ordinal of the matching `TychoFallbackRouter.FallbackProtocol` variant — the wire
    /// format's protocol byte.
    pub fn protocol_byte(self) -> u8 {
        self as u8
    }

    /// The [`PROTOCOLS`] row describing this protocol.
    fn info(self) -> &'static FallbackProtocolInfo {
        // `PROTOCOLS` is in protocol-byte order, held there by
        // `test_protocols_are_in_protocol_byte_order`, so the discriminant is the row index.
        &PROTOCOLS[self.protocol_byte() as usize]
    }

    /// The `fallback_protocol` value naming this protocol in a swap's `user_data`.
    pub fn user_data_name(self) -> &'static str {
        self.info().user_data_name
    }

    /// The protocol a Tycho `protocol_system` (or a [`user_data_name`](Self::user_data_name))
    /// encodes as, or `None` for one `TychoFallbackRouter` cannot run.
    pub fn from_protocol_system(protocol_system: &str) -> Option<Self> {
        PROTOCOLS
            .iter()
            .find(|info| info.matches(protocol_system))
            .map(|info| info.protocol)
    }

    /// Whether `chain`'s `TychoFallbackRouter` can run this protocol.
    ///
    /// A protocol with a `required_executor` calls a per-chain singleton the router takes as a
    /// constructor immutable, and a chain without that executor deploys the router with
    /// `address(0)` there, which makes the protocol revert
    /// `TychoFallbackRouter__ProtocolUnavailable`. Every other protocol takes its pool from the
    /// swap, so the router runs it on every chain and this returns `true`.
    pub fn supported_on(self, chain: Chain) -> bool {
        let Some(executor) = self.info().required_executor else {
            return true;
        };
        EXECUTOR_PROTOCOLS
            .get(&chain)
            .is_some_and(|executors| executors.contains(executor))
    }

    /// The protocols `chain`'s `TychoFallbackRouter` can run, in protocol-byte order.
    pub fn supported(chain: Chain) -> Vec<Self> {
        Self::all()
            .filter(|protocol| protocol.supported_on(chain))
            .collect()
    }
}

/// The fallback protocol and pool that fill a pAMM swap when the pAMM fails. The solver picks
/// them, JSON-encoded into `Swap::user_data` (e.g.
/// `{"fallback_protocol":"uniswap_v3","pool":"0x…"}`); a swap without one is rejected.
struct FallbackSwap {
    protocol: FallbackProtocol,
    data: FallbackSwapData,
}

impl FallbackSwap {
    fn from_user_data(user_data: &Option<Bytes>) -> Result<Self, EncodingError> {
        let Some(bytes) = user_data
            .as_ref()
            .filter(|bytes| !bytes.is_empty())
        else {
            return Err(EncodingError::InvalidInput(
                "Fallback swaps require user_data naming the fallback protocol \
                 (e.g. {\"fallback_protocol\":\"uniswap_v3\",\"pool\":\"0x…\"})"
                    .to_string(),
            ));
        };
        let invalid_json = |e| {
            EncodingError::InvalidInput(format!("Invalid fallback protocol user_data JSON: {e}"))
        };

        let mut value: serde_json::Value = serde_json::from_slice(bytes).map_err(invalid_json)?;
        let protocol = {
            let name = value
                .get("fallback_protocol")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    EncodingError::InvalidInput(
                        "Fallback protocol user_data JSON names no fallback_protocol".to_string(),
                    )
                })?;
            FallbackProtocol::from_protocol_system(name).ok_or_else(|| {
                EncodingError::InvalidInput(format!(
                    "Fallback protocol {name} is not one TychoFallbackRouter can run"
                ))
            })?
        };
        // A fork name resolves to its base protocol, whose own name is the serde tag.
        value["fallback_protocol"] = protocol.user_data_name().into();
        let data = serde_json::from_value(value).map_err(invalid_json)?;

        Ok(Self { protocol, data })
    }

    /// Encodes the protocol byte followed by the protocol's data.
    fn encode(&self) -> Result<Vec<u8>, EncodingError> {
        let mut encoded = vec![self.protocol.protocol_byte()];
        encoded.extend(self.data.encode()?);
        Ok(encoded)
    }
}

/// The protocol data `TychoFallbackRouter` decodes after the protocol byte, one variant per
/// [`FallbackProtocol`]. The variant fields are the protocol's `user_data` JSON fields.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "fallback_protocol", rename_all = "snake_case")]
enum FallbackSwapData {
    UniswapV2 {
        pair: Bytes,
        fee_bps: u8,
    },
    UniswapV3 {
        pool: Bytes,
    },
    UniswapV4 {
        fee: u32,
        tick_spacing: i32,
        hook: Bytes,
        #[serde(default)]
        hook_data: Bytes,
    },
    Curve {
        pool: Bytes,
        pool_type: u8,
        i: u8,
        j: u8,
    },
    FluidV1 {
        dex: Bytes,
        zero2one: bool,
    },
    AerodromeV1 {
        pool: Bytes,
    },
}

impl FallbackSwapData {
    /// Packs the fields in the order the contract reads them, rejecting values
    /// `TychoFallbackRouter` would revert on.
    fn encode(&self) -> Result<Vec<u8>, EncodingError> {
        let mut data = Vec::new();
        match self {
            FallbackSwapData::UniswapV2 { pair, fee_bps } => {
                if *fee_bps > MAX_UNISWAP_V2_FEE_BPS {
                    return Err(EncodingError::InvalidInput(format!(
                        "Uniswap V2 fallback fee is {fee_bps} bps, the fallback router accepts \
                         at most {MAX_UNISWAP_V2_FEE_BPS}"
                    )));
                }
                data.extend_from_slice(bytes_to_address(pair)?.as_slice());
                data.push(*fee_bps);
            }
            FallbackSwapData::UniswapV3 { pool } => {
                data.extend_from_slice(bytes_to_address(pool)?.as_slice());
            }
            FallbackSwapData::UniswapV4 { fee, tick_spacing, hook, hook_data } => {
                if *fee >= 1 << 24 {
                    return Err(EncodingError::InvalidInput(format!(
                        "Uniswap V4 fallback fee {fee} does not fit uint24"
                    )));
                }
                if *tick_spacing < -(1 << 23) || *tick_spacing >= 1 << 23 {
                    return Err(EncodingError::InvalidInput(format!(
                        "Uniswap V4 fallback tick spacing {tick_spacing} does not fit int24"
                    )));
                }
                data.extend_from_slice(&fee.to_be_bytes()[1..]);
                data.extend_from_slice(&tick_spacing.to_be_bytes()[1..]);
                data.extend_from_slice(bytes_to_address(hook)?.as_slice());
                data.extend_from_slice(hook_data.as_ref());
            }
            FallbackSwapData::Curve { pool, pool_type, i, j } => {
                data.extend_from_slice(bytes_to_address(pool)?.as_slice());
                data.extend_from_slice(&[*pool_type, *i, *j]);
            }
            FallbackSwapData::FluidV1 { dex, zero2one } => {
                data.extend_from_slice(bytes_to_address(dex)?.as_slice());
                data.push(u8::from(*zero2one));
            }
            FallbackSwapData::AerodromeV1 { pool } => {
                data.extend_from_slice(bytes_to_address(pool)?.as_slice());
            }
        }
        Ok(data)
    }
}

/// Encodes a swap that runs a pAMM through `TychoFallbackRouter` so a failing pAMM retries on
/// the fallback protocol named in the swap's `user_data` instead of reverting the route.
///
/// The pAMM address comes from the `pamm_address` static attribute of the component.
///
/// # Fields
/// * `executor_address` - The address of the executor contract that will perform the swap.
/// * `chain` - The chain whose `TychoFallbackRouter` runs the swap; a fallback protocol it cannot
///   run ([`FallbackProtocol::supported_on`]) is rejected at encoding time.
/// * `angstrom_hook_address` - The chain's Angstrom hook, from the `fallback` section of
///   `protocol_specific_addresses.json`. Uniswap V4 fallbacks naming this hook are rejected. `None`
///   on a chain without Angstrom, where there is nothing to reject.
#[derive(Clone)]
pub struct FallbackSwapEncoder {
    executor_address: Bytes,
    chain: Chain,
    angstrom_hook_address: Option<Bytes>,
}

impl FallbackSwapEncoder {
    fn pamm_address(swap: &Swap) -> Result<Bytes, EncodingError> {
        let component = swap.component();
        component
            .static_attributes
            .get(PAMM_ADDRESS_ATTRIBUTE)
            .cloned()
            .ok_or_else(|| {
                EncodingError::FatalError(format!(
                    "pAMM component {} is missing the {PAMM_ADDRESS_ATTRIBUTE} static \
                     attribute",
                    component.id
                ))
            })
    }

    /// Rejects a protocol the chain's `TychoFallbackRouter` deploys without.
    fn reject_unsupported(&self, protocol: FallbackProtocol) -> Result<(), EncodingError> {
        if !protocol.supported_on(self.chain) {
            return Err(EncodingError::InvalidInput(format!(
                "Fallback protocol {} is unavailable on {}: the chain's TychoFallbackRouter \
                 deploys without its singleton, so the swap would revert on chain",
                protocol.user_data_name(),
                self.chain
            )));
        }
        Ok(())
    }

    /// Rejects a Uniswap V4 fallback whose hook is the chain's Angstrom hook.
    fn reject_angstrom_hook(&self, data: &FallbackSwapData) -> Result<(), EncodingError> {
        if let FallbackSwapData::UniswapV4 { hook, .. } = data {
            if Some(hook) == self.angstrom_hook_address.as_ref() {
                return Err(EncodingError::InvalidInput(
                    "Angstrom pools are unsupported as a fallback protocol".to_string(),
                ));
            }
        }
        Ok(())
    }
}

impl SwapEncoder for FallbackSwapEncoder {
    fn new(
        executor_address: Bytes,
        chain: Chain,
        config: Option<HashMap<String, String>>,
    ) -> Result<Self, EncodingError> {
        let angstrom_hook_address = config
            .as_ref()
            .and_then(|config| config.get("angstrom_hook_address"))
            .map(|address| {
                Bytes::from_str(address).map_err(|_| {
                    EncodingError::FatalError(format!("Invalid Angstrom hook address {address}"))
                })
            })
            .transpose()?;

        Ok(Self { executor_address, chain, angstrom_hook_address })
    }

    fn encode_swap(
        &self,
        swap: &Swap,
        _encoding_context: &EncodingContext,
    ) -> Result<Vec<u8>, EncodingError> {
        let fallback = FallbackSwap::from_user_data(swap.user_data())?;
        self.reject_unsupported(fallback.protocol)?;
        self.reject_angstrom_hook(&fallback.data)?;
        let pamm = bytes_to_address(&Self::pamm_address(swap)?)?;
        let token_in = bytes_to_address(&swap.token_in().address)?;
        let token_out = bytes_to_address(&swap.token_out().address)?;

        let mut data = (token_in, token_out, pamm).abi_encode_packed();
        data.extend(fallback.encode()?);
        Ok(data)
    }

    fn executor_address(&self) -> &Bytes {
        &self.executor_address
    }

    fn clone_box(&self) -> Box<dyn SwapEncoder> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use alloy::hex::encode;
    use num_bigint::BigUint;
    use tycho_common::models::protocol::ProtocolComponent;

    use super::*;
    use crate::encoding::models::default_token;

    // The addresses below match the Fallback.t.sol fixtures so that test can reuse them.
    const PAMM: &str = "1111111111111111111111111111111111111111";
    const USDC: &str = "a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48";
    const WETH: &str = "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";
    const USDC_WETH_USV3: &str = "88e6a0c2ddd26feeb64f039a2c41296fcb3f5640";

    fn usdc_weth_component() -> ProtocolComponent {
        ProtocolComponent {
            // The id the price level stream produces: pamm ++ token0 ++ token1.
            id: format!("0x{PAMM}{USDC}{WETH}"),
            protocol_system: String::from("fallback:kipseli"),
            static_attributes: HashMap::from([(
                PAMM_ADDRESS_ATTRIBUTE.to_string(),
                Bytes::from(format!("0x{PAMM}").as_str()),
            )]),
            ..Default::default()
        }
    }

    // The mainnet address from the `fallback` section of
    // `config/protocol_specific_addresses.json`.
    const ANGSTROM_HOOK: &str = "0000000aa232009084Bd71A5797d089AA4Edfad4";

    fn encoder() -> FallbackSwapEncoder {
        FallbackSwapEncoder::new(
            Bytes::default(),
            Chain::Ethereum,
            Some(HashMap::from([(
                "angstrom_hook_address".to_string(),
                format!("0x{ANGSTROM_HOOK}"),
            )])),
        )
        .unwrap()
    }

    fn encode_usdc_weth(user_data: Option<&str>) -> Result<String, EncodingError> {
        let token_in = Bytes::from(format!("0x{USDC}").as_str());
        let token_out = Bytes::from(format!("0x{WETH}").as_str());
        let mut swap = Swap::new(
            usdc_weth_component(),
            default_token(token_in.clone()),
            default_token(token_out.clone()),
            BigUint::ZERO,
        );
        if let Some(data) = user_data {
            swap = swap.with_user_data(Bytes::from(data.as_bytes()));
        }
        let encoding_context = EncodingContext {
            router_address: Some(Bytes::zero(20)),
            group_token_in: token_in,
            group_token_out: token_out,
        };

        encoder()
            .encode_swap(&swap, &encoding_context)
            .map(|encoded| encode(&encoded))
    }

    #[test]
    fn test_encode_uniswap_v3_fallback() {
        let hex_swap = encode_usdc_weth(Some(&format!(
            r#"{{"fallback_protocol":"uniswap_v3","pool":"0x{USDC_WETH_USV3}"}}"#
        )))
        .unwrap();

        assert_eq!(hex_swap, format!("{USDC}{WETH}{PAMM}01{USDC_WETH_USV3}"));
    }

    #[test]
    fn test_encode_uniswap_v2_fallback() {
        let pair = "b4e16d0168e52d35cacd2c6185b44281ec28c9dc";
        let hex_swap = encode_usdc_weth(Some(&format!(
            r#"{{"fallback_protocol":"uniswap_v2","pair":"0x{pair}","fee_bps":30}}"#
        )))
        .unwrap();

        assert_eq!(hex_swap, format!("{USDC}{WETH}{PAMM}00{pair}1e"));
    }

    #[test]
    fn test_encode_sushiswap_v2_alias() {
        let pair = "b4e16d0168e52d35cacd2c6185b44281ec28c9dc";
        let hex_swap = encode_usdc_weth(Some(&format!(
            r#"{{"fallback_protocol":"sushiswap_v2","pair":"0x{pair}","fee_bps":30}}"#
        )))
        .unwrap();

        // Byte 00 = UniswapV2: the fork name resolves to the base variant.
        assert_eq!(hex_swap, format!("{USDC}{WETH}{PAMM}00{pair}1e"));
    }

    #[test]
    fn test_encode_pancakeswap_v3_alias() {
        let hex_swap = encode_usdc_weth(Some(&format!(
            r#"{{"fallback_protocol":"pancakeswap_v3","pool":"0x{USDC_WETH_USV3}"}}"#
        )))
        .unwrap();

        // Byte 01 = UniswapV3.
        assert_eq!(hex_swap, format!("{USDC}{WETH}{PAMM}01{USDC_WETH_USV3}"));
    }

    #[test]
    fn test_encode_slipstreams_alias() {
        for fork in SLIPSTREAMS_FORKS {
            let hex_swap = encode_usdc_weth(Some(&format!(
                r#"{{"fallback_protocol":"{fork}","pool":"0x{USDC_WETH_USV3}"}}"#
            )))
            .unwrap();

            assert_eq!(hex_swap, format!("{USDC}{WETH}{PAMM}01{USDC_WETH_USV3}"), "{fork}");
        }
    }

    #[test]
    fn test_encode_curve_by_protocol_system_name() {
        let pool = "3333333333333333333333333333333333333333";
        let hex_swap = encode_usdc_weth(Some(&format!(
            r#"{{"fallback_protocol":"vm:curve","pool":"0x{pool}","pool_type":1,"i":0,"j":2}}"#
        )))
        .unwrap();

        assert_eq!(hex_swap, format!("{USDC}{WETH}{PAMM}03{pool}010002"));
    }

    /// `FallbackProtocol::info` reads the row at the protocol byte, so a row out of order
    /// describes the wrong protocol.
    #[test]
    fn test_protocols_are_in_protocol_byte_order() {
        for (byte, info) in PROTOCOLS.iter().enumerate() {
            assert_eq!(usize::from(info.protocol.protocol_byte()), byte, "{:?}", info.protocol);
        }
    }

    #[test]
    fn test_from_protocol_system() {
        let cases = [
            ("uniswap_v2", Some(FallbackProtocol::UniswapV2)),
            ("quickswap_v2", Some(FallbackProtocol::UniswapV2)),
            ("uniswap_v3", Some(FallbackProtocol::UniswapV3)),
            ("pancakeswap_v3", Some(FallbackProtocol::UniswapV3)),
            ("velodrome_slipstreams", Some(FallbackProtocol::UniswapV3)),
            ("uniswap_v4", Some(FallbackProtocol::UniswapV4)),
            ("uniswap_v4_hooks", Some(FallbackProtocol::UniswapV4)),
            ("curve", Some(FallbackProtocol::Curve)),
            ("vm:curve", Some(FallbackProtocol::Curve)),
            ("fluid_v1", Some(FallbackProtocol::FluidV1)),
            ("aerodrome_v1", Some(FallbackProtocol::AerodromeV1)),
            ("vm:balancer_v2", None),
            ("pricelevelstream:kipseli", None),
        ];
        for (name, expected) in cases {
            assert_eq!(FallbackProtocol::from_protocol_system(name), expected, "{name}");
        }
    }

    /// Every protocol's own name round-trips, so a solver can hand a `FallbackProtocol` back as
    /// the `user_data` tag.
    #[test]
    fn test_user_data_name_round_trips() {
        for protocol in FallbackProtocol::all() {
            assert_eq!(
                FallbackProtocol::from_protocol_system(protocol.user_data_name()),
                Some(protocol)
            );
        }
    }

    /// `FallbackSwap::from_user_data` pairs a protocol with its data through the tag, so serde
    /// must know every `user_data_name` as a `FallbackSwapData` variant.
    #[test]
    fn test_every_user_data_name_is_a_serde_tag() {
        for protocol in FallbackProtocol::all() {
            let tag_only = format!(r#"{{"fallback_protocol":"{}"}}"#, protocol.user_data_name());
            // The data fields are missing, so this fails; it must not fail on the tag.
            if let Err(error) = serde_json::from_str::<FallbackSwapData>(&tag_only) {
                assert!(
                    !error
                        .to_string()
                        .contains("unknown variant"),
                    "{protocol:?}: {error}"
                );
            }
        }
    }

    /// Ethereum has every singleton; Base has no Fluid; Plasma has no Uniswap V4.
    #[test]
    fn test_supported_follows_executor_config() {
        assert_eq!(
            FallbackProtocol::supported(Chain::Ethereum),
            FallbackProtocol::all().collect::<Vec<_>>()
        );
        assert!(!FallbackProtocol::FluidV1.supported_on(Chain::Base));
        assert!(FallbackProtocol::UniswapV4.supported_on(Chain::Base));
        assert!(!FallbackProtocol::UniswapV4.supported_on(Chain::Plasma));
        assert!(FallbackProtocol::FluidV1.supported_on(Chain::Plasma));
        // Per-swap protocols need nothing from the deployment.
        for chain in [Chain::Base, Chain::Plasma, Chain::Unichain] {
            assert!(FallbackProtocol::UniswapV3.supported_on(chain), "{chain}");
            assert!(FallbackProtocol::AerodromeV1.supported_on(chain), "{chain}");
        }
    }

    #[test]
    fn test_rejects_protocol_unavailable_on_chain() {
        let encoder = FallbackSwapEncoder::new(Bytes::default(), Chain::Base, None).unwrap();
        let dex = "4444444444444444444444444444444444444444";
        let token_in = Bytes::from(format!("0x{USDC}").as_str());
        let token_out = Bytes::from(format!("0x{WETH}").as_str());
        let swap = Swap::new(
            usdc_weth_component(),
            default_token(token_in.clone()),
            default_token(token_out.clone()),
            BigUint::ZERO,
        )
        .with_user_data(Bytes::from(
            format!(r#"{{"fallback_protocol":"fluid_v1","dex":"0x{dex}","zero2one":true}}"#)
                .into_bytes(),
        ));
        let encoding_context = EncodingContext {
            router_address: Some(Bytes::zero(20)),
            group_token_in: token_in,
            group_token_out: token_out,
        };

        let err = encoder
            .encode_swap(&swap, &encoding_context)
            .unwrap_err();
        assert!(
            matches!(err, EncodingError::InvalidInput(msg) if msg.contains("fluid_v1") && msg.contains("base"))
        );
    }

    #[test]
    fn test_encode_aerodrome_v1_fallback() {
        let pool = "5555555555555555555555555555555555555555";
        let hex_swap = encode_usdc_weth(Some(&format!(
            r#"{{"fallback_protocol":"aerodrome_v1","pool":"0x{pool}"}}"#
        )))
        .unwrap();

        assert_eq!(hex_swap, format!("{USDC}{WETH}{PAMM}05{pool}"));
    }

    #[test]
    fn test_encode_uniswap_v4_fallback() {
        let hex_swap = encode_usdc_weth(Some(
            r#"{"fallback_protocol":"uniswap_v4","fee":3000,"tick_spacing":-60,
                "hook":"0x2222222222222222222222222222222222222222","hook_data":"0xdeadbeef"}"#,
        ))
        .unwrap();

        // fee 3000 = 0x000bb8; tick spacing -60 = 0xffffc4 in int24 two's complement.
        assert_eq!(
            hex_swap,
            format!(
                "{USDC}{WETH}{PAMM}02000bb8ffffc42222222222222222222222222222222222222222deadbeef"
            )
        );
    }

    #[test]
    fn test_encode_uniswap_v4_fallback_without_hook_data() {
        let hex_swap = encode_usdc_weth(Some(
            r#"{"fallback_protocol":"uniswap_v4","fee":500,"tick_spacing":10,
                "hook":"0x0000000000000000000000000000000000000000"}"#,
        ))
        .unwrap();

        assert_eq!(
            hex_swap,
            format!("{USDC}{WETH}{PAMM}020001f400000a0000000000000000000000000000000000000000")
        );
    }

    #[test]
    fn test_encode_curve_fallback() {
        let pool = "3333333333333333333333333333333333333333";
        let hex_swap = encode_usdc_weth(Some(&format!(
            r#"{{"fallback_protocol":"curve","pool":"0x{pool}","pool_type":1,"i":0,"j":2}}"#
        )))
        .unwrap();

        assert_eq!(hex_swap, format!("{USDC}{WETH}{PAMM}03{pool}010002"));
    }

    #[test]
    fn test_encode_fluid_v1_fallback() {
        let dex = "4444444444444444444444444444444444444444";
        let hex_swap = encode_usdc_weth(Some(&format!(
            r#"{{"fallback_protocol":"fluid_v1","dex":"0x{dex}","zero2one":true}}"#
        )))
        .unwrap();

        assert_eq!(hex_swap, format!("{USDC}{WETH}{PAMM}04{dex}01"));
    }

    #[test]
    fn test_rejects_missing_user_data() {
        let err = encode_usdc_weth(None).unwrap_err();
        assert!(matches!(err, EncodingError::InvalidInput(msg) if msg.contains("user_data")));
    }

    #[test]
    fn test_rejects_unknown_protocol() {
        let err = encode_usdc_weth(Some(r#"{"fallback_protocol":"balancer_v2","pool":"0x11"}"#))
            .unwrap_err();
        assert!(matches!(err, EncodingError::InvalidInput(msg) if msg.contains("balancer_v2")));
    }

    #[test]
    fn test_rejects_user_data_without_a_protocol_name() {
        let err = encode_usdc_weth(Some(r#"{"pool":"0x11"}"#)).unwrap_err();
        assert!(
            matches!(err, EncodingError::InvalidInput(msg) if msg.contains("fallback_protocol"))
        );
    }

    #[test]
    fn test_rejects_pool_shorter_than_an_address() {
        // `Bytes` deserializes any length, so a short pool passes serde and must fail at the
        // address conversion instead.
        let err = encode_usdc_weth(Some(r#"{"fallback_protocol":"uniswap_v3","pool":"0x11"}"#))
            .unwrap_err();
        assert!(matches!(err, EncodingError::InvalidInput(msg) if msg.contains("Invalid address")));
    }

    #[test]
    fn test_rejects_uniswap_v2_fee_above_cap() {
        let pair = "b4e16d0168e52d35cacd2c6185b44281ec28c9dc";
        let err = encode_usdc_weth(Some(&format!(
            r#"{{"fallback_protocol":"uniswap_v2","pair":"0x{pair}","fee_bps":31}}"#
        )))
        .unwrap_err();
        assert!(matches!(err, EncodingError::InvalidInput(msg) if msg.contains("31")));
    }

    #[test]
    fn test_rejects_uniswap_v4_fee_overflowing_uint24() {
        let err = encode_usdc_weth(Some(
            r#"{"fallback_protocol":"uniswap_v4","fee":16777216,"tick_spacing":60,
                "hook":"0x0000000000000000000000000000000000000000"}"#,
        ))
        .unwrap_err();
        assert!(matches!(err, EncodingError::InvalidInput(msg) if msg.contains("uint24")));
    }

    #[test]
    fn test_rejects_uniswap_v4_tick_spacing_overflowing_int24() {
        let err = encode_usdc_weth(Some(
            r#"{"fallback_protocol":"uniswap_v4","fee":500,"tick_spacing":8388608,
                "hook":"0x0000000000000000000000000000000000000000"}"#,
        ))
        .unwrap_err();
        assert!(matches!(err, EncodingError::InvalidInput(msg) if msg.contains("int24")));
    }

    #[test]
    fn test_rejects_component_without_pamm_address() {
        let mut component = usdc_weth_component();
        component.static_attributes.clear();
        let swap = Swap::new(
            component,
            default_token(Bytes::from(format!("0x{USDC}").as_str())),
            default_token(Bytes::from(format!("0x{WETH}").as_str())),
            BigUint::ZERO,
        )
        .with_user_data(Bytes::from(
            format!(r#"{{"fallback_protocol":"uniswap_v3","pool":"0x{USDC_WETH_USV3}"}}"#)
                .into_bytes(),
        ));
        let encoding_context = EncodingContext {
            router_address: Some(Bytes::zero(20)),
            group_token_in: Bytes::from(format!("0x{USDC}").as_str()),
            group_token_out: Bytes::from(format!("0x{WETH}").as_str()),
        };

        let result = encoder().encode_swap(&swap, &encoding_context);
        assert!(
            matches!(result, Err(EncodingError::FatalError(msg)) if msg.contains(PAMM_ADDRESS_ATTRIBUTE))
        );
    }

    #[test]
    fn test_encoder_builds_on_any_chain_without_config() {
        FallbackSwapEncoder::new(Bytes::zero(20), Chain::Base, None).unwrap();
    }

    #[test]
    fn test_encoder_rejects_malformed_angstrom_hook() {
        let config = HashMap::from([("angstrom_hook_address".to_string(), "0xzz".to_string())]);
        let result = FallbackSwapEncoder::new(Bytes::zero(20), Chain::Ethereum, Some(config));
        assert!(matches!(result, Err(EncodingError::FatalError(msg)) if msg.contains("0xzz")));
    }

    fn encode_v4_with_hook(
        encoder: &FallbackSwapEncoder,
        hook: &str,
    ) -> Result<String, EncodingError> {
        let token_in = Bytes::from(format!("0x{USDC}").as_str());
        let token_out = Bytes::from(format!("0x{WETH}").as_str());
        let swap = Swap::new(
            usdc_weth_component(),
            default_token(token_in.clone()),
            default_token(token_out.clone()),
            BigUint::ZERO,
        )
        .with_user_data(Bytes::from(
            format!(
                r#"{{"fallback_protocol":"uniswap_v4","fee":3000,"tick_spacing":60,"hook":"0x{hook}"}}"#
            )
            .into_bytes(),
        ));
        let encoding_context = EncodingContext {
            router_address: Some(Bytes::zero(20)),
            group_token_in: token_in,
            group_token_out: token_out,
        };
        encoder
            .encode_swap(&swap, &encoding_context)
            .map(|encoded| encode(&encoded))
    }

    #[test]
    fn test_angstrom_hook() {
        let err = encode_v4_with_hook(&encoder(), ANGSTROM_HOOK).unwrap_err();
        assert!(matches!(err, EncodingError::InvalidInput(msg) if msg.contains("Angstrom")));
    }

    #[test]
    fn test_non_angstrom_hook() {
        let hook = "2222222222222222222222222222222222222222";
        let hex_swap = encode_v4_with_hook(&encoder(), hook).unwrap();
        // fee 3000 = 0x000bb8; tick spacing 60 = 0x00003c.
        assert_eq!(hex_swap, format!("{USDC}{WETH}{PAMM}02000bb800003c{hook}"));
    }

    /// A chain without Angstrom configures no hook, so no hook is rejected.
    #[test]
    fn test_no_angstrom_hook_configured_accepts_any_hook() {
        let encoder = FallbackSwapEncoder::new(Bytes::default(), Chain::Base, None).unwrap();
        let hex_swap = encode_v4_with_hook(&encoder, ANGSTROM_HOOK).unwrap();
        assert_eq!(
            hex_swap,
            format!("{USDC}{WETH}{PAMM}02000bb800003c{}", ANGSTROM_HOOK.to_lowercase())
        );
    }
}
