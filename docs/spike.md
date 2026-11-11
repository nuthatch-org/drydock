# Phase 0 spike

Run on 2026-11-11 on the ThinkPad, which was under heavy unrelated load (load average 128 on 32
cores), so **nothing here is a timing**. The containers were capped at two cores. The files used
are in [`spike/`](../spike): the compose file, the generated config, and the test subgraph (USDC
transfers on Base from 3,000 blocks back, with `log.info` calls and, in its second form, a
deliberate null dereference on the 300th transfer).

Stack: `graphprotocol/graph-node:v0.45.0`, `postgres:16`, `ipfs/kubo:v0.38.1`, with graph-node in
`--config` mode. RPC: `https://base-rpc.publicnode.com`, no key, declared with no capabilities.

## What was confirmed

| Question | Answer |
|---|---|
| Does `[log_store] backend = "file"` give `_logs` over HTTP on v0.45.0? | **Yes.** `Log store initialized, backend: File`. `_logs` on the subgraph's own endpoint returns mapping `log.info` lines (tagged `component: UserMapping`) and graph-node's own lines for that deployment |
| Can a page on `http://localhost:8787` read ports 8000 and 8030? | **Yes.** Preflight answers 200 with `access-control-allow-origin: *` and `Content-Type` allowed on both |
| Can it call the admin port? | **No.** `OPTIONS` on 8020 is 405. Redeploy has to go through sib |
| Does the status API follow a sync? | **Not by default.** See below |
| What does a handler failure look like? | `health: failed`, `deterministic: true`, block number, and a message naming **file, line, column and handler**: `Mapping aborted at src/mapping.ts, line 20, column 19, with message: unexpected null in handler handleTransfer at block #56055226`. The `handler` field itself was null; the name is only in the message |
| Does a free Base endpoint carry it? | **Yes, slowly and noisily.** See below |
| Does anything leave the machine? | No. `graph deploy --ipfs` pointed at the local kubo |

## What was found

**The status API lies while syncing, unless write batching is off.** With defaults, two and a half
minutes into a healthy sync that had processed about 950 blocks, `indexingStatuses` still reported
`latestBlock` equal to the start block and `entityCount: 0`, and the entity query returned null.
With `GRAPH_STORE_WRITE_BATCH_DURATION=0` the same query moved every sample and the entity was
readable mid-sync. Restarting the node without it also lost the unflushed work. This was marked
*inferred* in the plan and is now measured: **sib sets it to 0.**

**publicnode rate-limits at 1,200 requests a minute and graph-node has no brake.** On a contract
with transfers in every block, graph-node asked for blocks faster than that. 105 of 9,475 log
lines were 429s. It recovers by retrying, so the sync proceeds in bursts, but nothing in the
status says why it paused; the reason is only in `_logs`, at DEBUG. **The page has to surface
rate-limiting as a state of its own**, read from the logs, or a developer will think it is stuck.
Whether `ETHEREUM_BLOCK_BATCH_SIZE` or a per-provider limit can hold it under the cap is the next
thing to try.

**The file log store is verbose.** 35 MB in seven minutes for one deployment, because every
trigger is logged at DEBUG. sib needs a retention setting from the start and the page should
default to INFO and above.

**The log files are written as root** inside the bind mount. Use a named volume, not a bind mount.

**`_logs` cannot share a request with an entity query.** The page makes two requests.

**A failed subgraph refuses queries** with `indexing_error` unless the query passes
`subgraphError: allow`. The playground should say so when it happens.

**It is small.** At rest during sync: graph-node 88 MB, Postgres 169 MB, kubo 176 MB.

## Not tested

- Timing of any kind.
- `eth_call` from a mapping, and therefore the state-depth half of the preflight.
- A subgraph with `features: [nonFatalErrors]`, which is where `GRAPH_DISABLE_FAIL_FAST` matters.
- A factory or template subgraph.
- Any chain but Base, any endpoint but publicnode.
- Docker Desktop on macOS, which is where most developers will run it.
