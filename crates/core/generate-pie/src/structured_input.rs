//! Bounded transport for structured generation input; the library accepts the same typed input.
use std::io::Read;

use blockifier::execution::syscalls::committed_data::{deserialize_witnesses, CommittedDataWitness};
use serde::Deserialize;

use crate::{error::PieGenerationError, PieGenerationInput};

/// Maximum bytes for a generation request, including witnesses. Entry/path limits also apply.
pub const MAX_PIE_INPUT_BYTES: u64 = 128 * 1024 * 1024;

/// Reads one complete JSON request. Rejects trailing data, oversized input and invalid config.
pub fn read_pie_input(reader: impl Read) -> Result<PieGenerationInput, PieGenerationError> {
    let bytes = read_bounded(reader, MAX_PIE_INPUT_BYTES)?;
    let input: PieGenerationInput = serde_json::from_slice(&bytes)
        .map_err(|error| PieGenerationError::InvalidConfig(format!("Invalid PIE input: {error}")))?;
    input.validate()?;
    Ok(input)
}

/// Compatibility transport for standalone witness files, with identical entry and byte limits.
pub fn read_committed_data_witnesses(reader: impl Read) -> Result<Vec<CommittedDataWitness>, PieGenerationError> {
    #[derive(Deserialize)]
    struct WitnessList(#[serde(deserialize_with = "deserialize_witnesses")] Vec<CommittedDataWitness>);
    let bytes = read_bounded(reader, MAX_PIE_INPUT_BYTES)?;
    serde_json::from_slice::<WitnessList>(&bytes)
        .map(|list| list.0)
        .map_err(|error| PieGenerationError::InvalidConfig(format!("Invalid committed-data input: {error}")))
}

fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>, PieGenerationError> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| PieGenerationError::InvalidConfig(format!("Cannot read input: {error}")))?;
    if bytes.len() as u64 > limit {
        return Err(PieGenerationError::InvalidConfig(format!("Input exceeds {limit} bytes")));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_transport_stops_an_unbounded_reader() {
        assert!(read_bounded(std::io::repeat(b' '), 32).is_err());
        assert_eq!(read_bounded(&b"abc"[..], 3).unwrap(), b"abc");
    }

    #[test]
    fn witness_transport_rejects_trailing_input_and_wrong_path_length() {
        assert!(read_committed_data_witnesses(&b"[] []"[..]).is_err());
        let bad = br#"[{"root":"0x1","index":0,"value":"0x3","siblings":[]}]"#;
        assert!(read_committed_data_witnesses(&bad[..]).is_err());
        assert!(read_committed_data_witnesses(&b"[]"[..]).unwrap().is_empty());
    }
    #[test]
    fn structured_input_accepts_inline_configuration_without_files() {
        let input = serde_json::json!({
            "rpc_url": "http://127.0.0.1:1234", "blocks": [1, 2], "layout": "all_cairo",
            "chain_config": crate::types::ChainConfig::default(),
            "os_hints_config": { "use_committed_data": true, "committed_data_witnesses": [] }
        });
        let bytes = serde_json::to_vec(&input).unwrap();
        let parsed = read_pie_input(&bytes[..]).unwrap();
        assert!(parsed.os_hints_config.use_committed_data);
        assert!(parsed.versioned_constants.is_none());
        assert_eq!(parsed.blocks, vec![1, 2]);
    }

    #[test]
    fn committed_data_permission_defaults_off_and_rejects_legacy_policy() {
        let mut input = serde_json::json!({
            "rpc_url": "http://127.0.0.1:1234", "blocks": [1], "layout": "all_cairo",
            "chain_config": crate::types::ChainConfig::default()
        });
        let parse = |value: &serde_json::Value| read_pie_input(serde_json::to_vec(value).unwrap().as_slice());
        assert!(!parse(&input).unwrap().os_hints_config.use_committed_data);
        for legacy in ["committed_data_activation_block", "committed_data_readers"] {
            input["os_hints_config"] = serde_json::json!({});
            input["os_hints_config"][legacy] = serde_json::Value::Null;
            assert!(parse(&input).is_err());
        }
        input["os_hints_config"] = serde_json::json!({});
        input["committed_data_rpc_url"] = serde_json::json!("http://127.0.0.1:1235");
        assert!(parse(&input).is_err());
        input["os_hints_config"]["use_committed_data"] = serde_json::json!(true);
        assert!(parse(&input).is_ok());
    }

    #[test]
    fn shared_root_witness_input_requires_permission_and_rejects_publisher_field() {
        use blockifier::execution::syscalls::committed_data::CommittedDataSet;
        use starknet_types_core::felt::Felt;
        let tree = CommittedDataSet::new(vec![Felt::MAX]).unwrap();
        let mut input = serde_json::json!({
            "rpc_url": "http://127.0.0.1:1234", "blocks": [1], "layout": "all_cairo",
            "chain_config": crate::types::ChainConfig::default(),
            "os_hints_config": {"committed_data_witnesses": [tree.witness(0).unwrap()]}
        });
        let parse = |value: &serde_json::Value| read_pie_input(serde_json::to_vec(value).unwrap().as_slice());
        assert!(parse(&input).is_err());
        input["os_hints_config"]["use_committed_data"] = serde_json::json!(true);
        let parsed = parse(&input).unwrap();
        assert!(parsed.os_hints_config.committed_data_witnesses[0].verify());
        input["os_hints_config"]["committed_data_witnesses"][0]["publisher"] = serde_json::json!("0x1");
        assert!(parse(&input).is_err());
    }
}
