//! What a built subgraph asks of an RPC endpoint.
//!
//! Read from `build/subgraph.yaml` and the compiled mappings beside it, because that is what
//! graph-node reads. Its own rules are followed, not improved on: `archive` is wanted when any
//! mapping imports `ethereum.call`, whatever the start block, and `traces` when there is a call
//! handler or a block handler filtered on calls.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needs {
    pub network: String,
    /// The earliest block any data source starts at.
    pub start_block: u64,
    /// An address to ask for logs of, so a probe costs what a real request would.
    pub sample_address: Option<String>,
    /// A data source with no address, which some public endpoints refuse outright.
    pub addressless: bool,
    /// Mappings read contract state, so the endpoint must keep it back to the start block.
    pub calls: bool,
    pub traces: bool,
    /// Data sources created at runtime. A shortened window never finds the ones made before it.
    pub templates: bool,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default, rename = "dataSources")]
    data_sources: Vec<DataSource>,
    #[serde(default)]
    templates: Vec<DataSource>,
}

#[derive(Deserialize)]
struct DataSource {
    kind: String,
    #[serde(default)]
    network: Option<String>,
    #[serde(default)]
    source: Source,
    mapping: Mapping,
}

#[derive(Deserialize, Default)]
struct Source {
    #[serde(default)]
    address: Option<String>,
    #[serde(default, rename = "startBlock")]
    start_block: Option<u64>,
}

#[derive(Deserialize)]
struct Mapping {
    #[serde(default)]
    file: Option<String>,
    #[serde(default, rename = "callHandlers")]
    call_handlers: Vec<serde_yaml_ng::Value>,
    #[serde(default, rename = "blockHandlers")]
    block_handlers: Vec<BlockHandler>,
}

#[derive(Deserialize)]
struct BlockHandler {
    #[serde(default)]
    filter: Option<Filter>,
}

#[derive(Deserialize)]
struct Filter {
    kind: String,
}

/// The built manifest for a subgraph directory or manifest path: `build/subgraph.yaml` beside it.
pub fn built_manifest(path: &Path) -> PathBuf {
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(Path::new("."))
    };
    dir.join("build").join("subgraph.yaml")
}

pub fn read(built: &Path) -> Result<Needs> {
    let text = std::fs::read_to_string(built).with_context(|| {
        format!(
            "no built manifest at {}; run `graph build` first",
            built.display()
        )
    })?;
    let dir = built.parent().unwrap_or(Path::new("."));
    needs(&text, |file| std::fs::read(dir.join(file)).ok())
}

/// `wasm` hands back a mapping's bytes by the path the manifest gives, or None if it is missing.
pub fn needs(manifest: &str, wasm: impl Fn(&str) -> Option<Vec<u8>>) -> Result<Needs> {
    let m: Manifest =
        serde_yaml_ng::from_str(manifest).context("the built manifest is not valid YAML")?;
    let chain: Vec<&DataSource> = m
        .data_sources
        .iter()
        .filter(|d| d.kind.starts_with("ethereum"))
        .collect();
    if chain.is_empty() {
        bail!("the manifest has no ethereum data source; sib only runs EVM subgraphs");
    }
    let mut networks: Vec<&str> = chain.iter().filter_map(|d| d.network.as_deref()).collect();
    networks.sort_unstable();
    networks.dedup();
    let network = match networks.as_slice() {
        [one] => one.to_string(),
        [] => bail!("no data source names a network"),
        many => bail!(
            "data sources name more than one network ({}); sib runs one chain",
            many.join(", ")
        ),
    };

    let every = || chain.iter().copied().chain(m.templates.iter());
    let traces = every().any(|d| {
        !d.mapping.call_handlers.is_empty()
            || d.mapping
                .block_handlers
                .iter()
                .any(|b| b.filter.as_ref().is_some_and(|f| f.kind == "call"))
    });
    let mut files: Vec<&str> = every().filter_map(|d| d.mapping.file.as_deref()).collect();
    files.sort_unstable();
    files.dedup();
    let calls = files
        .iter()
        .any(|f| wasm(f).is_some_and(|bytes| imports_ethereum_call(&bytes)));

    Ok(Needs {
        network,
        start_block: chain
            .iter()
            .map(|d| d.source.start_block.unwrap_or(0))
            .min()
            .unwrap_or(0),
        sample_address: chain.iter().find_map(|d| d.source.address.clone()),
        addressless: chain.iter().any(|d| d.source.address.is_none()),
        calls,
        traces,
        templates: !m.templates.is_empty(),
    })
}

/// graph-node's own test for needing an archive node: the import section names `ethereum.call`.
fn imports_ethereum_call(wasm: &[u8]) -> bool {
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let Ok(wasmparser::Payload::ImportSection(section)) = payload {
            return section
                .into_imports()
                .flatten()
                .any(|i| i.name == "ethereum.call");
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENTS_ONLY: &str = r#"
dataSources:
  - kind: ethereum
    name: USDC
    network: base
    source:
      address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
      abi: ERC20
      startBlock: 56055187
    mapping:
      kind: ethereum/events
      eventHandlers:
        - event: Transfer(indexed address,indexed address,uint256)
          handler: handleTransfer
      file: USDC/USDC.wasm
"#;

    /// A module whose only content is one import, `(import "ethereum" "<name>" (func))`.
    fn wasm_importing(name: &str) -> Vec<u8> {
        let mut m = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        m.extend([0x01, 0x04, 0x01, 0x60, 0x00, 0x00]);
        let module = b"ethereum";
        let mut import = vec![0x01, module.len() as u8];
        import.extend(module);
        import.push(name.len() as u8);
        import.extend(name.as_bytes());
        import.extend([0x00, 0x00]);
        m.push(0x02);
        m.push(import.len() as u8);
        m.extend(import);
        m
    }

    #[test]
    fn an_events_only_subgraph_needs_neither_archive_nor_traces() {
        let n = needs(EVENTS_ONLY, |_| Some(wasm_importing("ethereum.encode"))).unwrap();
        assert_eq!(n.network, "base");
        assert_eq!(n.start_block, 56_055_187);
        assert_eq!(
            n.sample_address.as_deref(),
            Some("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913")
        );
        assert!(!n.calls && !n.traces && !n.addressless && !n.templates);
    }

    #[test]
    fn importing_ethereum_call_is_what_asks_for_archive() {
        assert!(
            needs(EVENTS_ONLY, |_| Some(wasm_importing("ethereum.call")))
                .unwrap()
                .calls
        );
        assert!(!needs(EVENTS_ONLY, |_| None).unwrap().calls);
    }

    #[test]
    fn call_handlers_and_call_filtered_block_handlers_ask_for_traces() {
        let with_call = EVENTS_ONLY.replace("      file:", "      callHandlers:\n        - function: mint()\n          handler: handleMint\n      file:");
        assert!(needs(&with_call, |_| None).unwrap().traces);
        let filtered = EVENTS_ONLY.replace("      file:", "      blockHandlers:\n        - handler: handleBlock\n          filter:\n            kind: call\n      file:");
        assert!(needs(&filtered, |_| None).unwrap().traces);
        let every_block = EVENTS_ONLY.replace(
            "      file:",
            "      blockHandlers:\n        - handler: handleBlock\n      file:",
        );
        assert!(!needs(&every_block, |_| None).unwrap().traces);
    }

    #[test]
    fn a_factory_starts_at_its_earliest_source_and_admits_to_templates() {
        let factory = format!(
            "{EVENTS_ONLY}  - kind: ethereum\n    name: Anything\n    network: base\n    source:\n      abi: ERC20\n      startBlock: 100\n    mapping:\n      file: Anything/Anything.wasm\ntemplates:\n  - kind: ethereum\n    name: Pool\n    network: base\n    source:\n      abi: Pool\n    mapping:\n      file: templates/Pool/Pool.wasm\n"
        );
        let n = needs(&factory, |f| {
            (f == "templates/Pool/Pool.wasm").then(|| wasm_importing("ethereum.call"))
        })
        .unwrap();
        assert_eq!(n.start_block, 100);
        assert!(n.addressless && n.templates && n.calls);
    }

    #[test]
    fn two_networks_or_no_chain_are_refused_in_words() {
        let two = format!(
            "{EVENTS_ONLY}  - kind: ethereum\n    name: B\n    network: mainnet\n    source: {{}}\n    mapping: {{}}\n"
        );
        assert!(
            needs(&two, |_| None)
                .unwrap_err()
                .to_string()
                .contains("more than one network")
        );
        assert!(
            needs("dataSources: []", |_| None)
                .unwrap_err()
                .to_string()
                .contains("no ethereum data source")
        );
    }
}
