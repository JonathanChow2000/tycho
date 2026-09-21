require('dotenv').config();
const hre = require("hardhat");
const {deployCreate2} = require("./utils");

// TychoFallbackRouter calls three per-chain singletons directly: the PoolManager
// for the Uniswap V4 protocol, the Fluid liquidity layer for what dexCallback
// pays, and the Uniswap V3 static quoter that prices a V3 fallback. The first
// two already live in the executor config, keyed by protocol; the quoter is not
// an executor argument, so it lives under `fallback_router` in the
// protocol-specific config. A chain without one deploys with address(0) there:
// the protocol then reverts TychoFallbackRouter__ProtocolUnavailable, or for
// the quoter, Uniswap V3 fallbacks are quoted by simulation.
//
// fallback_protocols.json lists the protocols each chain's router runs; the
// Rust encoder reads it to reject a protocol before it reverts on chain. This
// script refuses to deploy a router that disagrees with it: a listed Uniswap V4
// or Fluid V1 needs its singleton, and a found singleton needs its listing.
//
// The FallbackExecutor is deployed separately by deploy-executors.js: add a
// `fallback` entry with the address this script prints to
// executor_deployments.json, then list `fallback` under the chain there.
const executorDeployments = require("../../config/executor_deployments.json");
const protocolSpecific = require("../../config/protocol_specific_addresses.json");
const fallbackProtocols = require("../../config/fallback_protocols.json");

const ZERO_ADDRESS = "0x0000000000000000000000000000000000000000";

async function main() {
    const network = hre.network.name;
    // Strip tenderly_ to match the executor_deployments.json keys.
    const base = network.replace(/^tenderly_/, "");

    const deployments = executorDeployments[base];
    if (!deployments) {
        throw new Error(
            `No executor deployments configured for network '${base}' in ` +
            "executor_deployments.json"
        );
    }
    const listed = fallbackProtocols[base];
    if (!listed) {
        throw new Error(
            `No fallback protocols listed for network '${base}' in ` +
            "fallback_protocols.json"
        );
    }
    const poolManager = deployments.uniswap_v4?.args?.[0] ?? ZERO_ADDRESS;
    const fluidLiquidity = deployments.fluid_v1?.args?.[0] ?? ZERO_ADDRESS;
    const staticQuoter =
        protocolSpecific[base]?.fallback_router?.uniswap_v3_static_quoter ??
        ZERO_ADDRESS;
    requireListingMatchesSingleton(listed, "uniswap_v4", poolManager);
    requireListingMatchesSingleton(listed, "fluid_v1", fluidLiquidity);

    console.log(`Deploying TychoFallbackRouter to ${network} with:`);
    console.log(
        `- poolManager: ${describe(poolManager, "Uniswap V4 disabled")}`
    );
    console.log(
        `- fluidLiquidity: ${describe(fluidLiquidity, "Fluid V1 disabled")}`
    );
    console.log(
        `- uniswapV3StaticQuoter: ${describe(
            staticQuoter,
            "Uniswap V3 quoted by simulation"
        )}`
    );

    await deployCreate2({
        contractName: "TychoFallbackRouter",
        contractFqn: "src/fallback/TychoFallbackRouter.sol:TychoFallbackRouter",
        args: [poolManager, fluidLiquidity, staticQuoter],
        network,
    });
}

function requireListingMatchesSingleton(listed, protocol, singleton) {
    const isListed = listed.includes(protocol);
    const hasSingleton = singleton !== ZERO_ADDRESS;
    if (isListed && !hasSingleton) {
        throw new Error(
            `fallback_protocols.json lists ${protocol} for this network, but ` +
            `executor_deployments.json has no ${protocol} entry to read its ` +
            "singleton from"
        );
    }
    if (!isListed && hasSingleton) {
        throw new Error(
            `executor_deployments.json has a ${protocol} singleton for this ` +
            `network, but fallback_protocols.json does not list ${protocol}`
        );
    }
}

function describe(address, whenZero) {
    return address === ZERO_ADDRESS
        ? `${address} (${whenZero}: not configured for this network)`
        : address;
}

main()
    .then(() => process.exit(0))
    .catch((error) => {
        console.error("Deployment failed:", error);
        process.exit(1);
    });
