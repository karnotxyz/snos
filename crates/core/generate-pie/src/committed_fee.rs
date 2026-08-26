use std::collections::HashMap;
use std::sync::Arc;

use blockifier::blockifier::transaction_executor::TransactionExecutor;
use blockifier::execution::call_info::CallInfo;
use blockifier::fee::fee_utils::get_address_balance_keys;
use blockifier::state::state_api::{State, StateReader};
use blockifier::transaction::objects::{HasRelatedFeeType, TransactionExecutionInfo};
use blockifier::transaction::transaction_execution::Transaction;
use log::{info, warn};
use num_traits::ToPrimitive;
use starknet::core::types::{Event, TransactionReceipt};
use starknet_api::abi::abi_utils::selector_from_name;
use starknet_api::core::ContractAddress;
use starknet_api::execution_resources::GasVector;
use starknet_api::transaction::constants::TRANSFER_EVENT_NAME;
use starknet_api::transaction::fields::Fee;
use starknet_types_core::felt::Felt;

use crate::conversions::{transaction_receipt_fee, transaction_receipt_gas, TransactionConversionResult};
use crate::error::BlockProcessingError;

pub(crate) const USE_COMMITTED_FEES_ENV: &str = "SNOS_REPLAY_USE_COMMITTED_FEES";

pub(crate) fn is_enabled() -> bool {
    match std::env::var(USE_COMMITTED_FEES_ENV) {
        Ok(value) if matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes") => true,
        Ok(value) if matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "no") => false,
        Ok(value) => {
            warn!("Ignoring invalid {USE_COMMITTED_FEES_ENV}={value:?}; expected true or false");
            false
        }
        Err(_) => false,
    }
}

/// Re-executes transactions sequentially while reproducing receipt and fee-transfer data already
/// committed by a historical non-standard executor.
///
/// This mode is intentionally opt-in. Normal SNOS execution must continue deriving fees from
/// Blockifier. When enabled, each transaction is first executed normally. The receipt's
/// `actual_fee` and gas vector are retained for block-hash calculation, while the fee token's
/// committed `Transfer` event determines what was actually charged. These values can differ on
/// historical blocks. The fee-transfer trace and cached fee-token balances are reconciled to the
/// latter before the next transaction executes.
pub(crate) fn execute_with_committed_fees<S: StateReader + Send>(
    txn_executor: &mut TransactionExecutor<S>,
    transactions: &[TransactionConversionResult],
    receipts: &HashMap<Felt, TransactionReceipt>,
) -> Result<Vec<TransactionExecutionInfo>, BlockProcessingError> {
    info!("{} is enabled; replaying account fee transfers from committed receipts", USE_COMMITTED_FEES_ENV);

    let mut execution_infos = Vec::with_capacity(transactions.len());
    for transaction in transactions {
        let tx_hash = Transaction::tx_hash(&transaction.blockifier_tx).0;
        let receipt = receipts.get(&tx_hash).ok_or_else(|| BlockProcessingError::CommittedFeeReplay {
            tx_hash,
            reason: "transaction receipt is missing".to_string(),
        })?;
        let committed_fee = transaction_receipt_fee(receipt)
            .map_err(|error| BlockProcessingError::CommittedFeeReplay { tx_hash, reason: error.to_string() })?;
        let committed_gas = transaction_receipt_gas(receipt);

        let output = txn_executor
            .execute_txs_sequentially(std::slice::from_ref(&transaction.blockifier_tx), None)
            .into_iter()
            .next()
            .ok_or_else(|| BlockProcessingError::CommittedFeeReplay {
                tx_hash,
                reason: "Blockifier returned no execution result".to_string(),
            })??;
        let (mut execution_info, _) = output;

        let committed_charge = match &transaction.blockifier_tx {
            Transaction::Account(account_transaction) => {
                let fee_token_address =
                    txn_executor.block_context.chain_info().fee_token_address(&account_transaction.fee_type());
                committed_fee_transfer_amount(
                    receipt.events(),
                    fee_token_address,
                    account_transaction.sender_address(),
                    txn_executor.block_context.block_info().sequencer_address,
                )
                .map_err(|reason| BlockProcessingError::CommittedFeeReplay { tx_hash, reason })?
            }
            _ => committed_fee,
        };

        reconcile_transaction_receipt(
            txn_executor,
            &transaction.blockifier_tx,
            tx_hash,
            &mut execution_info,
            committed_fee,
            committed_charge,
            committed_gas,
        )?;
        execution_infos.push(execution_info);
    }

    Ok(execution_infos)
}

fn reconcile_transaction_receipt<S: StateReader>(
    txn_executor: &mut TransactionExecutor<S>,
    transaction: &Transaction,
    tx_hash: Felt,
    execution_info: &mut TransactionExecutionInfo,
    committed_fee: Fee,
    committed_charge: Fee,
    committed_gas: GasVector,
) -> Result<(), BlockProcessingError> {
    let calculated_fee = execution_info.receipt.fee;
    let calculated_gas = execution_info.receipt.gas;
    if calculated_fee != committed_charge {
        let Transaction::Account(account_transaction) = transaction else {
            return Err(BlockProcessingError::CommittedFeeReplay {
                tx_hash,
                reason: format!(
                    "non-account transaction charge differs: calculated={}, committed={}",
                    calculated_fee.0, committed_charge.0
                ),
            });
        };

        let fee_transfer_call_info =
            execution_info.fee_transfer_call_info.as_mut().ok_or_else(|| BlockProcessingError::CommittedFeeReplay {
                tx_hash,
                reason: format!(
                    "fee transfer is absent while calculated charge {} differs from committed charge {}",
                    calculated_fee.0, committed_charge.0
                ),
            })?;
        replace_fee_in_transfer_trace(fee_transfer_call_info, calculated_fee, committed_charge)
            .map_err(|reason| BlockProcessingError::CommittedFeeReplay { tx_hash, reason })?;

        let sender_address = account_transaction.sender_address();
        let sequencer_address = txn_executor.block_context.block_info().sequencer_address;
        if sender_address != sequencer_address {
            let fee_token_address =
                txn_executor.block_context.chain_info().fee_token_address(&account_transaction.fee_type());
            let block_state =
                txn_executor.block_state.as_mut().ok_or(BlockProcessingError::MissingBlockStateAfterExecution)?;
            reconcile_fee_token_balances(
                block_state,
                fee_token_address,
                sender_address,
                sequencer_address,
                calculated_fee,
                committed_charge,
            )
            .map_err(|reason| BlockProcessingError::CommittedFeeReplay { tx_hash, reason })?;
        }
    }

    if calculated_fee != committed_charge || committed_fee != committed_charge || calculated_gas != committed_gas {
        info!(
            "Reconciled transaction {tx_hash:#x}: fee Blockifier={} receipt={} charged={}; gas Blockifier={calculated_gas:?} committed={committed_gas:?}",
            calculated_fee.0, committed_fee.0, committed_charge.0
        );
    }
    // `CentralTransactionExecutionInfo.actual_fee` is loaded directly by Cairo's `charge_fee`
    // hint. It therefore has to describe the amount actually transferred, which is not always the
    // receipt's `actual_fee` on historical non-standard blocks. The receipt fee is supplied only
    // to block-hash calculation from the RPC receipt.
    execution_info.receipt.fee = committed_charge;
    execution_info.receipt.gas = committed_gas;
    Ok(())
}

fn committed_fee_transfer_amount(
    events: &[Event],
    fee_token_address: ContractAddress,
    sender_address: ContractAddress,
    sequencer_address: ContractAddress,
) -> Result<Fee, String> {
    let transfer_selector = selector_from_name(TRANSFER_EVENT_NAME).0;
    let fee_token_address = *fee_token_address.0.key();
    let sender_address = *sender_address.0.key();
    let sequencer_address = *sequencer_address.0.key();
    let matches: Vec<_> = events
        .iter()
        .filter(|event| {
            event.from_address == fee_token_address
                && event.keys.as_slice() == [transfer_selector]
                && event.data.len() == 4
                && event.data[0] == sender_address
                && event.data[1] == sequencer_address
                && event.data[3] == Felt::ZERO
        })
        .collect();

    let [event] = matches.as_slice() else {
        return Err(format!(
            "receipt contained {} matching fee-transfer events from {sender_address:#x} to {sequencer_address:#x}; expected exactly 1",
            matches.len()
        ));
    };
    Ok(Fee(felt_limb_to_u128(event.data[2])?))
}

fn replace_fee_in_transfer_trace(
    call_info: &mut CallInfo,
    calculated_fee: Fee,
    committed_fee: Fee,
) -> Result<(), String> {
    let calldata = Arc::make_mut(&mut call_info.call.calldata.0);
    if calldata.len() != 3 {
        return Err(format!("fee-transfer calldata has {} fields, expected 3", calldata.len()));
    }
    if calldata[1] != Felt::from(calculated_fee.0) || calldata[2] != Felt::ZERO {
        return Err(format!(
            "fee-transfer calldata does not contain the calculated fee {}; got [{:#x}, {:#x}]",
            calculated_fee.0, calldata[1], calldata[2]
        ));
    }
    calldata[1] = Felt::from(committed_fee.0);
    calldata[2] = Felt::ZERO;

    let replaced_events = replace_fee_in_transfer_events(
        call_info,
        selector_from_name(TRANSFER_EVENT_NAME).0,
        Felt::from(calculated_fee.0),
        Felt::from(committed_fee.0),
    );
    if replaced_events != 1 {
        return Err(format!(
            "fee-transfer trace contained {replaced_events} events with calculated fee {}; expected exactly 1",
            calculated_fee.0
        ));
    }

    Ok(())
}

fn replace_fee_in_transfer_events(
    call_info: &mut CallInfo,
    transfer_event_selector: Felt,
    calculated_fee: Felt,
    committed_fee: Felt,
) -> usize {
    let mut replaced = 0;

    for event in &mut call_info.execution.events {
        let event_data = &mut event.event.data.0;
        if event.event.keys.len() == 1
            && event.event.keys[0].0 == transfer_event_selector
            && event_data.len() == 4
            && event_data[2] == calculated_fee
            && event_data[3] == Felt::ZERO
        {
            event_data[2] = committed_fee;
            replaced += 1;
        }
    }
    for inner_call in &mut call_info.inner_calls {
        replaced += replace_fee_in_transfer_events(inner_call, transfer_event_selector, calculated_fee, committed_fee);
    }

    replaced
}

fn reconcile_fee_token_balances(
    state: &mut impl State,
    fee_token_address: ContractAddress,
    sender_address: ContractAddress,
    sequencer_address: ContractAddress,
    calculated_fee: Fee,
    committed_fee: Fee,
) -> Result<(), String> {
    let (difference, sender_increases) = if calculated_fee.0 >= committed_fee.0 {
        (calculated_fee.0 - committed_fee.0, true)
    } else {
        (committed_fee.0 - calculated_fee.0, false)
    };
    if difference == 0 {
        return Ok(());
    }

    adjust_balance(state, fee_token_address, sender_address, difference, sender_increases)?;
    adjust_balance(state, fee_token_address, sequencer_address, difference, !sender_increases)?;
    Ok(())
}

fn adjust_balance(
    state: &mut impl State,
    fee_token_address: ContractAddress,
    owner: ContractAddress,
    amount: u128,
    increase: bool,
) -> Result<(), String> {
    let (low_key, high_key) = get_address_balance_keys(owner);
    let (low, high) = state
        .get_fee_token_balance(owner, fee_token_address)
        .map_err(|error| format!("failed to read fee-token balance for {owner:?}: {error}"))?;
    let low = felt_limb_to_u128(low)?;
    let high = felt_limb_to_u128(high)?;
    let (new_low, new_high) = adjust_u256_limbs(low, high, amount, increase)?;

    state
        .set_storage_at(fee_token_address, low_key, Felt::from(new_low))
        .map_err(|error| format!("failed to write low fee-token balance for {owner:?}: {error}"))?;
    state
        .set_storage_at(fee_token_address, high_key, Felt::from(new_high))
        .map_err(|error| format!("failed to write high fee-token balance for {owner:?}: {error}"))?;
    Ok(())
}

fn felt_limb_to_u128(value: Felt) -> Result<u128, String> {
    value.to_u128().ok_or_else(|| format!("fee-token balance limb exceeds u128: {value:#x}"))
}

fn adjust_u256_limbs(low: u128, high: u128, amount: u128, increase: bool) -> Result<(u128, u128), String> {
    if increase {
        let (new_low, carry) = low.overflowing_add(amount);
        let new_high =
            high.checked_add(u128::from(carry)).ok_or_else(|| "fee-token balance overflowed u256".to_string())?;
        Ok((new_low, new_high))
    } else {
        let (new_low, borrow) = low.overflowing_sub(amount);
        let new_high =
            high.checked_sub(u128::from(borrow)).ok_or_else(|| "fee-token balance underflowed u256".to_string())?;
        Ok((new_low, new_high))
    }
}

#[cfg(test)]
mod tests {
    use blockifier::execution::call_info::OrderedEvent;
    use starknet_api::transaction::{EventContent, EventData, EventKey};

    use super::*;

    #[test]
    fn replaces_fee_event_in_nested_transfer_call() {
        let calculated_fee = Felt::from(17_u64);
        let committed_fee = Felt::from(11_u64);
        let fee_event = OrderedEvent {
            order: 1,
            event: EventContent {
                keys: vec![EventKey(selector_from_name(TRANSFER_EVENT_NAME).0)],
                data: EventData(vec![Felt::ONE, Felt::TWO, calculated_fee, Felt::ZERO]),
            },
        };
        let mut call_info = CallInfo { inner_calls: vec![CallInfo::default()], ..Default::default() };
        call_info.inner_calls[0].execution.events.push(fee_event);

        assert_eq!(
            replace_fee_in_transfer_events(
                &mut call_info,
                selector_from_name(TRANSFER_EVENT_NAME).0,
                calculated_fee,
                committed_fee,
            ),
            1
        );
        assert_eq!(call_info.inner_calls[0].execution.events[0].event.data.0[2], committed_fee);
    }

    #[test]
    fn reads_historical_charge_from_receipt_transfer_event() {
        let fee_token = ContractAddress::try_from(Felt::from(3_u8)).unwrap();
        let sender = ContractAddress::try_from(Felt::from(5_u8)).unwrap();
        let sequencer = ContractAddress::try_from(Felt::from(7_u8)).unwrap();
        let charged_fee = Felt::from(11_u8);
        let events = vec![Event {
            from_address: Felt::from(fee_token),
            keys: vec![selector_from_name(TRANSFER_EVENT_NAME).0],
            data: vec![Felt::from(sender), Felt::from(sequencer), charged_fee, Felt::ZERO],
        }];

        assert_eq!(committed_fee_transfer_amount(&events, fee_token, sender, sequencer).unwrap(), Fee(11));
    }

    #[test]
    fn adjusts_u256_limbs_and_rejects_underflow() {
        assert_eq!(adjust_u256_limbs(u128::MAX, 7, 2, true).unwrap(), (1, 8));
        assert_eq!(adjust_u256_limbs(1, 8, 2, false).unwrap(), (u128::MAX, 7));
        assert!(adjust_u256_limbs(0, 0, 1, false).unwrap_err().contains("underflowed"));
    }
}
