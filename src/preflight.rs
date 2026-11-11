//! Ask an RPC endpoint, before a sync starts, whether it can carry this subgraph.
//!
//! graph-node does not ask. It takes capabilities on trust, and what an endpoint cannot do shows
//! up later as a restart loop, a stall or a rate-limit storm with nothing in the status to say
//! which. Public limits also move: the figures measured on 2026-11-11 differ from September's.
//! So nothing here is a table of providers. Every answer is asked for, every time.

use std::time::Duration;

use serde_json::{Value, json};

use crate::manifest::Needs;
use crate::rpc::{Rpc, RpcError, from_hex, hex};

/// Block ranges tried for `eth_getLogs`, widest first. The first one accepted is the cap.
const RANGES: [u64; 9] = [100_000, 50_000, 10_000, 5_000, 2_000, 1_000, 500, 100, 10];
/// graph-node's own ceiling (`GRAPH_ETHEREUM_MAX_BLOCK_RANGE_SIZE`), which Drydock never raises.
const GRAPH_NODE_MAX_RANGE: u64 = 2_000;
/// Kept clear of the head so a probe is not answered differently by two backends mid-reorg.
const HEAD_MARGIN: u64 = 64;
/// A topic no event has, so a range probe measures the range cap and not the size of the answer.
const NO_SUCH_TOPIC: &str = "0x00000000000000000000000000000000000000000000000000000000deadbeef";
const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Warn,
    /// The sync cannot finish on this endpoint.
    Blocker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub level: Level,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub head: u64,
    /// Where the sync will start: the manifest's start block, or `--last N` back from the head.
    pub start: u64,
    pub findings: Vec<Finding>,
    /// For `GRAPH_ETHEREUM_MAX_BLOCK_RANGE_SIZE`. None when no range was accepted at all.
    pub max_block_range: Option<u64>,
    /// What to declare to graph-node for this provider. Measured, where graph-node would assume.
    pub features: Vec<&'static str>,
}

/// Which blocks to sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// From the manifest's own start block.
    Whole,
    /// This many blocks back from the head, as asked for with `--last`.
    Last(u64),
    /// From this block: a `--last` window as it was first measured. The head moves, so asking
    /// for "the last N" twice gives two start blocks, two deployments and a re-index.
    From(u64),
}

impl Report {
    pub fn blocked(&self) -> bool {
        self.findings.iter().any(|f| f.level == Level::Blocker)
    }
}

/// One call, tried again once after a pause if the endpoint says it is rate-limiting: a probe
/// that lands in a bad second should not condemn a method.
async fn ask(
    rpc: &impl Rpc,
    pause: Duration,
    limited: &mut Option<String>,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    match rpc.call(method, params.clone()).await {
        Err(RpcError::RateLimited(m)) => {
            limited.get_or_insert(m);
            tokio::time::sleep(pause).await;
            rpc.call(method, params).await
        }
        other => other,
    }
}

fn logs_filter(address: Option<&str>, from: u64, to: u64) -> Value {
    let mut f = json!({ "fromBlock": hex(from), "toBlock": hex(to), "topics": [NO_SUCH_TOPIC] });
    if let Some(a) = address {
        f["address"] = json!(a);
    }
    json!([f])
}

pub async fn run(rpc: &impl Rpc, needs: &Needs, window: Window) -> Result<Report, RpcError> {
    run_with_pause(rpc, needs, window, Duration::from_secs(3)).await
}

pub async fn run_with_pause(
    rpc: &impl Rpc,
    needs: &Needs,
    window: Window,
    pause: Duration,
) -> Result<Report, RpcError> {
    let mut limited = None;
    let mut findings = Vec::new();
    macro_rules! note {
        ($level:expr, $text:expr $(,)?) => {
            findings.push(Finding {
                level: $level,
                text: $text,
            })
        };
    }

    let head = from_hex(&ask(rpc, pause, &mut limited, "eth_blockNumber", json!([])).await?)
        .ok_or_else(|| {
            RpcError::Transport("eth_blockNumber did not answer with a block number".into())
        })?;
    let start = match window {
        Window::Last(n) => head.saturating_sub(n).max(needs.start_block),
        Window::From(block) => block.max(needs.start_block),
        Window::Whole => needs.start_block,
    };
    let windowed = window != Window::Whole;
    let to_sync = head.saturating_sub(start);
    let tip = head.saturating_sub(HEAD_MARGIN);
    let address = needs.sample_address.as_deref();

    // The widest eth_getLogs range it accepts.
    let mut accepted = None;
    let mut refusal = None;
    for range in RANGES {
        match ask(
            rpc,
            pause,
            &mut limited,
            "eth_getLogs",
            logs_filter(
                address.or(Some(ZERO_ADDRESS)),
                tip.saturating_sub(range - 1),
                tip,
            ),
        )
        .await
        {
            Ok(_) => {
                accepted = Some(range);
                break;
            }
            Err(e) => refusal = Some(e),
        }
    }
    let max_block_range = accepted.map(|r| r.min(GRAPH_NODE_MAX_RANGE));
    match (accepted, &refusal) {
        (Some(r), _) if r >= GRAPH_NODE_MAX_RANGE => note!(
            Level::Ok,
            format!("Logs: accepts {r}-block ranges, more than graph-node asks for.")
        ),
        (Some(r), _) => {
            let requests = to_sync.div_ceil(r);
            note!(
                Level::Warn,
                format!(
                    "Logs: capped at {r} blocks a request, so {to_sync} blocks is at least {requests} requests. Drydock will hold graph-node to {r}; left alone, graph-node v0.45.0 asks for more and loops."
                ),
            )
        }
        (None, Some(RpcError::RateLimited(m))) => note!(
            Level::Blocker,
            format!(
                "Logs: every eth_getLogs was rate-limited, even for 10 blocks (\"{m}\"). This endpoint cannot index anything."
            )
        ),
        (None, Some(e)) => note!(
            Level::Blocker,
            format!(
                "Logs: eth_getLogs was refused at every range down to 10 blocks (\"{}\").",
                e.message()
            )
        ),
        (None, None) => unreachable!("RANGES is not empty"),
    }

    if let Some(range) = accepted {
        // History: can it serve logs from where the sync starts?
        let span = range.min(10);
        match ask(
            rpc,
            pause,
            &mut limited,
            "eth_getLogs",
            logs_filter(
                address.or(Some(ZERO_ADDRESS)),
                start,
                (start + span - 1).min(tip),
            ),
        )
        .await
        {
            Ok(_) => note!(
                Level::Ok,
                format!("History: serves logs from block {start}, {to_sync} blocks back.")
            ),
            Err(e) => note!(
                Level::Blocker,
                format!(
                    "History: no logs at block {start}, {to_sync} blocks back (\"{}\"). Use --last N to start nearer the head, or an endpoint with more history.",
                    e.message()
                ),
            ),
        }

        if needs.addressless {
            match ask(
                rpc,
                pause,
                &mut limited,
                "eth_getLogs",
                logs_filter(None, tip, tip),
            )
            .await
            {
                Ok(_) => note!(
                    Level::Ok,
                    "Logs without an address: accepted, which a data source with no address needs."
                        .into()
                ),
                Err(e) => note!(
                    Level::Blocker,
                    format!(
                        "Logs without an address: refused (\"{}\"), and a data source in this subgraph has none.",
                        e.message()
                    )
                ),
            }
        }
    }

    let mut features = Vec::new();
    if needs.calls {
        let at = json!([address.unwrap_or(ZERO_ADDRESS), hex(start)]);
        match ask(rpc, pause, &mut limited, "eth_getBalance", at).await {
            Ok(_) => {
                features.push("archive");
                note!(
                    Level::Ok,
                    format!(
                        "State: readable at block {start}, which mappings that call contracts need."
                    )
                );
            }
            Err(e) => note!(
                Level::Blocker,
                format!(
                    "State: not kept back to block {start} (\"{}\"). The mappings call contracts, so graph-node would stall there without saying why. Use --last N within the endpoint's state window, or an archive endpoint.",
                    e.message()
                ),
            ),
        }
    }

    if needs.traces {
        let filter = json!([{ "fromBlock": hex(tip), "toBlock": hex(tip), "toAddress": [address.unwrap_or(ZERO_ADDRESS)] }]);
        match ask(rpc, pause, &mut limited, "trace_filter", filter).await {
            Ok(_) => {
                features.push("traces");
                note!(
                    Level::Ok,
                    "Traces: trace_filter answers, which call handlers need.".into()
                );
            }
            Err(e) => note!(
                Level::Blocker,
                format!(
                    "Traces: trace_filter is not available (\"{}\"), and this subgraph has call handlers. Almost no free endpoint offers it.",
                    e.message()
                )
            ),
        }
    }

    match ask(rpc, pause, &mut limited, "eth_getBlockReceipts", json!([hex(tip)])).await {
        Ok(_) => note!(Level::Ok, "Receipts: eth_getBlockReceipts answers.".into()),
        Err(_) => note!(Level::Warn, "Receipts: no eth_getBlockReceipts, so graph-node fetches one receipt per transaction near the head. Slower, and harder on a rate limit.".into()),
    }

    match limited {
        Some(m) => note!(
            Level::Warn,
            format!(
                "Rate limit: hit during these few probes (\"{m}\"). A sync will run in bursts, and will look stuck between them."
            )
        ),
        // A dozen requests cannot find a per-minute cap. publicnode passed this and then
        // throttled a real sync at 1,200 a minute on 2026-11-11.
        None if !findings.iter().any(|f| f.level == Level::Blocker) => {
            note!(Level::Ok, "Rate limit: none hit, but these were a dozen requests. A shared endpoint may still throttle a real sync; Drydock shows it when it does.".into())
        }
        None => {}
    }
    if needs.templates && windowed {
        note!(Level::Warn, "Window: this subgraph creates data sources at runtime. Ones created before the window will never be found, so entities that depend on them will be missing.".into());
    }
    if windowed && start > needs.start_block {
        note!(
            Level::Warn,
            format!(
                "Window: starting at {start} where the manifest says {}. Handlers that rely on earlier state will find it missing.",
                needs.start_block
            )
        );
    }

    findings.sort_by_key(|f| std::cmp::Reverse(f.level));
    Ok(Report {
        head,
        start,
        findings,
        max_block_range,
        features,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: u64 = 56_000_000;

    /// An endpoint described by its limits, as measured on real ones on 2026-11-11.
    #[derive(Clone)]
    struct Endpoint {
        log_range: u64,
        log_depth: u64,
        state_depth: u64,
        addressless: bool,
        receipts: bool,
        traces: bool,
        /// Refuse every eth_getLogs as rate-limited, as mainnet.base.org did.
        logs_limited: bool,
        /// Rate-limit the first call of each method once, as a busy shared endpoint does.
        flaky: std::cell::Cell<bool>,
    }

    impl Endpoint {
        fn generous() -> Self {
            Self {
                log_range: 50_000,
                log_depth: 5_000_000,
                state_depth: 3_000_000,
                addressless: false,
                receipts: true,
                traces: false,
                logs_limited: false,
                flaky: false.into(),
            }
        }
    }

    impl Rpc for Endpoint {
        async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
            if self.flaky.replace(false) {
                return Err(RpcError::RateLimited(
                    "Rate limit (1200rqs/60s) reached.".into(),
                ));
            }
            let block = |v: &Value| from_hex(v).unwrap();
            match method {
                "eth_blockNumber" => Ok(json!(hex(HEAD))),
                "eth_getLogs" => {
                    if self.logs_limited {
                        return Err(RpcError::RateLimited("request limit reached".into()));
                    }
                    let f = &params[0];
                    let (from, to) = (block(&f["fromBlock"]), block(&f["toBlock"]));
                    if f.get("address").is_none() && !self.addressless {
                        return Err(RpcError::Refused("Please specify an address".into()));
                    }
                    if to - from + 1 > self.log_range {
                        return Err(RpcError::Refused(format!(
                            "exceed maximum block range: {}",
                            self.log_range
                        )));
                    }
                    if HEAD - from > self.log_depth {
                        return Err(RpcError::Refused(
                            "Archive requests require a personal token".into(),
                        ));
                    }
                    Ok(json!([]))
                }
                "eth_getBalance" if HEAD - block(&params[1]) > self.state_depth => {
                    Err(RpcError::Refused("missing trie node".into()))
                }
                "eth_getBalance" => Ok(json!("0x0")),
                "eth_getBlockReceipts" if self.receipts => Ok(json!([])),
                "trace_filter" if self.traces => Ok(json!([])),
                other => Err(RpcError::Refused(format!(
                    "the method {other} does not exist/is not available"
                ))),
            }
        }
    }

    fn needs() -> Needs {
        Needs {
            network: "base".into(),
            start_block: HEAD - 3_000,
            sample_address: Some("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into()),
            addressless: false,
            calls: false,
            traces: false,
            templates: false,
        }
    }

    async fn check(e: Endpoint, n: Needs, last: Option<u64>) -> Report {
        let window = last.map_or(Window::Whole, Window::Last);
        run_with_pause(&e, &n, window, Duration::ZERO)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_pinned_window_starts_where_it_first_did_however_far_the_head_has_moved() {
        let old = Needs {
            start_block: 1_000_000,
            ..needs()
        };
        let first = check(Endpoint::generous(), old.clone(), Some(200_000)).await;
        let pinned = run_with_pause(
            &Endpoint::generous(),
            &old,
            Window::From(first.start - 5_000),
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(pinned.start, first.start - 5_000, "not head minus anything");
        assert!(says(
            &pinned,
            Level::Warn,
            "where the manifest says 1000000"
        ));
        // A pin older than the manifest's own start is not a window at all.
        let whole = run_with_pause(
            &Endpoint::generous(),
            &needs(),
            Window::From(5),
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(whole.start, needs().start_block);
    }

    fn says(r: &Report, level: Level, needle: &str) -> bool {
        r.findings
            .iter()
            .any(|f| f.level == level && f.text.contains(needle))
    }

    #[tokio::test]
    async fn a_generous_endpoint_and_a_recent_events_subgraph_pass() {
        let r = check(Endpoint::generous(), needs(), None).await;
        assert!(!r.blocked(), "{:#?}", r.findings);
        assert_eq!(r.max_block_range, Some(2_000));
        assert!(
            r.features.is_empty(),
            "nothing is declared that was not asked for and measured"
        );
        assert_eq!((r.head, r.start), (HEAD, HEAD - 3_000));
    }

    #[tokio::test]
    async fn an_endpoint_that_rate_limits_every_log_request_is_a_blocker() {
        let r = check(
            Endpoint {
                logs_limited: true,
                ..Endpoint::generous()
            },
            needs(),
            None,
        )
        .await;
        assert!(says(&r, Level::Blocker, "cannot index anything"));
        assert_eq!(r.max_block_range, None);
    }

    #[tokio::test]
    async fn a_ten_block_cap_passes_with_the_arithmetic_spelled_out() {
        // Alchemy's free tier, per its docs on 2026-11-11.
        let r = check(
            Endpoint {
                log_range: 10,
                ..Endpoint::generous()
            },
            needs(),
            None,
        )
        .await;
        assert!(!r.blocked());
        assert_eq!(r.max_block_range, Some(10));
        assert!(says(&r, Level::Warn, "at least 300 requests"));
    }

    #[tokio::test]
    async fn a_start_block_older_than_the_log_history_is_a_blocker_that_names_the_way_out() {
        let old = Needs {
            start_block: 1_000_000,
            ..needs()
        };
        let r = check(Endpoint::generous(), old.clone(), None).await;
        assert!(says(&r, Level::Blocker, "--last N"));
        let windowed = check(Endpoint::generous(), old, Some(200_000)).await;
        assert!(!windowed.blocked());
        assert_eq!(windowed.start, HEAD - 200_000);
        assert!(says(
            &windowed,
            Level::Warn,
            "where the manifest says 1000000"
        ));
    }

    #[tokio::test]
    async fn mappings_that_call_contracts_need_state_at_the_start_block() {
        let calling = Needs {
            calls: true,
            ..needs()
        };
        // arb1.arbitrum.io kept state for 5,000 to 10,000 blocks.
        let shallow = check(
            Endpoint {
                state_depth: 2_000,
                ..Endpoint::generous()
            },
            calling.clone(),
            None,
        )
        .await;
        assert!(says(
            &shallow,
            Level::Blocker,
            "stall there without saying why"
        ));
        assert!(shallow.features.is_empty());
        let deep = check(Endpoint::generous(), calling, None).await;
        assert!(!deep.blocked());
        assert_eq!(deep.features, vec!["archive"]);
    }

    #[tokio::test]
    async fn call_handlers_need_trace_filter() {
        let tracing = Needs {
            traces: true,
            ..needs()
        };
        assert!(says(
            &check(Endpoint::generous(), tracing.clone(), None).await,
            Level::Blocker,
            "trace_filter is not available"
        ));
        let r = check(
            Endpoint {
                traces: true,
                ..Endpoint::generous()
            },
            tracing,
            None,
        )
        .await;
        assert_eq!(r.features, vec!["traces"]);
    }

    #[tokio::test]
    async fn a_source_without_an_address_is_checked_only_when_there_is_one() {
        let r = check(
            Endpoint::generous(),
            Needs {
                addressless: true,
                ..needs()
            },
            None,
        )
        .await;
        assert!(says(&r, Level::Blocker, "Logs without an address: refused"));
        assert!(
            !check(Endpoint::generous(), needs(), None)
                .await
                .findings
                .iter()
                .any(|f| f.text.contains("without an address"))
        );
    }

    #[tokio::test]
    async fn one_rate_limited_probe_is_retried_and_reported_not_fatal() {
        let r = check(
            Endpoint {
                flaky: true.into(),
                ..Endpoint::generous()
            },
            needs(),
            None,
        )
        .await;
        assert!(!r.blocked());
        assert!(says(&r, Level::Warn, "1200rqs/60s"));
    }

    #[tokio::test]
    async fn a_window_on_a_factory_warns_that_children_will_be_missing() {
        let factory = Needs {
            templates: true,
            start_block: 1_000_000,
            ..needs()
        };
        let r = check(Endpoint::generous(), factory.clone(), Some(100_000)).await;
        assert!(says(&r, Level::Warn, "never be found"));
        assert!(
            !check(
                Endpoint {
                    log_depth: u64::MAX,
                    ..Endpoint::generous()
                },
                factory,
                None
            )
            .await
            .findings
            .iter()
            .any(|f| f.text.contains("never be found"))
        );
    }

    #[tokio::test]
    async fn blockers_come_first() {
        let r = check(
            Endpoint {
                receipts: false,
                ..Endpoint::generous()
            },
            Needs {
                traces: true,
                ..needs()
            },
            None,
        )
        .await;
        assert_eq!(r.findings[0].level, Level::Blocker);
        assert!(says(&r, Level::Warn, "one receipt per transaction"));
    }
}
