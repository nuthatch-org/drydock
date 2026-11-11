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
- A subgraph with `features: [nonFatalErrors]`, which is where `GRAPH_DISABLE_FAIL_FAST` matters.
- A factory or template subgraph.
- Call handlers, and so `trace_filter`.
- Any chain but Base.
- Docker Desktop on macOS, which is where most developers will run it.
- An RPC on the developer's own machine, such as Anvil. A container cannot reach the host's
  `localhost`, so sib will have to rewrite it.

## Second run: `sib up`, and the brake

The same day, the same machine, the same subgraph, this time through `sib up` (commit on
`pete/preflight`). `sib up` checked the endpoint, wrote the stack, uploaded and linked the build
to the local IPFS itself, and deployed it. Stopping with `sib down` and starting again resumed at
the block it had reached, on the same deployment hash.

**Stock graph-node cannot hold itself under a rate limit, and stalls for it.** With requests going
straight to publicnode it applied no block for ninety seconds at a time. Its retries had backed
off to a tenth attempt, the chain-head poll was being refused too, and the chain moved ahead of
the sync.

**A governor in front of the endpoint fixes it.** An nginx container in the stack queues
graph-node's requests and lets them out at a fixed rate (`--rps`, default 15):

| | Without | With, at 15 a second |
|---|---|---|
| Requests reaching the endpoint | about 37 a second in bursts | 13 to 15 a second, steady |
| Rate-limit refusals in graph-node's log | hundreds | 0 |
| Blocks applied over a 90-second window | 0 during a stall | about 150 every 15 seconds, every sample |

These are counts, not timings; the machine was still busy.

The first version of the governor did nothing at all, and looked as if it worked. Its limit was
keyed on `$server_name`, which is empty in this configuration, and nginx does not count a request
with an empty key. 37 requests a second went through a "15 a second" limit. It is keyed on
`$server_port` now, and there is a test that says why.

Also found: an endpoint that has just been hammered will rate-limit the preflight's first call.
`sib` now says that in words. And the governor returned six 403s from publicnode that never
appeared in graph-node's log and did not affect the sync; unexplained.

A contract with a transfer in every block is the worst case for this, since graph-node fetches
every block. At 15 requests a second it synced about ten blocks a second on a chain producing
about five, so it gains slowly. A quieter subgraph needs far fewer requests.

## Third run: a paid endpoint, calls, a window and a failure

Later the same day, with a dedicated GraphOps RPC key (`sib-dev`), still on the busy ThinkPad.

| What | Result |
|---|---|
| Preflight, GraphOps Base, start block 1,000,000 | Passes. 10,000-block ranges, logs and state at the start. publicnode refuses the same subgraph: logs yes, state no |
| A mapping that calls `totalSupply()`, read from the real compiled wasm | The preflight adds the state check, and the config declares `features = ["archive"]`, which it does not for an events-only subgraph |
| `sib up --last 3000 --rps 50` on a manifest that starts at block 30,000,000 | Deployed with the start block moved to 3,000 back. The calls returned values, visible in `_logs` |
| A mapping that dereferences null on its 300th transfer | `sib status` exits 1 with the file, line, handler and block, and says it is deterministic |
| Two stacks at once, `--port-offset 20000` and `21000` | No collision |

At 50 requests a second on this endpoint the busy-contract sync ran at about 18 blocks a second.
That is a count over one minute on a loaded machine, not a benchmark.

The generated `nginx.conf` holds the RPC URL, and so the key. It sits in `.sib/`, which sib
writes a `.gitignore` for.
