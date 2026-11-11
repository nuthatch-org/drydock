//! The graph-node stack Drydock runs, as two files: a compose file and graph-node's own config.
//!
//! Every setting here is one that a default graph-node gets wrong for a developer on a shared
//! endpoint, and each was either measured in `docs/spike.md` or read out of graph-node's source.

use crate::preflight::Report;

pub const GRAPH_NODE_IMAGE: &str = "graphprotocol/graph-node:v0.45.0";
pub const POSTGRES_IMAGE: &str = "postgres:16";
pub const IPFS_IMAGE: &str = "ipfs/kubo:v0.38.1";
pub const GOVERNOR_IMAGE: &str = "nginx:1.29-alpine";

/// Requests a second Drydock lets through to the endpoint unless told otherwise. publicnode's cap,
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
    /// The page.
    pub ui: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            query: 8000,
            admin: 8020,
            status: 8030,
            ipfs: 5001,
            ui: 8787,
        }
    }
}

/// The same endpoint as a container must spell it. `localhost` inside a container is the
/// container, so an RPC on the developer's own machine (Anvil, Hardhat, a local node) is reached
/// through `host.docker.internal`, which the compose file maps to the host. The second value says
/// whether it was rewritten.
pub fn from_container(rpc_url: &str) -> (String, bool) {
    let Ok(mut url) = reqwest::Url::parse(rpc_url) else {
        return (rpc_url.to_string(), false);
    };
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "0.0.0.0")
    );
    if loopback && url.set_host(Some("host.docker.internal")).is_ok() {
        return (url.to_string().trim_end_matches('/').to_string(), true);
    }
    (rpc_url.to_string(), false)
}

/// What to do when the containers cannot reach an RPC that this machine can. Docker Desktop
/// reaches the host's loopback; Docker on Linux reaches the host by its bridge address, where a
/// server bound to 127.0.0.1 is not listening.
pub fn local_rpc_fix(linux: bool) -> &'static str {
    if linux {
        "On Linux, containers reach this machine at Docker's bridge address, not at 127.0.0.1, so the RPC has to listen on all interfaces: `anvil --host 0.0.0.0`, or `hardhat node --hostname 0.0.0.0`."
    } else {
        "Check that it is still running, and that Docker Desktop allows containers to reach the host."
    }
}

/// Why graph-node is not answering, from its container's state, or `None` if it is running.
/// graph-node ships for x86-64 only; on Apple Silicon it runs emulated, and under QEMU it was
/// killed for memory within a minute of starting to sync (2026-11-11, colima, 6 GB).
pub fn why_stopped(state: &str, oom_killed: bool, exit_code: i64, arm: bool) -> Option<String> {
    if state == "running" {
        return None;
    }
    Some(if oom_killed {
        let mut s = String::from("graph-node ran out of memory and was killed.");
        if arm {
            s.push_str(" graph-node is built for x86-64 only and is being emulated on this machine. With Docker Desktop, turn on Rosetta under Settings > General; QEMU emulation, which colima uses by default, does not survive a sync.");
        } else {
            s.push_str(" Give Docker more memory, then `drydock up` again.");
        }
        s
    } else {
        format!(
            "graph-node is {state} (exit code {exit_code}). `drydock logs` has its last words; `drydock up` starts it again."
        )
    })
}

/// graph-node's `--config` file. Config mode is the only one that can switch the log store on,
/// which is what puts mapping logs behind `_logs` on the query port.
pub fn config_toml(network: &str, rpc_url: &str, rps: u32, report: &Report) -> String {
    let direct = from_container(rpc_url).0;
    let rpc_url = if rps > 0 { GOVERNED_URL } else { &direct };
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

/// A file `up` writes beside the compose file and copies into a container.
pub struct Copied {
    pub file: &'static str,
    pub service: &'static str,
    pub dest: &'static str,
    /// Read once at start, so the service is restarted when it changes.
    pub restart: bool,
}

pub const COPIED: [Copied; 5] = [
    Copied {
        file: "config.toml",
        service: "graph-node",
        dest: "/config.toml",
        restart: true,
    },
    Copied {
        file: "nginx.conf",
        service: "rpc",
        dest: "/etc/nginx/nginx.conf",
        restart: true,
    },
    Copied {
        file: "ui.conf",
        service: "ui",
        dest: "/etc/nginx/nginx.conf",
        restart: true,
    },
    Copied {
        file: "index.html",
        service: "ui",
        dest: "/usr/share/nginx/html/index.html",
        restart: false,
    },
    Copied {
        file: "drydock.json",
        service: "ui",
        dest: "/usr/share/nginx/html/drydock.json",
        restart: false,
    },
];

/// The page, compiled into the binary. One file, no requests to anywhere but its own origin.
pub const UI_HTML: &str = include_str!("../ui/index.html");

/// Serves the page and puts graph-node's query and status ports under the same origin, so the
/// page needs no CORS and no knowledge of where the node is. A hosted Drydock can keep the page as
/// it is and change only this.
pub fn ui_conf() -> &'static str {
    r#"events {}
http {
  include /etc/nginx/mime.types;
  server {
    listen 8787;
    root /usr/share/nginx/html;
    index index.html;
    add_header Cache-Control "no-store";
    proxy_connect_timeout 3s;
    location /query/ {
      proxy_pass http://graph-node:8000/;
      proxy_read_timeout 120s;
    }
    location /status/ {
      proxy_pass http://graph-node:8030/;
      proxy_read_timeout 10s;
    }
  }
}
"#
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
    let (upstream, local) = from_container(rpc_url);
    // A remote endpoint behind a CDN is chosen by its Host header. A local one is not, and
    // would be told the wrong name.
    let host_header = if local {
        String::new()
    } else {
        format!("      proxy_set_header Host {host};\n")
    };
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
      proxy_pass {upstream};
      proxy_ssl_server_name on;
{host_header}      proxy_http_version 1.1;
      proxy_set_header Connection "";
      proxy_connect_timeout 5s;
      proxy_next_upstream error timeout;
      proxy_read_timeout 300s;
    }}
  }}
}}
"#
    ))
}

pub fn compose_yaml(project: &str, ports: Ports, rps: u32, report: &Report) -> String {
    compose_yaml_with(project, ports, rps, report, None)
}

/// As [`compose_yaml`], with graph-node from another image: `DRYDOCK_GRAPH_NODE_IMAGE`. The
/// published image is x86-64 only, so it is pinned to that platform; one named by hand is
/// taken to be built for the machine it is on.
pub fn compose_yaml_with(
    project: &str,
    ports: Ports,
    rps: u32,
    report: &Report,
    graph_node_image: Option<&str>,
) -> String {
    let log_addresses = report
        .max_log_addresses
        .map(|n| format!("\n      GRAPH_ETH_GET_LOGS_MAX_CONTRACTS: \"{n}\""))
        .unwrap_or_default();
    let graph_node = match graph_node_image {
        Some(image) => format!("image: {image}"),
        None => format!("image: {GRAPH_NODE_IMAGE}\n    platform: linux/amd64"),
    };
    let range = report.max_block_range.unwrap_or(10);
    let (depends, governor) = if rps > 0 {
        (
            "[ipfs, postgres, rpc]",
            format!(
                "  rpc:\n    image: {GOVERNOR_IMAGE}\n    extra_hosts:\n      - \"host.docker.internal:host-gateway\"\n"
            ),
        )
    } else {
        ("[ipfs, postgres]", String::new())
    };
    format!(
        r#"name: {project}
services:
  graph-node:
    {graph_node}
    depends_on: {depends}
    extra_hosts:
      - "host.docker.internal:host-gateway"
    ports:
      - "127.0.0.1:{query}:8000"
      - "127.0.0.1:{admin}:8020"
      - "127.0.0.1:{status}:8030"
    volumes:
      - logs:/data/logs
    environment:
      GRAPH_NODE_CONFIG: /config.toml
      ipfs: "ipfs:5001"
      node_id: default
      GRAPH_LOG: info
      GRAPH_STORE_WRITE_BATCH_DURATION: "0"
      GRAPH_ETHEREUM_MAX_BLOCK_RANGE_SIZE: "{range}"{log_addresses}
      GRAPH_DISABLE_FAIL_FAST: "true"
      ETHEREUM_REORG_THRESHOLD: "20"
      ETHEREUM_POLLING_INTERVAL: "5000"
      GRAPH_ALLOW_NON_DETERMINISTIC_FULLTEXT_SEARCH: "true"
  ipfs:
    image: {IPFS_IMAGE}
    command: ["daemon", "--migrate=true", "--offline"]
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
  ui:
    image: {GOVERNOR_IMAGE}
    depends_on: [graph-node]
    extra_hosts:
      - "host.docker.internal:host-gateway"
    ports:
      - "127.0.0.1:{ui}:8787"
{governor}volumes:
  logs:
  ipfs:
  postgres:
"#,
        query = ports.query,
        admin = ports.admin,
        status = ports.status,
        ipfs = ports.ipfs,
        ui = ports.ui,
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
            max_log_addresses: None,
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

    /// Kubo's default is to join the public swarm and announce what it holds. A subgraph under
    /// test is not published, and "nothing is uploaded anywhere" has to be true.
    #[test]
    fn the_ipfs_node_stays_off_the_network() {
        let c = compose_yaml(
            "drydock-abc",
            Ports::default(),
            DEFAULT_RPS,
            &report(Some(2000), vec![]),
        );
        assert!(c.contains(r#"command: ["daemon", "--migrate=true", "--offline"]"#));
    }

    #[test]
    fn another_graph_node_image_is_not_pinned_to_x86() {
        let r = report(Some(2000), vec![]);
        let stock = compose_yaml("p", Ports::default(), 15, &r);
        assert!(stock.contains(&format!(
            "  graph-node:\n    image: {GRAPH_NODE_IMAGE}\n    platform: linux/amd64\n    depends_on"
        )));
        let own = compose_yaml_with("p", Ports::default(), 15, &r, Some("me/graph-node:arm64"));
        assert!(own.contains("  graph-node:\n    image: me/graph-node:arm64\n    depends_on"));
        assert!(!own.contains("platform:"));
    }

    /// A file mounted from the host only exists in the container if the Docker VM shares that
    /// folder. colima shares the home directory and nothing else, and a subgraph kept anywhere
    /// else failed to start on 2026-11-11. The files are copied in instead.
    #[test]
    fn nothing_is_mounted_from_the_host() {
        let c = compose_yaml("p", Ports::default(), 15, &report(Some(2000), vec![]));
        assert!(!c.contains("./"), "{c}");
        assert_eq!(COPIED.iter().filter(|f| f.service == "ui").count(), 3);
    }

    #[test]
    fn graph_node_is_told_the_endpoints_address_cap() {
        let mut r = report(Some(2000), vec![]);
        assert!(!compose_yaml("p", Ports::default(), 15, &r).contains("GET_LOGS_MAX_CONTRACTS"));
        r.max_log_addresses = Some(10);
        assert!(
            compose_yaml("p", Ports::default(), 15, &r)
                .contains("      GRAPH_ETH_GET_LOGS_MAX_CONTRACTS: \"10\"\n")
        );
    }

    #[test]
    fn why_it_stopped_names_memory_and_emulation() {
        assert!(why_stopped("running", false, 0, true).is_none());
        let oom = why_stopped("exited", true, 137, false).unwrap();
        assert!(oom.contains("ran out of memory"), "{oom}");
        assert!(!oom.contains("Rosetta"));
        assert!(
            why_stopped("exited", true, 137, true)
                .unwrap()
                .contains("Rosetta")
        );
        assert!(
            why_stopped("exited", false, 1, false)
                .unwrap()
                .contains("drydock logs")
        );
    }

    #[test]
    fn nothing_listens_beyond_the_loopback() {
        let c = compose_yaml(
            "drydock-abc",
            Ports::default(),
            DEFAULT_RPS,
            &report(Some(2000), vec![]),
        );
        let published: Vec<&str> = c
            .lines()
            .filter(|l| {
                l.trim_start().starts_with("- \"") && l.contains(':') && !l.contains("host-gateway")
            })
            .collect();
        assert_eq!(published.len(), 5);
        assert!(
            published.iter().all(|l| l.contains("\"127.0.0.1:")),
            "graph-node's admin port has no authentication"
        );
    }

    #[test]
    fn it_carries_the_settings_the_spike_proved_and_the_measured_range() {
        let c = compose_yaml(
            "drydock-abc",
            Ports {
                query: 28000,
                admin: 28020,
                status: 28030,
                ipfs: 25001,
                ui: 28787,
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
        assert!(c.contains("name: drydock-abc"));
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
        assert!(!direct.contains("  rpc:\n") && direct.contains("depends_on: [ipfs, postgres]"));
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

    #[test]
    fn the_page_is_served_beside_the_node_it_reads() {
        let c = compose_yaml("p", Ports::default(), 0, &report(Some(2000), vec![]));
        assert!(c.contains("127.0.0.1:8787:8787"));
        let conf = ui_conf();
        assert!(
            conf.contains("location /query/")
                && conf.contains("proxy_pass http://graph-node:8000/;")
        );
        assert!(
            conf.contains("location /status/")
                && conf.contains("proxy_pass http://graph-node:8030/;")
        );
    }

    #[test]
    fn the_page_asks_nothing_of_any_other_origin() {
        // It has to work on a train, and a hosted Drydock must be able to serve it unchanged.
        for needle in ["https://", "http://"] {
            let outside: Vec<&str> = UI_HTML
                .match_indices(needle)
                .map(|(i, _)| &UI_HTML[i..(i + 60).min(UI_HTML.len())])
                .filter(|s| {
                    !s.starts_with("https://www.lodestar-dashboard.com/move")
                        && !s.starts_with("http://localhost:'")
                })
                .collect();
            assert!(
                outside.is_empty(),
                "the page reaches outside its origin: {outside:?}"
            );
        }
        assert!(
            UI_HTML.contains("fetch('drydock.json'")
                && UI_HTML.contains("'status/graphql'")
                && UI_HTML.contains("'query/subgraphs/name/'")
        );
        assert!(!UI_HTML.contains('\u{2014}'), "no em dashes");
    }

    #[test]
    fn an_rpc_on_the_developers_machine_is_reached_through_the_host() {
        assert_eq!(
            from_container("http://localhost:8545"),
            ("http://host.docker.internal:8545".into(), true)
        );
        assert_eq!(
            from_container("http://127.0.0.1:8545/rpc"),
            ("http://host.docker.internal:8545/rpc".into(), true)
        );
        assert_eq!(
            from_container("https://base-rpc.publicnode.com"),
            ("https://base-rpc.publicnode.com".into(), false)
        );
        let r = report(Some(2000), vec![]);
        assert!(
            config_toml("mainnet", "http://localhost:8545", 0, &r)
                .contains(r#"url = "http://host.docker.internal:8545""#)
        );
        let conf = governor_conf("http://127.0.0.1:8545", 15).unwrap();
        assert!(conf.contains("proxy_pass http://host.docker.internal:8545;"));
        assert!(
            !conf.contains("proxy_set_header Host"),
            "a local node is not behind a CDN"
        );
        let c = compose_yaml("p", Ports::default(), 15, &r);
        assert_eq!(
            c.matches("host.docker.internal:host-gateway").count(),
            3,
            "graph-node, the governor and the container that checks it all need the mapping"
        );
    }

    #[test]
    fn linux_is_told_to_listen_on_every_interface() {
        assert!(local_rpc_fix(true).contains("anvil --host 0.0.0.0"));
        assert!(!local_rpc_fix(false).contains("0.0.0.0"));
    }
}
