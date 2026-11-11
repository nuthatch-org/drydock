use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use sib::manifest;
use sib::preflight::{self, Level};
use sib::rpc::{self, HttpRpc};

#[derive(Parser)]
#[command(
    name = "sib",
    version,
    about = "Studio in a box: run a real graph-node on your own machine against your subgraph"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Ask an RPC endpoint whether it can carry this subgraph, before anything is started
    Preflight {
        /// The endpoint, as `network:url`, e.g. base:https://base-rpc.publicnode.com
        #[arg(long)]
        rpc: String,
        /// The subgraph directory, or its subgraph.yaml. It must have been built
        #[arg(default_value = ".")]
        subgraph: PathBuf,
        /// Start this many blocks back from the head, where the manifest's start block is too old for the endpoint
        #[arg(long, value_name = "BLOCKS")]
        last: Option<u64>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sib: {e:#}");
            ExitCode::from(2)
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Preflight {
            rpc,
            subgraph,
            last,
        } => {
            let (network, url) = rpc::parse_spec(&rpc).with_context(|| format!("--rpc wants network:url, e.g. base:https://base-rpc.publicnode.com; got {rpc:?}"))?;
            let needs = manifest::read(&manifest::built_manifest(&subgraph))?;
            if needs.network != network {
                bail!(
                    "the subgraph is on {:?} and --rpc names {network:?}",
                    needs.network
                );
            }
            let report = preflight::run(&HttpRpc::new(&url), &needs, last)
                .await
                .map_err(|e| anyhow::anyhow!("the endpoint did not answer: {}", e.message()))?;

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
                return Ok(ExitCode::from(1));
            }
            println!("This endpoint can carry it.");
            Ok(ExitCode::SUCCESS)
        }
    }
}
