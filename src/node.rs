//! Talking to the running stack: the local IPFS, graph-node's admin port and its status port.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::stack::Ports;

pub struct Node {
    ports: Ports,
    client: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Status {
    pub subgraph: String,
    pub synced: bool,
    pub health: String,
    #[serde(rename = "entityCount")]
    pub entity_count: String,
    #[serde(rename = "fatalError")]
    pub fatal_error: Option<FatalError>,
    pub chains: Vec<Chain>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FatalError {
    pub message: String,
    pub deterministic: bool,
    pub block: Option<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Chain {
    pub network: String,
    #[serde(rename = "chainHeadBlock")]
    pub head: Option<Block>,
    #[serde(rename = "earliestBlock")]
    pub earliest: Option<Block>,
    #[serde(rename = "latestBlock")]
    pub latest: Option<Block>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Block {
    pub number: String,
}

impl Block {
    pub fn n(&self) -> u64 {
        self.number.parse().unwrap_or(0)
    }
}

const STATUS_QUERY: &str = "query($name: String!) { indexingStatusForCurrentVersion(subgraphName: $name) { subgraph synced health entityCount fatalError { message deterministic block { number } } chains { network chainHeadBlock { number } earliestBlock { number } latestBlock { number } } } }";

impl Node {
    pub fn new(ports: Ports) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("a default client");
        Self { ports, client }
    }

    /// Wait until IPFS and graph-node's admin port both answer. graph-node needs Postgres first,
    /// and on a cold start that is most of a minute.
    pub async fn wait_ready(&self, limit: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let ipfs = self
                .client
                .post(format!(
                    "http://127.0.0.1:{}/api/v0/version",
                    self.ports.ipfs
                ))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            // The admin port answers a GET with 405, which is all that is wanted of it here.
            let admin = self
                .client
                .get(format!("http://127.0.0.1:{}/", self.ports.admin))
                .send()
                .await
                .is_ok();
            if ipfs && admin {
                return Ok(());
            }
            if tokio::time::Instant::now() > deadline {
                bail!(
                    "the stack did not come up in {}s (IPFS {}, graph-node {}). `drydock logs` shows why",
                    limit.as_secs(),
                    if ipfs { "up" } else { "down" },
                    if admin { "up" } else { "down" }
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    pub async fn ipfs_add(&self, name: &str, bytes: Vec<u8>) -> Result<String> {
        // Only the last path segment: given `USDC/ERC20.json`, kubo wraps the file in a directory
        // and answers with one line for each, and the hash wanted is the file's own.
        let leaf = name.rsplit('/').next().unwrap_or(name).to_string();
        let part = reqwest::multipart::Part::bytes(bytes).file_name(leaf);
        let text = self
            .client
            .post(format!(
                "http://127.0.0.1:{}/api/v0/add?pin=true",
                self.ports.ipfs
            ))
            .multipart(reqwest::multipart::Form::new().part("file", part))
            .send()
            .await
            .with_context(|| format!("uploading {name} to the local IPFS"))?
            .error_for_status()?
            .text()
            .await?;
        added_hash(&text)
            .ok_or_else(|| anyhow!("the local IPFS answered without a hash for {name}"))
    }

    async fn admin(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let res: Value = self
            .client
            .post(format!("http://127.0.0.1:{}/", self.ports.admin))
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        if let Some(e) = res.get("error") {
            bail!(
                "{}",
                e.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("graph-node refused the call")
            );
        }
        Ok(res.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Create the name if it is new, then point it at this build.
    pub async fn deploy(&self, name: &str, ipfs_hash: &str) -> Result<()> {
        if let Err(e) = self.admin("subgraph_create", json!({ "name": name })).await
            && !e.to_string().contains("already exists")
        {
            return Err(e.context("creating the subgraph name"));
        }
        self.admin(
            "subgraph_deploy",
            json!({ "name": name, "ipfs_hash": ipfs_hash }),
        )
        .await
        .context("deploying to the local graph-node")?;
        Ok(())
    }

    /// None when graph-node has no deployment under that name.
    pub async fn status(&self, name: &str) -> Result<Option<Status>> {
        let body = json!({ "query": STATUS_QUERY, "variables": { "name": name } });
        let res: Value = self
            .client
            .post(format!("http://127.0.0.1:{}/graphql", self.ports.status))
            .json(&body)
            .send()
            .await
            .context("the status port did not answer; is the stack up? (`drydock up`)")?
            .json()
            .await?;
        if let Some(e) = res.get("errors") {
            bail!("the status query failed: {e}");
        }
        Ok(serde_json::from_value(
            res["data"]["indexingStatusForCurrentVersion"].clone(),
        )?)
    }
}

/// The hash from kubo's answer to `add`, which is one JSON object per line.
fn added_hash(answer: &str) -> Option<String> {
    let first: Value = serde_json::from_str(answer.lines().next()?).ok()?;
    first
        .get("Hash")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One line a developer can read at a glance, and the error in full when there is one.
pub fn describe(s: &Status) -> String {
    let chain = s.chains.first();
    let at = |b: Option<&Block>| b.map(Block::n).unwrap_or(0);
    let (earliest, latest, head) = (
        at(chain.and_then(|c| c.earliest.as_ref())),
        at(chain.and_then(|c| c.latest.as_ref())),
        at(chain.and_then(|c| c.head.as_ref())),
    );
    let mut out = match &s.fatal_error {
        Some(e) => {
            let whose = if e.deterministic {
                "It is deterministic: the mapping has a bug, and every indexer would stop at the same block."
            } else {
                "It is not deterministic, so more likely the RPC endpoint than the mapping."
            };
            format!(
                "failed at block {}\n\n  {}\n\n{whose}",
                e.block.as_ref().map(Block::n).unwrap_or(latest),
                e.message
            )
        }
        None if s.synced => format!("synced at block {latest}"),
        None => {
            let (done, total) = (
                latest.saturating_sub(earliest),
                head.saturating_sub(earliest).max(1),
            );
            format!(
                "syncing: block {latest} of {head}, {}% ({} to go)",
                done * 100 / total,
                head.saturating_sub(latest)
            )
        }
    };
    out.push_str(&format!(
        "\n{} entities, health {}",
        s.entity_count, s.health
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(json: Value) -> Status {
        serde_json::from_value(json).unwrap()
    }

    // The shapes graph-node v0.45.0 answered with during the spike on 2026-11-11.
    #[test]
    fn a_sync_in_progress_reads_as_progress() {
        let s = status(
            json!({ "subgraph": "Qm", "synced": false, "health": "healthy", "entityCount": "2", "fatalError": null,
            "chains": [{ "network": "base", "chainHeadBlock": { "number": "56059681" }, "earliestBlock": { "number": "56055186" }, "latestBlock": { "number": "56057538" } }] }),
        );
        assert_eq!(
            describe(&s),
            "syncing: block 56057538 of 56059681, 52% (2143 to go)\n2 entities, health healthy"
        );
    }

    #[test]
    fn a_handler_failure_carries_the_message_and_says_whose_fault_it_is() {
        let s = status(
            json!({ "subgraph": "Qm", "synced": false, "health": "failed", "entityCount": "1",
            "fatalError": { "message": "Mapping aborted at src/mapping.ts, line 20, column 19", "deterministic": true, "block": { "number": "56055226" } },
            "chains": [{ "network": "base", "chainHeadBlock": null, "earliestBlock": { "number": "56055186" }, "latestBlock": { "number": "56055226" } }] }),
        );
        let text = describe(&s);
        assert!(text.starts_with("failed at block 56055226"));
        assert!(text.contains("src/mapping.ts, line 20") && text.contains("the mapping has a bug"));
        let flaky = status(
            json!({ "subgraph": "Qm", "synced": false, "health": "failed", "entityCount": "0",
            "fatalError": { "message": "x", "deterministic": false, "block": null }, "chains": [] }),
        );
        assert!(describe(&flaky).contains("more likely the RPC endpoint"));
    }

    #[test]
    fn the_hash_is_read_from_the_first_line_of_kubos_answer() {
        assert_eq!(
            added_hash("{\"Name\":\"ERC20.json\",\"Hash\":\"QmFile\",\"Size\":\"210\"}\n")
                .as_deref(),
            Some("QmFile")
        );
        assert_eq!(
            added_hash(
                "{\"Name\":\"a\",\"Hash\":\"QmFile\"}\n{\"Name\":\"\",\"Hash\":\"QmDir\"}\n"
            )
            .as_deref(),
            Some("QmFile")
        );
        assert_eq!(added_hash("not json"), None);
        assert_eq!(added_hash(""), None);
    }

    #[test]
    fn synced_says_so() {
        let s = status(
            json!({ "subgraph": "Qm", "synced": true, "health": "healthy", "entityCount": "9", "fatalError": null,
            "chains": [{ "network": "base", "chainHeadBlock": { "number": "10" }, "earliestBlock": { "number": "1" }, "latestBlock": { "number": "10" } }] }),
        );
        assert!(describe(&s).starts_with("synced at block 10"));
    }
}
