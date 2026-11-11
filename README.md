# sib

Studio in a box: one command that runs a real graph-node on your own machine against your
subgraph, with a local page showing sync progress, indexing errors, mapping logs and a query
playground. For testing a subgraph before publishing it, now that Subgraph Studio's test indexer
is going chain by chain.

**Status: early, and working end to end on Linux with Docker.** `sib up` checks your RPC
endpoint, starts a local graph-node, deploys your built subgraph to it and serves a page that
shows the sync, any indexing error, your mapping logs and a query box. The plan is in
[docs/plan.md](docs/plan.md); what has been measured so far is in [docs/spike.md](docs/spike.md).

## Use

```sh
graph build
sib up --rpc base:https://base-rpc.publicnode.com
open http://localhost:8787
sib status      # syncing: block 56056683 of 56068731, 11% (12048 to go)
sib logs        # graph-node's own output
sib down        # stops it and keeps the database, so the next `up` resumes
```

`sib up` puts three things between you and a default graph-node:

- **A check of the endpoint first** (`sib preflight`, below), so a sync that cannot finish is
  never started.
- **A brake.** graph-node has no rate limit of its own. On a shared endpoint it bursts, spends
  the minute's quota in seconds and then stalls in backoff. sib holds requests to 15 a second;
  `--rps 100` raises that, and `--rps 0` removes it for an endpoint you pay for.
- **Settings that a developer needs and the defaults get wrong**: the status API follows the
  sync, mapping logs are kept, the database survives a restart, and every port listens on
  127.0.0.1 only.

The page is one file with no requests to anywhere but itself, so it works offline. It says so
when graph-node stops answering, and it says so when your endpoint is rate-limiting, which
graph-node itself reports nowhere but its debug log.

`--last 200000` starts that many blocks back from the head, for a subgraph whose own start block
is older than the endpoint's history. The window is fixed the first time, so later `sib up`s
resume it. `--port-offset 1000` moves the ports if 8000, 8020, 8030, 5001 and 8787 are taken. Nothing is uploaded anywhere: the IPFS node is local too.

## `sib preflight`

graph-node takes an RPC endpoint's abilities on trust. What the endpoint cannot do shows up
later as a restart loop or a stall, with nothing in the status to say why. `sib preflight` asks
first, against what your built subgraph actually needs.

```sh
graph build
sib preflight --rpc base:https://base-rpc.publicnode.com
```

```
base: head 56062269, syncing from 1000000 (55062269 blocks)

  ✗ History: no logs at block 1000000, 55062269 blocks back ("Archive requests require a
    personal token."). Use --last N to start nearer the head, or an endpoint with more history.
  ✓ Logs: accepts 50000-block ranges, more than graph-node asks for.
  ✓ Receipts: eth_getBlockReceipts answers.

This endpoint cannot carry this subgraph as it stands.
```

It checks the widest `eth_getLogs` range the endpoint accepts, whether it has logs and state back
to your start block, `trace_filter` if you have call handlers, and logs without an address if a
data source has none. `--last 200000` asks the same for a window that starts nearer the head.
Exit status is 0 when it can carry the subgraph, 1 when it cannot.

It ships no table of providers. Public limits change month to month, so everything is measured
when you ask.

## Building

```sh
cargo build --release
```

Part of [Nuthatch](https://github.com/nuthatch-org). Sibling of
[gib](https://github.com/nuthatch-org/gib), the gateway in a box.
