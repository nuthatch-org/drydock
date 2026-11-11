use std::path::{Path, PathBuf};
use std::process::{Command as Process, ExitCode};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

use drydock::deploy;
use drydock::manifest::{self, Needs};
use drydock::node::{self, Node};
use drydock::preflight::{self, Level, Report, Window};
use drydock::rpc::{self, HttpRpc};
use drydock::stack::{self, Ports};

#[derive(Parser)]
#[command(
    name = "drydock",
    version,
    about = "Lodestar Drydock: run a real graph-node on your own machine against your subgraph"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ask an RPC endpoint whether it can carry this subgraph, before anything is started
    Preflight {
        #[command(flatten)]
        target: Target,
    },
    /// Check the endpoint, start a local graph-node, and deploy the built subgraph to it
    Up {
        #[command(flatten)]
        target: Target,
        /// Add this to every port, when 8000, 8020, 8030 and 5001 are taken
        #[arg(long, default_value_t = 0)]
        port_offset: u16,
        /// Requests a second let through to the endpoint. 0 removes the limit, for an endpoint you pay for
        #[arg(long, default_value_t = stack::DEFAULT_RPS)]
        rps: u32,
    },
    /// Where the sync has got to, and the error if it has failed
    Status {
        #[arg(default_value = ".")]
        subgraph: PathBuf,
    },
    /// graph-node's own output
    Logs {
        #[arg(default_value = ".")]
        subgraph: PathBuf,
        #[arg(long, default_value_t = 200)]
        tail: u32,
    },
    /// Stop the stack. The database is kept, so the next `up` resumes
    Down {
        #[arg(default_value = ".")]
        subgraph: PathBuf,
        /// Delete the database, the logs and the local IPFS as well
        #[arg(long)]
        wipe: bool,
    },
}

#[derive(clap::Args)]
struct Target {
    /// The endpoint, as `network:url`, e.g. base:https://base-rpc.publicnode.com
    #[arg(long)]
    rpc: String,
    /// The subgraph directory, or its subgraph.yaml. It must have been built
    #[arg(default_value = ".")]
    subgraph: PathBuf,
    /// Start this many blocks back from the head, where the manifest's start block is too old for the endpoint
    #[arg(long, value_name = "BLOCKS")]
    last: Option<u64>,
}

/// What `up` leaves behind for the other commands, in `.drydock/state.json`.
#[derive(Serialize, Deserialize)]
struct State {
    name: String,
    project: String,
    query: u16,
    admin: u16,
    status: u16,
    ipfs: u16,
    #[serde(default = "default_ui_port")]
    ui: u16,
    /// The `--last` this stack was started with, and the block that came to. Kept so the next
    /// `up` deploys the same window and resumes, where measuring again would start a new one.
    #[serde(default)]
    last: Option<u64>,
    #[serde(default)]
    start: Option<u64>,
}

fn default_ui_port() -> u16 {
    Ports::default().ui
}

impl State {
    fn ports(&self) -> Ports {
        Ports {
            query: self.query,
            admin: self.admin,
            status: self.status,
            ipfs: self.ipfs,
            ui: self.ui,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    // Rust ignores SIGPIPE, so `drydock status | head -1` would panic on the second line. Restoring
    // the default lets the process end quietly when whatever was reading it goes away.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("drydock: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn subgraph_dir(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(Path::new(".")).to_path_buf()
    }
}

fn state(subgraph: &Path) -> Result<(PathBuf, State)> {
    let dir = subgraph_dir(subgraph).join(".drydock");
    let text = std::fs::read_to_string(dir.join("state.json")).with_context(|| {
        format!(
            "no drydock stack for {}; run `drydock up` there first",
            subgraph.display()
        )
    })?;
    Ok((dir, serde_json::from_str(&text)?))
}

/// Write a file only if its content differs, and say whether it did.
fn changed(path: &Path, content: &str) -> Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(false);
    }
    std::fs::write(path, content)?;
    Ok(true)
}

/// One `eth_chainId` from inside the stack, using the page's container because it has `wget`.
fn reachable_from_container(dir: &Path, url: &str) -> bool {
    Process::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(dir.join("docker-compose.yml"))
        .args([
            "exec",
            "-T",
            "ui",
            "wget",
            "-q",
            "-T",
            "8",
            "-O",
            "/dev/null",
            "--header",
            "Content-Type: application/json",
        ])
        .arg("--post-data")
        .arg(r#"{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}"#)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn compose(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Process::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(dir.join("docker-compose.yml"))
        .args(args)
        .status()
        .context("could not run `docker compose`; Drydock needs Docker installed and running")?;
    if !status.success() {
        bail!("`docker compose {}` failed", args.join(" "));
    }
    Ok(())
}

/// The preflight, printed. Returns the report only when the endpoint can carry the subgraph.
async fn checked(
    target: &Target,
    pinned: Option<(u64, u64)>,
) -> Result<Option<(String, String, Needs, Report)>> {
    let (network, url) = rpc::parse_spec(&target.rpc).with_context(|| {
        format!(
            "--rpc wants network:url, e.g. base:https://base-rpc.publicnode.com; got {:?}",
            target.rpc
        )
    })?;
    let needs = manifest::read(&manifest::built_manifest(&target.subgraph))?;
    if needs.network != network {
        bail!(
            "the subgraph is on {:?} and --rpc names {network:?}",
            needs.network
        );
    }
    let window = match (target.last, pinned) {
        (Some(n), Some((last, start))) if n == last => Window::From(start),
        (Some(n), _) => Window::Last(n),
        (None, _) => Window::Whole,
    };
    let report = preflight::run(&HttpRpc::new(&url), &needs, window)
        .await
        .map_err(|e| match e {
            rpc::RpcError::RateLimited(m) => anyhow!("the endpoint is rate-limiting this machine right now (\"{m}\"). Wait a minute and try again"),
            e => anyhow!("the endpoint did not answer: {}", e.message()),
        })?;

    println!(
        "{network}: head {}, syncing from {} ({} blocks)\n",
        report.head,
        report.start,
        report.head.saturating_sub(report.start)
    );
    for f in &report.findings {
        let mark = match f.level {
            Level::Blocker => "✗",
            Level::Warn => "!",
            Level::Ok => "✓",
        };
        println!("  {mark} {}", f.text);
    }
    println!();
    if report.blocked() {
        println!("This endpoint cannot carry this subgraph as it stands.");
        return Ok(None);
    }
    Ok(Some((network, url, needs, report)))
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Preflight { target } => Ok(match checked(&target, None).await? {
            Some(_) => {
                println!("This endpoint can carry it.");
                ExitCode::SUCCESS
            }
            None => ExitCode::from(1),
        }),

        Command::Up {
            target,
            port_offset,
            rps,
        } => {
            let previous = state(&target.subgraph).ok().map(|(_, s)| s);
            let pinned = previous.as_ref().and_then(|s| Some((s.last?, s.start?)));
            let Some((network, url, needs, report)) = checked(&target, pinned).await? else {
                return Ok(ExitCode::from(1));
            };
            let root = subgraph_dir(&target.subgraph).canonicalize()?;
            let dir = root.join(".drydock");
            std::fs::create_dir_all(&dir)?;
            let slug: String = root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("subgraph")
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect();
            let d = Ports::default();
            let ports = Ports {
                query: d.query + port_offset,
                admin: d.admin + port_offset,
                status: d.status + port_offset,
                ipfs: d.ipfs + port_offset,
                ui: d.ui + port_offset,
            };
            let state = State {
                name: format!("local/{slug}"),
                project: format!("drydock-{slug}"),
                query: ports.query,
                admin: ports.admin,
                status: ports.status,
                ipfs: ports.ipfs,
                ui: ports.ui,
                last: target.last,
                start: (report.start > needs.start_block).then_some(report.start),
            };
            std::fs::write(
                dir.join("config.toml"),
                stack::config_toml(&network, &url, rps, &report),
            )?;
            std::fs::write(
                dir.join("docker-compose.yml"),
                stack::compose_yaml(&state.project, ports, rps, &report),
            )?;
            std::fs::write(
                dir.join("state.json"),
                serde_json::to_string_pretty(&state)?,
            )?;
            std::fs::write(dir.join(".gitignore"), "*\n")?;
            std::fs::create_dir_all(dir.join("ui"))?;
            // Docker does not restart a container when a file mounted into it changes.
            let mut stale = Vec::new();
            if changed(&dir.join("ui.conf"), stack::ui_conf())? {
                stale.push("ui");
            }
            std::fs::write(dir.join("ui").join("index.html"), stack::UI_HTML)?;
            let page_data = |hash: &str| {
                let findings: Vec<_> = report
                    .findings
                    .iter()
                    .map(|f| {
                        let level = match f.level {
                            Level::Blocker => "blocker",
                            Level::Warn => "warn",
                            Level::Ok => "ok",
                        };
                        serde_json::json!({ "level": level, "text": f.text })
                    })
                    .collect();
                serde_json::json!({ "name": state.name, "network": network, "hash": hash, "queryPort": ports.query, "rps": rps, "findings": findings }).to_string()
            };
            std::fs::write(dir.join("ui").join("drydock.json"), page_data(""))?;

            if rps > 0 {
                if changed(&dir.join("nginx.conf"), &stack::governor_conf(&url, rps)?)? {
                    stale.push("rpc");
                }
                println!(
                    "Starting graph-node, Postgres and IPFS, holding requests to the endpoint at {rps} a second (--rps)…"
                );
            } else {
                println!(
                    "Starting graph-node, Postgres and IPFS, with no limit on requests to the endpoint…"
                );
            }
            compose(&dir, &["up", "-d", "--quiet-pull"])?;
            if previous.is_some() && !stale.is_empty() {
                let mut args = vec!["restart"];
                args.extend(stale.iter());
                compose(&dir, &args)?;
            }
            let node = Node::new(ports);
            node.wait_ready(Duration::from_secs(180)).await?;

            // The preflight ran on this machine, where localhost is this machine. Ask again from
            // where graph-node will ask.
            let (inside, local) = stack::from_container(&url);
            if local && !reachable_from_container(&dir, &inside) {
                bail!(
                    "this machine can reach your RPC at {url}, but the containers cannot.\n{}\nThe stack is still up: fix the RPC and run `drydock up` again, or stop it with `drydock down`",
                    stack::local_rpc_fix(cfg!(target_os = "linux"))
                );
            }

            // Two passes over the manifest: one to learn which files it names, one to link them.
            let built = manifest::built_manifest(&target.subgraph);
            let build_dir = built.parent().unwrap_or(Path::new("."));
            let text = std::fs::read_to_string(&built)?;
            let window = (report.start > needs.start_block).then_some(report.start);
            let mut files = Vec::new();
            deploy::link_manifest(&text, None, |p| {
                files.push(p.to_string());
                Ok(String::new())
            })?;
            let mut cids = std::collections::HashMap::new();
            for file in &files {
                let bytes = std::fs::read(build_dir.join(file)).with_context(|| {
                    format!("the manifest names {file}, which is not in the build")
                })?;
                cids.insert(file.clone(), node.ipfs_add(file, bytes).await?);
            }
            let linked = deploy::link_manifest(&text, window, |p| {
                cids.get(p)
                    .cloned()
                    .ok_or_else(|| anyhow!("{p} was not uploaded"))
            })?;
            let hash = node.ipfs_add("subgraph.yaml", linked.into_bytes()).await?;
            node.deploy(&state.name, &hash).await?;
            std::fs::write(dir.join("ui").join("drydock.json"), page_data(&hash))?;

            println!("\nDeployed {hash} as {}.", state.name);
            println!("  Open:   http://localhost:{}", ports.ui);
            println!(
                "  Query:  http://localhost:{}/subgraphs/name/{}",
                ports.query, state.name
            );
            println!("  Status: drydock status");
            println!("  Stop:   drydock down");
            Ok(ExitCode::SUCCESS)
        }

        Command::Status { subgraph } => {
            let (_, state) = state(&subgraph)?;
            match Node::new(state.ports()).status(&state.name).await? {
                Some(s) => {
                    println!("{}", node::describe(&s));
                    Ok(if s.fatal_error.is_some() {
                        ExitCode::from(1)
                    } else {
                        ExitCode::SUCCESS
                    })
                }
                None => bail!("graph-node has nothing deployed as {}", state.name),
            }
        }

        Command::Logs { subgraph, tail } => {
            let (dir, _) = state(&subgraph)?;
            compose(&dir, &["logs", "--tail", &tail.to_string(), "graph-node"])?;
            Ok(ExitCode::SUCCESS)
        }

        Command::Down { subgraph, wipe } => {
            let (dir, _) = state(&subgraph)?;
            compose(&dir, if wipe { &["down", "-v"] } else { &["down"] })?;
            println!(
                "{}",
                if wipe {
                    "Stopped, and the database, logs and local IPFS are deleted."
                } else {
                    "Stopped. The database is kept; `drydock up` resumes it."
                }
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}
