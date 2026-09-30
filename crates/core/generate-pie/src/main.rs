//! Main entry point for the generate-pie application.
//!
//! This binary demonstrates how to use the generate-pie library to generate
//! Cairo PIE files from Starknet blocks.

use cairo_vm::types::layout_name::LayoutName;
use clap::Parser;
use generate_pie::constants::{DEFAULT_SEPOLIA_ETH_FEE_TOKEN, DEFAULT_SEPOLIA_STRK_FEE_TOKEN};
use generate_pie::types::{ChainConfig, OsHintsConfiguration, PieGenerationInput};
use generate_pie::utils::load_versioned_constants;
use generate_pie::{generate_pie, parse_layout, parse_public_key};
use log::{error, info};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(name = "snos")]
#[command(about = "SNOS - Starknet OS for block processing")]
struct Cli {
    /// Read the complete PieGenerationInput JSON from stdin, including private witnesses.
    /// Intended for orchestrators; avoids temporary files and command-line size limits.
    #[arg(long, conflicts_with_all = ["blocks", "rpc_url", "committed_data_witnesses_path", "committed_data_activation_block", "committed_data_readers", "committed_data_rpc_url", "layout", "chain", "strk_fee_token_address", "eth_fee_token_address", "is_l3", "output", "versioned_constants_path", "public_keys"])]
    input_stdin: bool,

    /// Inclusive extension activation height; must match the accepted OS configuration.
    #[arg(long, env = "SNOS_COMMITTED_DATA_ACTIVATION_BLOCK")]
    committed_data_activation_block: Option<u64>,

    /// Comma-separated approved adapter storage addresses; must match the proved chain policy.
    #[arg(long, default_value = "")]
    committed_data_readers: starknet_api::committed_data::CommittedDataReaders,

    /// Operator-configured Madara admin RPC for authenticated witnesses.
    #[arg(long, env = "SNOS_COMMITTED_DATA_RPC_URL")]
    committed_data_rpc_url: Option<String>,

    /// Block number(s) to process
    #[arg(short, long, value_delimiter = ',', required_unless_present = "input_stdin", env = "SNOS_BLOCKS")]
    blocks: Vec<u64>,

    /// RPC URL to connect to
    #[arg(short, long, required_unless_present = "input_stdin", env = "SNOS_RPC_URL")]
    rpc_url: Option<String>,

    /// Layout to be used for SNOS
    #[arg(short, long, default_value = "all_cairo", value_parser=parse_layout, env = "SNOS_LAYOUT")]
    layout: LayoutName,

    /// Chain configuration (defaults to Sepolia)
    #[arg(long, env = "SNOS_NETWORK", default_value = "sepolia")]
    chain: String,

    /// STRK fee token address
    #[arg(short, long, default_value = DEFAULT_SEPOLIA_STRK_FEE_TOKEN, env = "SNOS_STRK_FEE_TOKEN_ADDRESS")]
    strk_fee_token_address: String,

    /// ETH fee token address
    #[arg(short, long, default_value = DEFAULT_SEPOLIA_ETH_FEE_TOKEN, env = "SNOS_ETH_FEE_TOKEN_ADDRESS")]
    eth_fee_token_address: String,

    /// Is L3
    #[arg(short, long, default_value = "false", env = "SNOS_IS_L3")]
    is_l3: bool,

    /// Output path for the PIE file
    #[arg(short, long, env = "SNOS_OUTPUT")]
    output: Option<String>,

    /// Path to a JSON file containing versioned constants (optional)
    #[arg(long, env = "SNOS_VERSIONED_CONSTANTS_PATH")]
    versioned_constants_path: Option<String>,

    /// Private committed_data witnesses (JSON array); the root must already be in contract state.
    #[arg(long, env = "SNOS_COMMITTED_DATA_WITNESSES_PATH")]
    committed_data_witnesses_path: Option<std::path::PathBuf>,

    /// Public keys for OS execution (comma-separated hex values)
    #[arg(long, value_delimiter = ',', value_parser = parse_public_key, env = "SNOS_PUBLIC_KEYS")]
    public_keys: Option<Vec<starknet_types_core::felt::Felt>>,
}
/// Main entry point for the generate-pie application.
///
/// This function demonstrates the usage of the generate-pie library by:
/// 1. Initializing logging
/// 2. Creating a configuration for PIE generation
/// 3. Calling the core PIE generation function
/// 4. Handling the results and errors appropriately
///
/// # Returns
///
/// Returns `Ok(())` if the PIE generation completes successfully, or an error
/// if any step of the process fails.
///
/// # Errors
///
/// This function can return various errors including
/// - Configuration validation errors
/// - RPC client connection errors
/// - Block processing errors
/// - OS execution errors
/// - File I/O errors
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize logging
    env_logger::init();

    let cli = Cli::parse();

    info!("Starting SNOS PIE generation application");

    let input = if cli.input_stdin {
        generate_pie::read_pie_input(std::io::stdin().lock())?
    } else {
        let versioned_constants = load_versioned_constants(cli.versioned_constants_path.as_deref())?;
        let mut os_hints_config = OsHintsConfiguration::default_with_is_l3(cli.is_l3);
        os_hints_config.committed_data_activation_block = cli.committed_data_activation_block;
        os_hints_config.committed_data_readers = cli.committed_data_readers;
        if let Some(path) = &cli.committed_data_witnesses_path {
            // Keep the standalone CLI convenience; production callers can supply structured input.
            os_hints_config.committed_data_witnesses =
                generate_pie::read_committed_data_witnesses(std::fs::File::open(path)?)?;
        }
        PieGenerationInput {
            rpc_url: cli.rpc_url.ok_or("Missing RPC URL")?,
            committed_data_rpc_url: cli.committed_data_rpc_url,
            blocks: cli.blocks,
            chain_config: ChainConfig::new(
                &cli.chain,
                &cli.strk_fee_token_address,
                &cli.eth_fee_token_address,
                cli.is_l3,
            ),
            os_hints_config,
            output_path: cli.output,
            layout: cli.layout,
            versioned_constants,
            public_keys: cli.public_keys,
        }
    };
    input.validate()?;

    // Display configuration information
    info!("Configuration:");
    // RPC URLs may contain credentials; do not log them.
    info!("  Blocks: {:?}", input.blocks);
    info!("  Chain ID: {:?}", input.chain_config.chain_id);
    info!("  STRK Fee Token: {:?}", input.chain_config.strk_fee_token_address);
    info!("  ETH Fee Token: {:?}", input.chain_config.eth_fee_token_address);
    info!("  Layout: {:?}", input.layout);
    info!("  Is L3: {}", input.chain_config.is_l3);
    info!("  Debug mode: {}", input.os_hints_config.debug_mode);
    info!("  Full Output: {}", input.os_hints_config.full_output);
    info!("  Use KZG DA: {}", input.os_hints_config.use_kzg_da);
    info!("  Output path: {:?}", input.output_path);
    info!(
        "  Versioned constants: {}",
        if input.versioned_constants.is_some() { "provided from file" } else { "auto-detect from block" }
    );
    if let Some(ref public_keys) = input.public_keys {
        info!("  Public keys: {} provided", public_keys.len());
    } else {
        info!("  Public keys: none");
    }

    // Call the core PIE generation function
    match generate_pie(input).await {
        Ok(result) => {
            info!("PIE generation completed successfully!");
            info!("  Blocks processed: {:?}", result.blocks_processed);
            if let Some(output_path) = result.output_path {
                info!("  Output written to: {}", output_path);
            }
        }
        Err(e) => {
            error!("PIE generation failed: {}", e);
            return Err(e.into());
        }
    }

    info!("SNOS execution completed successfully!");
    Ok(())
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    #[test]
    fn structured_input_has_no_ambiguous_cli_overrides() {
        assert!(Cli::try_parse_from(["generate-pie", "--input-stdin"]).is_ok());
        for flag in ["--chain", "--layout", "--rpc-url", "--committed-data-rpc-url", "--committed-data-readers"] {
            assert!(Cli::try_parse_from(["generate-pie", "--input-stdin", flag, "ignored"]).is_err());
        }
    }
}
