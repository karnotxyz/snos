//! Block Processing Module
//!
//! This module provides functionality for processing individual Starknet blocks
//! and extracting all necessary information for PIE (Program Input/Output) generation.
//!
//! The main entry point is [`collect_single_block_info`] which orchestrates the entire
//! block processing pipeline, from fetching block data to constructing the final OS input.

use crate::error::BlockProcessingError;
use crate::state_update::ContractClassProcessingResult;
use crate::types::{BlockData, CommitmentCalculationResult, TransactionProcessingResult};
use blockifier::context::BlockContext;
use cairo_lang_starknet_classes::casm_contract_class::CasmContractClass;
use log::info;
use rpc_client::RpcClient;
use starknet::core::types::BlockId;
use starknet_api::block::BlockHash;
use starknet_api::core::{ClassHash, CompiledClassHash};
use starknet_api::deprecated_contract_class::ContractClass;
use starknet_os::io::os_input::OsBlockInput;
use starknet_types_core::felt::Felt;
use std::collections::BTreeMap;
// ================================================================================================
// Type Definitions
// ================================================================================================

/// Result containing all the information collected from a single block.
#[derive(Debug)]
pub struct BlockInfoResult {
    /// The OS block input for the block.
    pub os_block_input: OsBlockInput,
    /// Compiled classes used in the block.
    pub compiled_classes: BTreeMap<CompiledClassHash, CasmContractClass>,
    /// Deprecated compiled classes used in the block.
    pub deprecated_compiled_classes: BTreeMap<ClassHash, ContractClass>,
}

// ================================================================================================
// Public API
// ================================================================================================

/// Collects all necessary information from a single block for PIE generation.
///
/// This function processes a single block and extracts all the information needed
/// to generate a Cairo PIE, including transaction execution, state updates, proofs,
/// and contract classes.
///
/// # Arguments
///
/// * `block_number` - The block number to process
/// * `chain_config` - Chain mode and fee-token addresses
/// * `versioned_constants` - Optional versioned constants to use instead of auto-detecting
/// * `rpc_client` - The RPC client for fetching block data
/// * `committed_data_witnesses` - Immutable, prevalidated private committed_data data for replay
///
/// # Returns
///
/// Returns a `BlockProcessingResult` containing all the collected block information
/// or an error if any step of the processing fails.
///
/// # Errors
///
/// This function can return various errors including
/// - `BlockProcessingError::RpcClient` for RPC communication errors
/// - `BlockProcessingError::TransactionExecution` for transaction execution errors
/// - `BlockProcessingError::StateUpdateProcessing` for state update processing errors
/// - `BlockProcessingError::StorageProof` for storage proof errors
/// - `BlockProcessingError::ClassProof` for class proof errors
/// - `BlockProcessingError::ContractClassConversion` for contract class conversion errors
///
pub async fn collect_single_block_info(
    block_number: u64,
    chain_config: &crate::types::ChainConfig,
    versioned_constants: Option<blockifier::blockifier_versioned_constants::VersionedConstants>,
    rpc_client: RpcClient,
    committed_data_witnesses: std::sync::Arc<blockifier::execution::syscalls::committed_data::CommittedDataWitnesses>,
    committed_data_activation_block: Option<u64>,
    committed_data_readers: starknet_api::committed_data::CommittedDataReaders,
) -> Result<BlockInfoResult, BlockProcessingError> {
    info!("Starting block info collection for block {}", block_number);

    // Step 1: Fetch all required block data
    let block_data = BlockData::fetch(block_number, &rpc_client).await?;

    // Step 2: Build block context (only once, reused throughout)
    let mut block_context = block_data
        .build_context(
            chain_config.is_l3,
            &chain_config.strk_fee_token_address,
            &chain_config.eth_fee_token_address,
            versioned_constants,
        )
        .map_err(BlockProcessingError::ContextBuilding)?;

    block_context.committed_data_witnesses = committed_data_witnesses;
    block_context.committed_data_activation_block = committed_data_activation_block;
    block_context.committed_data_readers = committed_data_readers;

    // Step 3: Process transactions and extract execution information
    let tx_result = block_data.process_transactions(block_number, &rpc_client, &block_context).await?;

    // Step 4: Collect storage and class proofs
    let proofs = tx_result.collect_proofs(block_number, &rpc_client).await?;

    // Step 5: Calculate commitment information
    let block_id = BlockId::Number(block_number);
    let commitment_result = proofs.calculate_commitments(block_id).await?;

    // Step 6: Process contract classes
    let class_result = tx_result.processed_state_update.process_contract_classes()?;

    // Step 7: Extract values we need before moving ownership
    let compiled_classes = class_result.compiled_classes.clone();
    let deprecated_compiled_classes = class_result.deprecated_compiled_classes.clone();

    // Step 7b: Extract migrated compiled classes (SNIP-34).
    let mut class_hashes_to_migrate = tx_result.class_hashes_to_migrate.clone();
    class_hashes_to_migrate.sort_by_key(|(class_hash, _)| class_hash.0);

    // Step 8: Build final OS block input
    let os_block_input = build_os_block_input(
        &block_data,
        tx_result,
        commitment_result,
        class_result,
        &block_context,
        class_hashes_to_migrate,
    )?;

    info!("Successfully completed construction of OsBlockInput for block {}", block_number);

    Ok(BlockInfoResult { os_block_input, compiled_classes, deprecated_compiled_classes })
}

// ================================================================================================
// Private Helper Functions
// ================================================================================================

/// Builds the final OS block input from all processed data.
fn build_os_block_input(
    block_data: &BlockData,
    tx_result: TransactionProcessingResult,
    commitment_result: CommitmentCalculationResult,
    class_result: ContractClassProcessingResult,
    block_context: &BlockContext,
    class_hashes_to_migrate: Vec<(ClassHash, CompiledClassHash)>,
) -> Result<OsBlockInput, BlockProcessingError> {
    info!("Building OS block input");

    Ok(OsBlockInput {
        contract_state_commitment_info: commitment_result.contract_state_commitment_info,
        contract_class_commitment_info: commitment_result.contract_class_commitment_info,
        address_to_storage_commitment_info: commitment_result.address_to_storage_commitment_info,
        transactions: tx_result.starknet_api_txns,
        tx_execution_infos: tx_result.central_txn_execution_infos,
        declared_class_hash_to_component_hashes: class_result.declared_class_hash_component_hashes,
        block_info: block_context.block_info().clone(),
        block_hash_commitments: tx_result.block_hash_commitments,
        prev_block_hash: block_data
            .previous_block
            .as_ref()
            .map(|prev_block| BlockHash(prev_block.block_hash))
            .unwrap_or(BlockHash(Felt::ZERO)),
        new_block_hash: BlockHash(block_data.current_block.block_hash),
        old_block_number_and_hash: block_data.os_old_block_number_and_hash()?,
        class_hashes_to_migrate,
        initial_reads: tx_result.initial_reads,
    })
}
