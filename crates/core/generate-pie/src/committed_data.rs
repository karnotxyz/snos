//! Fetches only witnesses actually used during replay. The transport is not trusted for values.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blockifier::execution::syscalls::committed_data::{
    CommittedDataError, CommittedDataProvider, CommittedDataWitness, CommittedDataWitnesses,
    MAX_COMMITTED_DATA_WITNESSES,
};
use serde::Deserialize;
use starknet_types_core::felt::Felt;

use crate::error::PieGenerationError;

type Key = (Felt, Felt, u32);
const MAX_RESPONSE_BYTES: usize = 8192;

/// One request at a time on cache misses; hot reads never wait on network I/O.
/// URLs are deliberately excluded from Debug and transport errors.
pub(crate) struct ReplayWitnesses {
    witnesses: Mutex<BTreeMap<Key, CommittedDataWitness>>,
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
        CommittedDataWitnesses::new(witnesses.clone()).map_err(invalid_config)?;
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
            witnesses: Mutex::new(witnesses.into_iter().map(|w| ((w.root, w.publisher, w.index), w)).collect()),
            fetch: Mutex::new(()),
            remote,
        }))
    }

    pub fn all(&self) -> Result<Vec<CommittedDataWitness>, CommittedDataError> {
        Ok(self.witnesses.lock().map_err(|_| provider_error("Witness cache poisoned"))?.values().cloned().collect())
    }

    fn cached(&self, key: Key) -> Result<Option<Felt>, CommittedDataError> {
        Ok(self.witnesses.lock().map_err(|_| provider_error("Witness cache poisoned"))?.get(&key).map(|w| w.value))
    }

    async fn fetch_witness(&self, key: Key) -> Result<Option<CommittedDataWitness>, CommittedDataError> {
        let Some((client, url)) = &self.remote else {
            return Ok(None);
        };
        let mut response = client
            .post(url.clone())
            .json(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "madara_getCommittedDataWitness",
                "params": [key.0, key.1, key.2]
            }))
            .send()
            .await
            .map_err(|_| provider_error("Witness RPC request failed"))?;
        if !response.status().is_success() {
            return Err(provider_error("Witness RPC returned a failure status"));
        }
        if response.content_length().is_some_and(|len| len > MAX_RESPONSE_BYTES as u64) {
            return Err(provider_error("Oversized witness RPC response"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) =
            response.chunk().await.map_err(|_| provider_error("Cannot read witness RPC response"))?
        {
            if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
                return Err(provider_error("Oversized witness RPC response"));
            }
            bytes.extend_from_slice(&chunk);
        }
        parse_response(&bytes, key)
    }
}

impl CommittedDataProvider for ReplayWitnesses {
    fn value(&self, root: Felt, publisher: Felt, index: u32) -> Result<Option<Felt>, CommittedDataError> {
        let key = (root, publisher, index);
        if let Some(value) = self.cached(key)? {
            return Ok(Some(value));
        }
        if self.remote.is_none() {
            return Ok(None);
        }
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread => {
                Err(provider_error("RPC witness replay requires a multithread Tokio runtime"))
            }
            // Mark both lock waiting and network waiting as blocking, so another replay cannot
            // park the last Tokio worker while the first request needs the I/O driver.
            Ok(_) => tokio::task::block_in_place(|| self.fetch_and_store(key)),
            Err(_) => self.fetch_and_store(key),
        }
    }
}

impl ReplayWitnesses {
    fn fetch_and_store(&self, key: Key) -> Result<Option<Felt>, CommittedDataError> {
        // Missing data is fatal to replay, never transformed into a provable contract revert.
        let _fetch = self.fetch.lock().map_err(|_| provider_error("Witness fetch gate poisoned"))?;
        if let Some(value) = self.cached(key)? {
            return Ok(Some(value));
        }
        if self.witnesses.lock().map_err(|_| provider_error("Witness cache poisoned"))?.len()
            >= MAX_COMMITTED_DATA_WITNESSES
        {
            return Err(CommittedDataError::TooManyWitnesses);
        }
        let Some(witness) = rpc_client::utils::execute_coroutine(self.fetch_witness(key))? else {
            return Ok(None);
        };
        let value = witness.value;
        self.witnesses.lock().map_err(|_| provider_error("Witness cache poisoned"))?.insert(key, witness);
        Ok(Some(value))
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
    if let Some(witness) = &response.result {
        if (witness.root, witness.publisher, witness.index) != key || !witness.verify() {
            return Err(CommittedDataError::InvalidWitness);
        }
    }
    Ok(response.result)
}

fn provider_error(message: &str) -> CommittedDataError {
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
        let tree = CommittedDataSet::new(Felt::ONE, vec![Felt::MAX]).unwrap();
        let witness = tree.witness(0).unwrap();
        let key = (witness.root, witness.publisher, witness.index);
        let envelope = |w| serde_json::to_vec(&serde_json::json!({"jsonrpc":"2.0", "id":1, "result":w})).unwrap();
        assert_eq!(parse_response(&envelope(witness.clone()), key).unwrap(), Some(witness.clone()));
        assert!(parse_response(&envelope(witness.clone()), (key.0, Felt::TWO, 0)).is_err());
        let mut bad = witness;
        bad.value = Felt::ZERO;
        assert!(parse_response(&envelope(bad), key).is_err());
        assert!(parse_response(br#"{"jsonrpc":"2.0","id":2,"result":null}"#, key).is_err());
        assert!(parse_response(br#"{"jsonrpc":"2.0","id":1,"error":{"code":-1}}"#, key).is_err());
    }

    #[test]
    fn inline_witnesses_work_without_network() {
        let tree = CommittedDataSet::new(Felt::ONE, vec![Felt::MAX]).unwrap();
        let provider = ReplayWitnesses::new(vec![tree.witness(0).unwrap()], None).unwrap();
        assert_eq!(provider.value(tree.root(), Felt::ONE, 0).unwrap(), Some(Felt::MAX));
        assert_eq!(provider.value(tree.root(), Felt::ONE, 1).unwrap(), None);
        assert_eq!(provider.all().unwrap().len(), 1);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authenticated_rpc_response_is_cached_and_collected_for_os() {
        use std::io::{Read, Write};
        let tree = CommittedDataSet::new(Felt::ONE, vec![Felt::MAX]).unwrap();
        let witness = tree.witness(0).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
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
                        break;
                    }
                }
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)
                .unwrap();
        });
        let provider = ReplayWitnesses::new(Vec::new(), Some(&endpoint)).unwrap();
        let root = tree.root();
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let cached = provider.clone();
                tokio::spawn(async move {
                    assert_eq!(cached.value(root, Felt::ONE, 0).unwrap(), Some(Felt::MAX));
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
        assert_eq!(provider.all().unwrap(), vec![witness]);
    }
}
