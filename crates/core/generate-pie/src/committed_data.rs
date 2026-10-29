//! Fetches only witnesses actually used during replay. The transport is not trusted for values.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blockifier::execution::syscalls::committed_data::{
    CommittedDataBatchProof, CommittedDataError, CommittedDataProvider, CommittedDataWitness,
    MAX_COMMITTED_DATA_READ_KEYS, MAX_COMMITTED_DATA_WITNESSES,
};
use serde::Deserialize;
use starknet_types_core::felt::Felt;

use crate::error::PieGenerationError;

type Key = (Felt, Felt);
const MAX_RESPONSE_BYTES: usize = 32 * 1024;

/// One request at a time on cache misses; hot reads never wait on network I/O.
/// URLs are deliberately excluded from Debug and transport errors.
pub(crate) struct ReplayWitnesses {
    initial: Mutex<BTreeMap<Key, CommittedDataWitness>>,
    witnesses: Mutex<BTreeMap<Key, CommittedDataWitness>>,
    timing_client: Option<rpc_client::RpcClient>,
    fetch: Mutex<()>,
    remote: Option<(reqwest::Client, reqwest::Url)>,
}

impl std::fmt::Debug for ReplayWitnesses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayWitnesses").finish_non_exhaustive()
    }
}

impl ReplayWitnesses {
    pub fn new(witnesses: Vec<CommittedDataWitness>, endpoint: Option<&str>) -> Result<Arc<Self>, PieGenerationError> {
        if witnesses.len() > MAX_COMMITTED_DATA_WITNESSES {
            return Err(invalid_config(CommittedDataError::TooManyWitnesses));
        }
        let mut validated = BTreeMap::new();
        for witness in witnesses {
            if !witness.verify() {
                return Err(invalid_config(CommittedDataError::InvalidWitness));
            }
            if validated.insert((witness.root, witness.key), witness).is_some() {
                return Err(invalid_config(CommittedDataError::DuplicateWitness));
            }
        }
        let remote = endpoint
            .map(|endpoint| {
                let url =
                    reqwest::Url::parse(endpoint).map_err(|_| invalid_config("Invalid committed-data RPC URL"))?;
                if !matches!(url.scheme(), "http" | "https") {
                    return Err(invalid_config("Committed-data RPC requires HTTP or HTTPS"));
                }
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .connect_timeout(Duration::from_secs(5))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|_| invalid_config("Cannot initialize committed-data RPC"))?;
                Ok((client, url))
            })
            .transpose()?;
        Ok(Arc::new(Self {
            initial: Mutex::new(validated),
            witnesses: Mutex::new(BTreeMap::new()),
            timing_client: None,
            fetch: Mutex::new(()),
            remote,
        }))
    }

    /// Use the ordinary RPC recorder so overlapping witness and state requests count once.
    pub fn with_timing(mut self: Arc<Self>, client: rpc_client::RpcClient) -> Arc<Self> {
        Arc::get_mut(&mut self).expect("Witness provider is not shared during initialization").timing_client =
            Some(client);
        self
    }

    /// Keep only witnesses required by admitted execution of this block. Reverted work may
    /// have populated the bounded transport cache, but must not consume the next block's limit.
    pub fn collect(&self, keys: &[Key]) -> Result<Vec<CommittedDataWitness>, CommittedDataError> {
        if keys.len() > MAX_COMMITTED_DATA_WITNESSES {
            return Err(CommittedDataError::TooManyWitnesses);
        }
        let mut witnesses = Vec::with_capacity(keys.len());
        for group in keys.chunk_by(|left, right| left.0 == right.0) {
            for chunk in group.chunks(MAX_COMMITTED_DATA_READ_KEYS) {
                self.prefetch(chunk[0].0, &chunk.iter().map(|(_, key)| *key).collect::<Vec<_>>())?;
                for key in chunk {
                    witnesses.push(self.witness(*key)?.ok_or(CommittedDataError::Unavailable)?);
                }
            }
        }
        Ok(witnesses)
    }

    #[cfg(test)]
    fn take_all(&self) -> Result<Vec<CommittedDataWitness>, CommittedDataError> {
        let mut witnesses = std::mem::take(&mut *self.initial.lock().unwrap());
        witnesses.extend(std::mem::take(&mut *self.witnesses.lock().unwrap()));
        Ok(witnesses.into_values().collect())
    }

    fn cached(&self, key: Key) -> Result<Option<CommittedDataWitness>, CommittedDataError> {
        if let Some(witness) = self.initial.lock().map_err(|_| provider_error("Witness cache poisoned"))?.get(&key) {
            return Ok(Some(witness.clone()));
        }
        Ok(self.witnesses.lock().map_err(|_| provider_error("Witness cache poisoned"))?.get(&key).cloned())
    }

    async fn fetch_witness(&self, key: Key) -> Result<Option<CommittedDataWitness>, CommittedDataError> {
        let Some((client, url)) = &self.remote else {
            return Ok(None);
        };
        // A feeder base URL selects the read-only GET transport. Retain the legacy JSON-RPC
        // transport for existing operators; there is no automatic fallback after an error.
        let feeder = url.path().trim_end_matches('/').ends_with("/feeder_gateway");
        let request = if feeder {
            let mut endpoint = url.clone();
            endpoint.set_path(&format!("{}/get_committed_data_witness", url.path().trim_end_matches('/')));
            endpoint
                .query_pairs_mut()
                .append_pair("root", &format!("{:#x}", key.0))
                .append_pair("key", &format!("{:#x}", key.1));
            client.get(endpoint)
        } else {
            client.post(url.clone()).json(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "madara_getCommittedDataWitness",
                "params": [key.0, key.1]
            }))
        };
        let _timing =
            self.timing_client.as_ref().map(|client| client.timing().call_started("get_committed_data_witness"));
        let mut response = request.send().await.map_err(|error| transport_error("request", error))?;
        if !response.status().is_success() {
            return Err(provider_error(format!("Witness RPC returned HTTP {}", response.status().as_u16())));
        }
        if response.content_length().is_some_and(|len| len > MAX_RESPONSE_BYTES as u64) {
            return Err(provider_error("Oversized witness RPC response"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| transport_error("response body", error))? {
            if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                return Err(provider_error("Oversized witness RPC response"));
            }
            bytes.extend_from_slice(&chunk);
        }
        drop(_timing);
        if feeder {
            let witness =
                serde_json::from_slice(&bytes).map_err(|_| provider_error("Invalid feeder witness response"))?;
            validate_witness(witness, key)
        } else {
            parse_response(&bytes, key)
        }
    }
}

impl CommittedDataProvider for ReplayWitnesses {
    fn batch(&self, root: Felt, keys: &[Felt]) -> Result<CommittedDataBatchProof, CommittedDataError> {
        if keys.is_empty() || keys.len() > MAX_COMMITTED_DATA_READ_KEYS {
            return Err(provider_error("Invalid witness batch size"));
        }
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread => {
                // Cache-only callers work on any runtime; an actual miss fails below.
            }
            Ok(_) => tokio::task::block_in_place(|| self.prefetch(root, keys))?,
            Err(_) => self.prefetch(root, keys)?,
        }
        CommittedDataBatchProof::new(root, keys, |key| self.witness((root, key)))
    }
    fn value(&self, root: Felt, key: Felt) -> Result<Option<(Felt, usize)>, CommittedDataError> {
        Ok(self.witness((root, key))?.map(|witness| (witness.value, witness.siblings.len())))
    }
}

impl ReplayWitnesses {
    /// One feeder request per bounded batch, with compatibility for pre-batch gateways.
    /// JSON POST keeps wide batches below URI limits. Missing/GET-only routes permit legacy
    /// single reads; malformed/unavailable proofs fail closed.
    fn prefetch(&self, root: Felt, keys: &[Felt]) -> Result<(), CommittedDataError> {
        let Some((client, url)) = &self.remote else {
            return Ok(());
        };
        if !url.path().trim_end_matches('/').ends_with("/feeder_gateway") {
            return Ok(());
        }
        let _fetch = self.fetch.lock().map_err(|_| provider_error("Witness fetch gate poisoned"))?;
        let mut missing = std::collections::BTreeSet::new();
        for key in keys {
            if self.cached((root, *key))?.is_none() {
                missing.insert(*key);
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        let keys: Vec<_> = missing.into_iter().collect();
        let mut endpoint = url.clone();
        endpoint.set_path(&format!("{}/get_committed_data_witnesses", url.path().trim_end_matches('/')));
        let fetched = rpc_client::utils::execute_coroutine(async {
            let timing =
                self.timing_client.as_ref().map(|client| client.timing().call_started("get_committed_data_witnesses"));
            let mut response = client
                .post(endpoint)
                .json(&serde_json::json!({"root": root, "keys": keys}))
                .send()
                .await
                .map_err(|error| transport_error("batch request", error))?;
            if matches!(response.status().as_u16(), 404 | 405) {
                return Ok(None);
            }
            if !response.status().is_success() {
                return Err(provider_error("Witness batch HTTP failure"));
            }
            let limit = keys.len() * MAX_RESPONSE_BYTES;
            if response.content_length().is_some_and(|size| size > limit as u64) {
                return Err(provider_error("Oversized witness batch"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|error| transport_error("batch body", error))? {
                if chunk.len() > limit - bytes.len() {
                    return Err(provider_error("Oversized witness batch"));
                }
                bytes.extend_from_slice(&chunk);
            }
            drop(timing);
            let witnesses: Vec<CommittedDataWitness> =
                serde_json::from_slice(&bytes).map_err(|_| provider_error("Invalid witness batch"))?;
            if witnesses.len() != keys.len() {
                return Err(provider_error("Incomplete witness batch"));
            }
            for (key, witness) in keys.iter().zip(&witnesses) {
                if witness.root != root || witness.key != *key || !witness.verify() {
                    return Err(CommittedDataError::InvalidWitness);
                }
            }
            Ok(Some(witnesses))
        })?;
        if let Some(witnesses) = fetched {
            let mut cache = self.witnesses.lock().map_err(|_| provider_error("Witness cache poisoned"))?;
            while cache.len() + witnesses.len() > MAX_COMMITTED_DATA_WITNESSES {
                cache.pop_first();
            }
            for witness in witnesses {
                cache.insert((witness.root, witness.key), witness);
            }
        }
        Ok(())
    }

    fn witness(&self, key: Key) -> Result<Option<CommittedDataWitness>, CommittedDataError> {
        if let Some(witness) = self.cached(key)? {
            return Ok(Some(witness));
        }
        if self.remote.is_none() {
            return Ok(None);
        }
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread => {
                Err(provider_error("RPC witness replay requires a multithread Tokio runtime"))
            }
            Ok(_) => tokio::task::block_in_place(|| self.fetch_and_store(key)),
            Err(_) => self.fetch_and_store(key),
        }
    }

    fn fetch_and_store(&self, key: Key) -> Result<Option<CommittedDataWitness>, CommittedDataError> {
        let _fetch = self.fetch.lock().map_err(|_| provider_error("Witness fetch gate poisoned"))?;
        if let Some(witness) = self.cached(key)? {
            return Ok(Some(witness));
        }
        let Some(witness) = rpc_client::utils::execute_coroutine(self.fetch_witness(key))? else {
            return Ok(None);
        };
        let mut cache = self.witnesses.lock().map_err(|_| provider_error("Witness cache poisoned"))?;
        // This is a transport cache, not a consensus/admission limit. Blocks retain their
        // own bounded witness vectors; evicting an old fetch cannot invalidate a valid batch.
        if cache.len() == MAX_COMMITTED_DATA_WITNESSES {
            cache.pop_first();
        }
        cache.insert(key, witness.clone());
        Ok(Some(witness))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    jsonrpc: String,
    id: u64,
    result: Option<CommittedDataWitness>,
}

fn parse_response(bytes: &[u8], key: Key) -> Result<Option<CommittedDataWitness>, CommittedDataError> {
    let response: Response =
        serde_json::from_slice(bytes).map_err(|_| provider_error("Invalid witness RPC envelope"))?;
    if response.jsonrpc != "2.0" || response.id != 1 {
        return Err(provider_error("Witness RPC response mismatch"));
    }
    validate_witness(response.result, key)
}

/// Both transports must authenticate identity and Merkle membership before caching any value.
fn validate_witness(
    witness: Option<CommittedDataWitness>,
    key: Key,
) -> Result<Option<CommittedDataWitness>, CommittedDataError> {
    if let Some(witness) = &witness {
        if (witness.root, witness.key) != key || !witness.verify() {
            return Err(CommittedDataError::InvalidWitness);
        }
    }
    Ok(witness)
}

// Keep only safe categories: reqwest's Display/Debug/source chain can contain access-bearing URLs.
fn transport_error(operation: &str, error: reqwest::Error) -> CommittedDataError {
    let category = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection failure"
    } else if error.is_body() {
        "body transfer failure"
    } else if error.is_decode() {
        "decoding failure"
    } else {
        "transport failure"
    };
    provider_error(format!("Witness RPC {operation} failed: {category}"))
}

fn provider_error(message: impl Into<String>) -> CommittedDataError {
    CommittedDataError::Provider(message.into())
}
fn invalid_config(error: impl std::fmt::Display) -> PieGenerationError {
    PieGenerationError::InvalidConfig(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use blockifier::execution::syscalls::committed_data::CommittedDataSet;

    #[test]
    fn rpc_cannot_substitute_a_different_tuple_or_value() {
        let tree = CommittedDataSet::new(vec![(Felt::ZERO, Felt::MAX)]).unwrap();
        let witness = tree.witness(Felt::ZERO).unwrap();
        let key = (witness.root, witness.key);
        let envelope = |w| serde_json::to_vec(&serde_json::json!({"jsonrpc":"2.0", "id":1, "result":w})).unwrap();
        assert_eq!(parse_response(&envelope(witness.clone()), key).unwrap(), Some(witness.clone()));
        assert!(parse_response(&envelope(witness.clone()), (key.0, Felt::ONE)).is_err());
        let mut bad = witness;
        bad.value = Felt::ZERO;
        assert!(parse_response(&envelope(bad), key).is_err());
        assert!(parse_response(br#"{"jsonrpc":"2.0","id":2,"result":null}"#, key).is_err());
        assert!(parse_response(br#"{"jsonrpc":"2.0","id":1,"error":{"code":-1}}"#, key).is_err());
    }

    #[test]
    fn inline_witnesses_work_without_network() {
        let tree = CommittedDataSet::new(vec![(Felt::ZERO, Felt::MAX)]).unwrap();
        let provider = ReplayWitnesses::new(vec![tree.witness(Felt::ZERO).unwrap()], None).unwrap();
        assert_eq!(provider.value(tree.root(), Felt::ZERO).unwrap(), Some((Felt::MAX, 0)));
        assert_eq!(provider.value(tree.root(), Felt::ONE).unwrap(), None);
        assert_eq!(
            provider.batch(tree.root(), &[Felt::ZERO, Felt::ZERO]).unwrap(),
            CommittedDataBatchProof::from_dataset(&tree, &[Felt::ZERO, Felt::ZERO]).unwrap()
        );
        assert!(provider.batch(tree.root(), &[Felt::ONE]).is_err());
        assert_eq!(provider.take_all().unwrap().len(), 1);
    }
    #[test]
    fn batch_replay_bounds_repeated_sibling_hashes() {
        use blockifier::execution::syscalls::committed_data::{
            branch, leaf, CommittedDataSibling, COMMITTED_DATA_TREE_HEIGHT,
        };
        let mut root = leaf(Felt::ZERO, Felt::ONE);
        let mut siblings = Vec::new();
        for bit in 0..COMMITTED_DATA_TREE_HEIGHT {
            siblings.push(CommittedDataSibling { bit: bit as u8, hash: root });
            root = branch(bit as u8, root, root);
        }
        let witness = CommittedDataWitness { root, key: Felt::ZERO, value: Felt::ONE, siblings };
        let provider = ReplayWitnesses::new(vec![witness.clone()], None).unwrap();
        let proof = provider.batch(root, &[Felt::ZERO, Felt::ZERO]).unwrap();
        assert_eq!(proof.branches, COMMITTED_DATA_TREE_HEIGHT);
        assert_eq!(proof.proof.len(), 4 * COMMITTED_DATA_TREE_HEIGHT + 2);
        assert_eq!(proof.leaves, vec![Felt::ZERO, Felt::ONE]);
        assert_eq!(proof.indices, vec![0, 0]);
        assert_eq!(provider.take_all().unwrap(), vec![witness]);
    }

    #[test]
    fn duplicate_root_key_input_is_rejected() {
        let tree = CommittedDataSet::new(vec![(Felt::ZERO, Felt::MAX)]).unwrap();
        let witness = tree.witness(Felt::ZERO).unwrap();
        assert!(ReplayWitnesses::new(vec![witness.clone(), witness], None).is_err());
    }

    #[test]
    fn invalid_and_oversized_inline_witnesses_are_rejected() {
        let tree = CommittedDataSet::new(vec![(Felt::ZERO, Felt::MAX)]).unwrap();
        let witness = tree.witness(Felt::ZERO).unwrap();
        let oversized = vec![witness.clone(); MAX_COMMITTED_DATA_WITNESSES + 1];
        let error = ReplayWitnesses::new(oversized, None).unwrap_err();
        assert!(error.to_string().contains("At most"));
        let mut invalid = witness;
        invalid.value = Felt::ZERO;
        let error = ReplayWitnesses::new(vec![invalid], None).unwrap_err();
        assert!(error.to_string().contains("does not match its root"));
    }

    #[test]
    fn taking_witnesses_empties_the_cache() {
        let tree = CommittedDataSet::new(vec![(Felt::ZERO, Felt::MAX), (Felt::ONE, Felt::ONE)]).unwrap();
        let witnesses = vec![tree.witness(Felt::ZERO).unwrap(), tree.witness(Felt::ONE).unwrap()];
        let provider = ReplayWitnesses::new(witnesses.clone(), None).unwrap();
        assert_eq!(provider.take_all().unwrap(), witnesses);
        assert!(provider.take_all().unwrap().is_empty());
        assert_eq!(provider.value(tree.root(), Felt::ZERO).unwrap(), None);
    }

    #[tokio::test]
    async fn transport_errors_preserve_categories_without_endpoint_secrets() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://user:password@{}/private?token=secret", listener.local_addr().unwrap());
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_millis(100)).build().unwrap();
        // Keep the listener open without responding, so the request must time out.
        let error = client.get(&endpoint).send().await.unwrap_err();
        assert_eq!(
            transport_error("request", error).to_string(),
            "Committed-data provider failed: Witness RPC request failed: timeout"
        );
        drop(listener);
        let error = client.get(&endpoint).send().await.unwrap_err();
        assert_eq!(
            transport_error("request", error).to_string(),
            "Committed-data provider failed: Witness RPC request failed: connection failure"
        );
    }

    #[tokio::test]
    async fn http_failure_preserves_status_without_endpoint_or_response_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://user:password@{}/private?token=secret", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0_u8; 1024];
                let count = stream.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                assert!(request.len() <= 4096);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&request[..end]).unwrap().to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            stream
                .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret")
                .await
                .unwrap();
        });
        let provider = ReplayWitnesses::new(Vec::new(), Some(&endpoint)).unwrap();
        let error = provider.fetch_witness((Felt::ONE, Felt::ZERO)).await.unwrap_err();
        assert_eq!(error.to_string(), "Committed-data provider failed: Witness RPC returned HTTP 503");
        assert!(provider.take_all().unwrap().is_empty());
        server.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn feeder_get_witness_is_authenticated_cached_and_collected() {
        use std::io::{Read, Write};
        let tree = CommittedDataSet::new(vec![(Felt::MAX, Felt::MAX)]).unwrap();
        let witness = tree.witness(Felt::MAX).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/feeder_gateway/", listener.local_addr().unwrap());
        let root = tree.root();
        let key = Felt::MAX;
        let body = serde_json::to_string(&Some(witness.clone())).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut buffer = [0_u8; 1024];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0 && request.len() + count <= 4096);
                request.extend_from_slice(&buffer[..count]);
            }
            let request = std::str::from_utf8(&request).unwrap();
            assert!(request.starts_with(&format!(
                "GET /feeder_gateway/get_committed_data_witness?root={root:#x}&key={key:#x} HTTP/1.1"
            )));
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
                .unwrap();
        });
        let provider = ReplayWitnesses::new(Vec::new(), Some(&endpoint)).unwrap();
        assert_eq!(provider.value(root, Felt::MAX).unwrap(), Some((Felt::MAX, 0)));
        server.join().unwrap();
        assert_eq!(provider.value(root, Felt::MAX).unwrap(), Some((Felt::MAX, 0)));
        assert_eq!(provider.take_all().unwrap(), vec![witness]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authenticated_rpc_response_is_cached_and_collected_for_os() {
        use std::io::{Read, Write};
        let tree = CommittedDataSet::new(vec![(Felt::ZERO, Felt::MAX)]).unwrap();
        let witness = tree.witness(Felt::ZERO).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let expected_params = serde_json::json!([tree.root(), Felt::ZERO]);
        let body = serde_json::to_string(&serde_json::json!({"jsonrpc":"2.0","id":1,"result":witness})).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0_u8; 1024];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                assert!(request.len() <= 4096);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&request[..end]).unwrap().to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        let json: serde_json::Value =
                            serde_json::from_slice(&request[end + 4..end + 4 + length]).unwrap();
                        assert_eq!(json["method"], "madara_getCommittedDataWitness");
                        assert_eq!(json["params"], expected_params);
                        break;
                    }
                }
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
                .unwrap();
        });
        let provider = ReplayWitnesses::new(Vec::new(), Some(&endpoint)).unwrap();
        let root = tree.root();
        let height = 0;
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let cached = provider.clone();
                tokio::spawn(async move {
                    assert_eq!(cached.value(root, Felt::ZERO).unwrap(), Some((Felt::MAX, height)));
                })
            })
            .collect();
        tokio::time::timeout(Duration::from_secs(10), async {
            for task in tasks {
                task.await.unwrap();
            }
        })
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(provider.take_all().unwrap(), vec![witness]);
    }

    /// The transport may retain speculative work, but each OS block gets only admitted keys.
    #[test]
    fn collecting_block_inputs_does_not_drain_other_blocks_or_include_unused_witnesses() {
        let tree =
            CommittedDataSet::new(vec![(Felt::ZERO, Felt::ONE), (Felt::ONE, Felt::TWO), (Felt::TWO, Felt::THREE)])
                .unwrap();
        let all: Vec<_> = tree.entries().iter().map(|(key, _)| tree.witness(*key).unwrap()).collect();
        let provider = ReplayWitnesses::new(all.clone(), None).unwrap();
        for key in [Felt::ONE, Felt::ZERO, Felt::ONE] {
            assert_eq!(provider.collect(&[(tree.root(), key)]).unwrap(), vec![tree.witness(key).unwrap()]);
        }
        assert_eq!(provider.take_all().unwrap(), all);
    }

    /// Read a complete bounded fixture request, including JSON bodies split across TCP packets.
    fn read_http_request(stream: &mut std::net::TcpStream) -> (String, Vec<u8>) {
        use std::io::Read;
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0 && request.len() + count <= 128 * 1024);
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&request[..end]).unwrap();
                let length = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
                if request.len() >= end + 4 + length {
                    return (headers.to_owned(), request[end + 4..end + 4 + length].to_vec());
                }
            }
        }
    }

    /// Real HTTP framing exercises one-request reconstruction, all-or-nothing authentication,
    /// legacy fallback, and the timing recorder through delayed response-body delivery.
    #[rstest::rstest]
    #[case::valid("valid", true)]
    #[case::missing_route("missing_route", true)]
    #[case::legacy_get_only("legacy_get_only", true)]
    #[case::maximum_wide_keys("maximum_wide_keys", true)]
    #[case::wrong_order("wrong_order", false)]
    #[case::corrupt_value("corrupt_value", false)]
    #[case::incomplete("incomplete", false)]
    #[case::unavailable("unavailable", false)]
    #[case::oversized("oversized", false)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn feeder_batch_transport_and_timing(#[case] case: &str, #[case] succeeds: bool) {
        use std::io::Write;
        let entries = if case == "maximum_wide_keys" {
            (0..MAX_COMMITTED_DATA_READ_KEYS).map(|index| (Felt::MAX - Felt::from(index), Felt::from(index))).collect()
        } else {
            vec![(Felt::ZERO, Felt::TWO), (Felt::ONE, Felt::MAX)]
        };
        let tree = CommittedDataSet::new(entries).unwrap();
        let keys: Vec<_> = tree.entries().iter().map(|(key, _)| *key).collect();
        let proofs: Vec<_> = keys.iter().map(|key| tree.witness(*key).unwrap()).collect();
        let mut response = proofs.clone();
        match case {
            "wrong_order" => response.reverse(),
            "corrupt_value" => response[1].value = Felt::ZERO,
            "incomplete" => {
                response.pop();
            }
            _ => {}
        }
        let status = match case {
            "missing_route" => "404 Not Found",
            "legacy_get_only" => "405 Method Not Allowed",
            "unavailable" => "503 Unavailable",
            _ => "200 OK",
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/feeder_gateway", listener.local_addr().unwrap());
        let expected_requests = if matches!(case, "missing_route" | "legacy_get_only") { 3 } else { 1 };
        let oversized = case == "oversized";
        let server = std::thread::spawn(move || {
            for request_index in 0..expected_requests {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline, "missing witness request");
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let (headers, body) = read_http_request(&mut stream);
                if request_index == 0 {
                    assert!(headers.starts_with("POST /feeder_gateway/get_committed_data_witnesses HTTP/1.1"));
                    let query: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(query["root"], serde_json::json!(response.first().unwrap().root));
                    let requested: Vec<Felt> = serde_json::from_value(query["keys"].clone()).unwrap();
                    assert_eq!(requested, proofs.iter().map(|proof| proof.key).collect::<Vec<_>>());
                } else {
                    assert!(headers.starts_with("GET /feeder_gateway/get_committed_data_witness?"));
                }
                let body = if request_index == 0 {
                    serde_json::to_vec(&response).unwrap()
                } else {
                    serde_json::to_vec(&Some(proofs[request_index - 1].clone())).unwrap()
                };
                let status = if request_index == 0 { status } else { "200 OK" };
                let length = if oversized { 2 * MAX_RESPONSE_BYTES + 1 } else { body.len() };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").unwrap();
                if oversized {
                    continue;
                }
                std::thread::sleep(Duration::from_millis(25));
                // Error responses may be closed without consuming their untrusted body.
                let _ = stream.write_all(&body);
            }
        });
        let timing = rpc_client::RpcClient::try_new(&endpoint).unwrap();
        let provider = ReplayWitnesses::new(Vec::new(), Some(&endpoint)).unwrap().with_timing(timing.clone());
        let result = provider.batch(tree.root(), &keys);
        server.join().unwrap();
        if succeeds {
            assert_eq!(result.unwrap(), CommittedDataBatchProof::from_dataset(&tree, &keys).unwrap());
            // The server is gone: both retained values and the per-block export are now cache hits.
            assert_eq!(
                provider.collect(&keys.iter().map(|key| (tree.root(), *key)).collect::<Vec<_>>()).unwrap().len(),
                keys.len()
            );
        } else {
            assert!(result.is_err());
            assert!(provider.take_all().unwrap().is_empty());
        }
        let snapshot = timing.timing_snapshot();
        assert_eq!(snapshot.calls, expected_requests as u64);
        assert_eq!(snapshot.calls_by_method["get_committed_data_witnesses"], 1);
        if succeeds {
            assert!(snapshot.wait_elapsed >= Duration::from_millis(25));
        }
    }

    /// Ten legal block read sets cross the old job-wide cap. Exercise real HTTP fetches,
    /// both ABIs, speculative reads discarded on revert, root rotation, and cache eviction.
    #[test]
    fn multi_block_replay_collects_more_than_one_block_witness_budget() {
        use blockifier::execution::syscalls::committed_data::{
            read_values_shared, CommittedDataReadSet, CommittedDataWitnesses,
        };
        use std::io::Write;
        use std::sync::atomic::{AtomicBool, Ordering};
        const BLOCKS: usize = 10;
        const KEYS_PER_BLOCK: usize = 7000;
        assert!(BLOCKS * KEYS_PER_BLOCK > MAX_COMMITTED_DATA_WITNESSES);
        let trees: Vec<_> = (0..BLOCKS)
            .map(|block| {
                CommittedDataSet::new(
                    (0..=KEYS_PER_BLOCK).map(|key| (Felt::from(key), Felt::from(block + key))).collect(),
                )
                .unwrap()
            })
            .collect();
        let roots: Vec<_> = trees.iter().map(CommittedDataSet::root).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/feeder_gateway", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = stop.clone();
        let server = std::thread::spawn(move || {
            let trees: BTreeMap<_, _> = trees.into_iter().map(|tree| (tree.root(), tree)).collect();
            while !server_stop.load(Ordering::Relaxed) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let (headers, body) = read_http_request(&mut stream);
                let body = if headers.starts_with("POST ") {
                    let query: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let root: Felt = serde_json::from_value(query["root"].clone()).unwrap();
                    let keys: Vec<Felt> = serde_json::from_value(query["keys"].clone()).unwrap();
                    let proofs: Vec<_> = keys.iter().map(|key| trees[&root].witness(*key).unwrap()).collect();
                    serde_json::to_vec(&proofs).unwrap()
                } else {
                    let url =
                        reqwest::Url::parse(&format!("http://localhost{}", headers.split_whitespace().nth(1).unwrap()))
                            .unwrap();
                    let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
                    let root = Felt::from_hex(&query["root"]).unwrap();
                    serde_json::to_vec(&trees[&root].witness(Felt::from_hex(&query["key"]).unwrap())).unwrap()
                };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                    .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        // Always stop the fixture server, including when an assertion below panics.
        struct StopServer(Arc<AtomicBool>);
        impl Drop for StopServer {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        let stop_server = StopServer(stop);
        let provider = ReplayWitnesses::new(Vec::new(), Some(&endpoint)).unwrap();
        let witnesses = CommittedDataWitnesses::from_provider(provider.clone());
        let single = starknet_api::abi::abi_utils::selector_from_name("get_value").0;
        let batch = starknet_api::abi::abi_utils::selector_from_name("get_values").0;
        let mut total = 0;
        for root in &roots {
            let accepted = CommittedDataReadSet::default();
            let discarded = accepted.fork();
            let mut speculative_gas = u64::MAX;
            read_values_shared(
                true,
                &witnesses,
                &discarded,
                single,
                &[*root, Felt::from(KEYS_PER_BLOCK)],
                &mut speculative_gas,
                &mut None,
            )
            .unwrap();
            assert!(accepted.is_empty());
            let mut gas = u64::MAX;
            read_values_shared(true, &witnesses, &accepted, single, &[*root, Felt::ZERO], &mut gas, &mut None).unwrap();
            let keys: Vec<_> = (1..KEYS_PER_BLOCK).map(Felt::from).collect();
            for keys in keys.chunks(MAX_COMMITTED_DATA_READ_KEYS) {
                let mut calldata = vec![*root, Felt::from(keys.len())];
                calldata.extend_from_slice(keys);
                read_values_shared(true, &witnesses, &accepted, batch, &calldata, &mut gas, &mut None).unwrap();
            }
            let required = accepted.witness_keys();
            assert_eq!(required.len(), KEYS_PER_BLOCK);
            assert!(!required.contains(&(*root, Felt::from(KEYS_PER_BLOCK))));
            assert_eq!(provider.collect(&required).unwrap().len(), KEYS_PER_BLOCK);
            total += required.len();
        }
        assert_eq!(total, BLOCKS * KEYS_PER_BLOCK);
        assert!(provider.witnesses.lock().unwrap().len() <= MAX_COMMITTED_DATA_WITNESSES);
        // Old block witnesses remain recoverable after the shared transport cache evicts them.
        let first: Vec<_> = (0..KEYS_PER_BLOCK).map(|key| (roots[0], Felt::from(key))).collect();
        assert_eq!(provider.collect(&first).unwrap().len(), KEYS_PER_BLOCK);
        drop(stop_server);
        server.join().unwrap();
    }
}
