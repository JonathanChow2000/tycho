require('dotenv').config();
const hre = require("hardhat");
const {deployCreate2} = require("./utils");

// TychoFallbackRouter takes three per-chain singletons: Uniswap V4's PoolManager,
// Fluid's liquidity layer and the Uniswap V3 static quoter. The first two come
// from the chain's executor deployments, the quoter from `fallback_router` in
// the protocol-specific config. A missing one is deployed as address(0): the
// protocol reverts ProtocolUnavailable, or Uniswap V3 is quoted by simulation.
// `SUPPORTED_PROTOCOLS` in the Rust encoder must agree with what this deploys;
// its tests check that against executor_deployments.json.
//
// Then deploy the FallbackExecutor with deploy-executors.js: add a `fallback`
// entry with the printed address to executor_deployments.json.
const executorDeployments = require("../../config/executor_deployments.json");
const protocolSpecific = require("../../config/protocol_specific_addresses.json");

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
    const poolManager = deployments.uniswap_v4?.args?.[0] ?? ZERO_ADDRESS;
    const fluidLiquidity = deployments.fluid_v1?.args?.[0] ?? ZERO_ADDRESS;
    const staticQuoter =
        protocolSpecific[base]?.fallback_router?.uniswap_v3_static_quoter ??
        ZERO_ADDRESS;

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
