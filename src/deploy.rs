//! Put a built subgraph on the local node: files to the local IPFS, then one admin call.
//!
//! This is what `graph deploy` does, done here so that the start block can be moved in the same
//! pass (`--last N`) and so that Drydock does not need graph-cli at run time. Nothing is uploaded
//! anywhere but the IPFS node Drydock itself started.

use anyhow::{Context, Result};
use serde_yaml_ng::{Mapping, Value};

/// Replace every `file: <path>` in a built manifest with the IPFS link graph-node expects,
/// `file: {"/": "/ipfs/<cid>"}`, uploading each distinct file once. `start` moves any data
/// source that begins before it up to it.
pub fn link_manifest(
    manifest: &str,
    start: Option<u64>,
    mut upload: impl FnMut(&str) -> Result<String>,
) -> Result<String> {
    let mut doc: Value =
        serde_yaml_ng::from_str(manifest).context("the built manifest is not valid YAML")?;
    let mut seen: Vec<(String, String)> = Vec::new();
    link(&mut doc, &mut |path: &str| {
        if let Some((_, cid)) = seen.iter().find(|(p, _)| p == path) {
            return Ok(cid.clone());
        }
        let cid = upload(path)?;
        seen.push((path.to_string(), cid.clone()));
        Ok(cid)
    })?;
    if let Some(start) = start
        && let Some(sources) = doc.get_mut("dataSources").and_then(Value::as_sequence_mut)
    {
        for source in sources
            .iter_mut()
            .filter_map(|d| d.get_mut("source"))
            .filter_map(Value::as_mapping_mut)
        {
            let current = source
                .get("startBlock")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if current < start {
                source.insert("startBlock".into(), start.into());
            }
        }
    }
    serde_yaml_ng::to_string(&doc).context("could not write the manifest back out")
}

fn link(node: &mut Value, upload: &mut impl FnMut(&str) -> Result<String>) -> Result<()> {
    match node {
        Value::Mapping(map) => {
            for (key, value) in map.iter_mut() {
                if key.as_str() == Some("file")
                    && let Value::String(path) = value
                {
                    let cid = upload(path)?;
                    let mut link = Mapping::new();
                    link.insert("/".into(), format!("/ipfs/{cid}").into());
                    *value = Value::Mapping(link);
                } else {
                    link(value, upload)?;
                }
            }
        }
        Value::Sequence(items) => {
            for item in items {
                link(item, upload)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILT: &str = r#"
specVersion: 1.2.0
schema:
  file: schema.graphql
dataSources:
  - kind: ethereum
    name: USDC
    network: base
    source:
      address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
      abi: ERC20
      startBlock: 1000
    mapping:
      abis:
        - name: ERC20
          file: USDC/ERC20.json
      file: USDC/USDC.wasm
  - kind: ethereum
    name: Late
    network: base
    source:
      abi: ERC20
      startBlock: 900000
    mapping:
      abis:
        - name: ERC20
          file: USDC/ERC20.json
      file: Late/Late.wasm
templates:
  - kind: ethereum
    name: Pool
    network: base
    source:
      abi: ERC20
    mapping:
      file: templates/Pool/Pool.wasm
"#;

    fn linked(start: Option<u64>) -> (Value, Vec<String>) {
        let mut uploaded = Vec::new();
        let out = link_manifest(BUILT, start, |p| {
            uploaded.push(p.to_string());
            Ok(format!("Qm-{}", p.replace('/', "-")))
        })
        .unwrap();
        (serde_yaml_ng::from_str(&out).unwrap(), uploaded)
    }

    #[test]
    fn every_file_becomes_a_link_and_each_is_uploaded_once() {
        let (doc, uploaded) = linked(None);
        assert_eq!(doc["schema"]["file"]["/"], "/ipfs/Qm-schema.graphql");
        assert_eq!(
            doc["dataSources"][0]["mapping"]["file"]["/"],
            "/ipfs/Qm-USDC-USDC.wasm"
        );
        assert_eq!(
            doc["dataSources"][0]["mapping"]["abis"][0]["file"]["/"],
            "/ipfs/Qm-USDC-ERC20.json"
        );
        assert_eq!(
            doc["templates"][0]["mapping"]["file"]["/"],
            "/ipfs/Qm-templates-Pool-Pool.wasm"
        );
        assert_eq!(
            uploaded.len(),
            5,
            "the ABI two data sources share goes up once: {uploaded:?}"
        );
    }

    #[test]
    fn nothing_else_in_the_manifest_changes() {
        let (doc, _) = linked(None);
        assert_eq!(doc["specVersion"], "1.2.0");
        assert_eq!(
            doc["dataSources"][0]["source"]["address"],
            "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"
        );
        assert_eq!(doc["dataSources"][0]["source"]["startBlock"], 1000);
    }

    #[test]
    fn a_window_moves_early_sources_up_and_leaves_later_ones_and_templates_alone() {
        let (doc, _) = linked(Some(500_000));
        assert_eq!(doc["dataSources"][0]["source"]["startBlock"], 500_000);
        assert_eq!(doc["dataSources"][1]["source"]["startBlock"], 900_000);
        assert!(doc["templates"][0]["source"].get("startBlock").is_none());
    }

    #[test]
    fn a_failed_upload_stops_the_deploy() {
        let err = link_manifest(BUILT, None, |p| anyhow::bail!("IPFS refused {p}")).unwrap_err();
        assert!(err.to_string().contains("IPFS refused"));
    }
}
