//! The graph-node stack sib runs, as two files: a compose file and graph-node's own config.
//!
//! Every setting here is one that a default graph-node gets wrong for a developer on a shared
//! endpoint, and each was either measured in `docs/spike.md` or read out of graph-node's source.

use crate::preflight::Report;

pub const GRAPH_NODE_IMAGE: &str = "graphprotocol/graph-node:v0.45.0";
pub const POSTGRES_IMAGE: &str = "postgres:16";
pub const IPFS_IMAGE: &str = "ipfs/kubo:v0.38.1";
pub const GOVERNOR_IMAGE: &str = "nginx:1.29-alpine";

/// Requests a second sib lets through to the endpoint unless told otherwise. publicnode's cap,
/// read from its own 429 on 2026-11-11, is 1,200 a minute, which is 20 a second.
pub const DEFAULT_RPS: u32 = 15;

/// Where graph-node is pointed when the governor is in the stack.
const GOVERNED_URL: &str = "http://rpc:8545";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ports {
    pub query: u16,
    pub admin: u16,
    pub status: u16,
    pub ipfs: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            query: 8000,
            admin: 8020,
            status: 8030,
            ipfs: 5001,
        }
    }
}

/// graph-node's `--config` file. Config mode is the only one that can switch the log store on,
/// which is what puts mapping logs behind `_logs` on the query port.
pub fn config_toml(network: &str, rpc_url: &str, rps: u32, report: &Report) -> String {
    let rpc_url = if rps > 0 { GOVERNED_URL } else { rpc_url };
    let features = report
        .features
        .iter()
        .map(|f| format!("\"{f}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"[store]
[store.primary]
connection = "postgresql://graph:graph@postgres:5432/graph"
pool_size = 10

[deployment]
[[deployment.rule]]
store = "primary"
indexers = ["default"]

[chains]
ingestor = "default"

[chains.{network}]
shard = "primary"
provider = [
  {{ label = "{network}", url = "{rpc_url}", features = [{features}] }}
]

[log_store]
backend = "file"
directory = "/data/logs"
retention_hours = 24
"#
    )
}

/// The brake graph-node does not have. Left alone on a shared endpoint it bursts, spends the
/// minute's quota in seconds and then sits in exponential backoff: on 2026-11-11 a sync of a busy
/// Base contract applied no block for ninety seconds at a time and fell behind the chain. This
/// queues requests and lets them out at a steady rate, so the endpoint never says no.
pub fn governor_conf(rpc_url: &str, rps: u32) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(rpc_url)?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("the RPC URL has no host"))?;
    Ok(format!(
        r#"events {{}}
http {{
  limit_req_zone $server_port zone=rpc:1m rate={rps}r/s;
  server {{
    listen 8545;
    client_max_body_size 32m;
    location / {{
      limit_req zone=rpc burst=4000;
      limit_req_status 429;
      proxy_pass {rpc_url};
      proxy_ssl_server_name on;
      proxy_set_header Host {host};
      proxy_http_version 1.1;
      proxy_set_header Connection "";
      proxy_read_timeout 300s;
    }}
  }}
}}
"#
    ))
}

pub fn compose_yaml(project: &str, ports: Ports, rps: u32, report: &Report) -> String {
    let range = report.max_block_range.unwrap_or(10);
    let (depends, governor) = if rps > 0 {
        (
            "[ipfs, postgres, rpc]",
            format!(
                "  rpc:\n    image: {GOVERNOR_IMAGE}\n    volumes:\n      - ./nginx.conf:/etc/nginx/nginx.conf:ro\n"
            ),
        )
    } else {
        ("[ipfs, postgres]", String::new())
    };
    format!(
        r#"name: {project}
services:
  graph-node:
    image: {GRAPH_NODE_IMAGE}
    depends_on: {depends}
    ports:
      - "127.0.0.1:{query}:8000"
      - "127.0.0.1:{admin}:8020"
      - "127.0.0.1:{status}:8030"
    volumes:
      - ./config.toml:/config.toml:ro
      - logs:/data/logs
    environment:
      GRAPH_NODE_CONFIG: /config.toml
      ipfs: "ipfs:5001"
      node_id: default
      GRAPH_LOG: info
      GRAPH_STORE_WRITE_BATCH_DURATION: "0"
      GRAPH_ETHEREUM_MAX_BLOCK_RANGE_SIZE: "{range}"
      GRAPH_DISABLE_FAIL_FAST: "true"
      ETHEREUM_REORG_THRESHOLD: "20"
      ETHEREUM_POLLING_INTERVAL: "5000"
      GRAPH_ALLOW_NON_DETERMINISTIC_FULLTEXT_SEARCH: "true"
  ipfs:
    image: {IPFS_IMAGE}
    ports:
      - "127.0.0.1:{ipfs}:5001"
    volumes:
      - ipfs:/data/ipfs
  postgres:
    image: {POSTGRES_IMAGE}
    command: ["postgres", "-cshared_preload_libraries=pg_stat_statements", "-cmax_connections=100"]
    environment:
      POSTGRES_USER: graph
      POSTGRES_PASSWORD: graph
      POSTGRES_DB: graph
      POSTGRES_INITDB_ARGS: "-E UTF8 --locale=C"
    volumes:
      - postgres:/var/lib/postgresql/data
{governor}volumes:
  logs:
  ipfs:
  postgres:
"#,
        query = ports.query,
        admin = ports.admin,
        status = ports.status,
        ipfs = ports.ipfs,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(range: Option<u64>, features: Vec<&'static str>) -> Report {
        Report {
            head: 100,
            start: 10,
            findings: vec![],
            max_block_range: range,
            features,
        }
    }

    #[test]
    fn the_config_declares_only_what_was_measured() {
        let none = config_toml(
            "base",
            "https://base-rpc.publicnode.com",
            0,
            &report(Some(2000), vec![]),
        );
        assert!(none.contains("[chains.base]"));
        assert!(none.contains(r#"url = "https://base-rpc.publicnode.com", features = []"#));
        assert!(
            none.contains("backend = \"file\""),
            "the log store is what makes _logs work"
        );
        let both = config_toml(
            "mainnet",
            "http://x",
            0,
            &report(Some(2000), vec!["archive", "traces"]),
        );
        assert!(both.contains(r#"features = ["archive", "traces"]"#));
        // It has to be TOML graph-node would accept, at least as far as parsing goes.
        assert!(both.lines().filter(|l| l.starts_with('[')).count() >= 6);
    }

    #[test]
    fn nothing_listens_beyond_the_loopback() {
        let c = compose_yaml(
            "sib-abc",
            Ports::default(),
            DEFAULT_RPS,
            &report(Some(2000), vec![]),
        );
        let published: Vec<&str> = c
            .lines()
            .filter(|l| l.trim_start().starts_with("- \"") && l.contains(':'))
            .collect();
        assert_eq!(published.len(), 4);
        assert!(
            published.iter().all(|l| l.contains("\"127.0.0.1:")),
            "graph-node's admin port has no authentication"
        );
    }

    #[test]
    fn it_carries_the_settings_the_spike_proved_and_the_measured_range() {
        let c = compose_yaml(
            "sib-abc",
            Ports {
                query: 28000,
                admin: 28020,
                status: 28030,
                ipfs: 25001,
            },
            0,
            &report(Some(100), vec![]),
        );
        assert!(
            c.contains("GRAPH_STORE_WRITE_BATCH_DURATION: \"0\""),
            "or the status API lies while syncing"
        );
        assert!(c.contains("GRAPH_ETHEREUM_MAX_BLOCK_RANGE_SIZE: \"100\""));
        assert!(c.contains("127.0.0.1:28000:8000") && c.contains("127.0.0.1:25001:5001"));
        assert!(c.contains("name: sib-abc"));
        assert!(
            c.contains("--locale=C"),
            "graph-node panics on a database without the C locale"
        );
    }

    #[test]
    fn every_image_is_pinned_and_state_lives_in_named_volumes() {
        let c = compose_yaml(
            "p",
            Ports::default(),
            DEFAULT_RPS,
            &report(Some(2000), vec![]),
        );
        for image in c.lines().filter(|l| l.trim_start().starts_with("image:")) {
            let tag = image.rsplit(':').next().unwrap();
            assert!(
                tag != "latest" && image.matches(':').count() == 2,
                "unpinned: {image}"
            );
        }
        assert!(
            c.contains("- logs:/data/logs"),
            "a bind mount leaves root-owned files in the project"
        );
        assert!(
            c.contains("- postgres:/var/lib/postgresql/data"),
            "stopping must not cost a re-index"
        );
    }

    #[test]
    fn the_governor_sits_between_graph_node_and_the_endpoint_unless_switched_off() {
        let r = report(Some(2000), vec![]);
        let governed = config_toml("base", "https://base-rpc.publicnode.com", 15, &r);
        assert!(
            governed.contains(r#"url = "http://rpc:8545""#) && !governed.contains("publicnode")
        );
        let c = compose_yaml("p", Ports::default(), 15, &r);
        assert!(
            c.contains("  rpc:\n    image: nginx:")
                && c.contains("depends_on: [ipfs, postgres, rpc]")
        );
        assert!(
            !c.contains("8545:"),
            "the governor is not published; only graph-node talks to it"
        );
        let direct = compose_yaml("p", Ports::default(), 0, &r);
        assert!(!direct.contains("nginx") && direct.contains("depends_on: [ipfs, postgres]"));
    }

    #[test]
    fn the_governor_keeps_a_key_in_the_path_and_names_the_real_host() {
        let conf = governor_conf("https://base-mainnet.g.alchemy.com/v2/abc123", 25).unwrap();
        assert!(conf.contains("rate=25r/s"));
        // nginx does not count a request whose key is empty, and `$server_name` is empty here:
        // the first version let 37 a second through a 15 a second limit.
        assert!(conf.contains("limit_req_zone $server_port "));
        assert!(conf.contains("proxy_pass https://base-mainnet.g.alchemy.com/v2/abc123;"));
        assert!(conf.contains("proxy_set_header Host base-mainnet.g.alchemy.com;"));
        assert!(
            conf.contains("proxy_ssl_server_name on;"),
            "or a TLS endpoint behind a CDN refuses the handshake"
        );
        assert!(governor_conf("not a url", 15).is_err());
    }
}
