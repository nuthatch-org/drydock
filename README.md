# Lodestar Drydock

A dry dock is where a ship is built and tested before it is launched. This is that for a
subgraph: one command that runs a real graph-node on your own machine against your
subgraph, with a local page showing sync progress, indexing errors, mapping logs and a query
playground. For testing a subgraph before publishing it, now that Subgraph Studio's test indexer
is going chain by chain.

`drydock up` checks your RPC endpoint, starts a local graph-node, deploys your built subgraph to
it and serves a page that shows the sync, any indexing error, your mapping logs and a query box.
When it looks right, publish it from [lodestar-dashboard.com/move](https://www.lodestar-dashboard.com/move).

**Status: 0.1.0, early.** It works end to end on Linux with Docker, which is where every run so
far has been. See [what has not been tried](#what-has-not-been-tried) before trusting it with
something else.

**[The guide](docs/guide.md)** walks through a first run, what the page shows, and what each
failure means.

## Install

You need Docker with Compose, and a subgraph that `graph build` has built.

Binaries for macOS and Linux (glibc 2.35 or newer), arm64 and x64, are attached to each
[release](https://github.com/nuthatch-org/drydock/releases). Unpack one and put `drydock` on your
`PATH`. Or build it: `cargo install --path .`

## Use

```sh
graph build
drydock up --rpc base:https://base-rpc.publicnode.com
open http://localhost:8787
drydock status      # syncing: block 56056683 of 56068731, 11% (12048 to go)
drydock logs        # graph-node's own output
drydock down        # stops it and keeps the database, so the next `up` resumes
```

`drydock up` puts three things between you and a default graph-node:

- **A check of the endpoint first** (`drydock preflight`, below), so a sync that cannot finish is
  never started.
- **A brake.** graph-node has no rate limit of its own. On a shared endpoint it bursts, spends
  the minute's quota in seconds and then stalls in backoff. Drydock holds requests to 15 a second;
  `--rps 100` raises that, and `--rps 0` removes it for an endpoint you pay for.
- **Settings that a developer needs and the defaults get wrong**: the status API follows the
  sync, mapping logs are kept, the database survives a restart, and every port listens on
  127.0.0.1 only.

The page is one file with no requests to anywhere but itself, so it works offline. It says so
when graph-node stops answering, and it says so when your endpoint is rate-limiting, which
graph-node itself reports nowhere but its debug log.

`--last 200000` starts that many blocks back from the head, for a subgraph whose own start block
is older than the endpoint's history. The window is fixed the first time, so later `drydock up`s
resume it. `--port-offset 1000` moves the ports if 8000, 8020, 8030, 5001 and 8787 are taken. Nothing is uploaded anywhere: the IPFS node is local too.

## `drydock preflight`

graph-node takes an RPC endpoint's abilities on trust. What the endpoint cannot do shows up
later as a restart loop or a stall, with nothing in the status to say why. `drydock preflight` asks
first, against what your built subgraph actually needs.

```sh
graph build
drydock preflight --rpc base:https://base-rpc.publicnode.com
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

## What has not been tried

- **Apple Silicon with the published graph-node image.** graph-node ships for x86-64 only. Under
  QEMU emulation (colima's default) it is killed for memory within a minute of syncing, and
  `drydock status` says so. A graph-node v0.45.0 built for arm64 from source synced here without
  trouble at 75 MB: build one and name it in `DRYDOCK_GRAPH_NODE_IMAGE`. Docker Desktop with
  Rosetta has not been tried.
- **Windows.** Nobody has run it.
- **Call handlers, synced.** `preflight` refuses an endpoint without `trace_filter` for a subgraph
  that has them, which is every free endpoint tried. No subgraph with call handlers has been
  synced here, because that takes an endpoint with traces.

Tried on 11 November 2026, on an Apple Silicon Mac with a graph-node built for arm64: a factory
with a template on Base (23 pairs created and followed), and USDC on Arbitrum One. The template
run found that publicnode allows ten contract addresses in one log query, where graph-node sends
up to 2,000 and retries a refusal for ever while reporting itself healthy. `preflight` now measures
that limit and `up` passes it to graph-node.

It runs graph-node v0.45.0, Postgres 16 and Kubo v0.38.1, pinned. The plan is in
[docs/plan.md](docs/plan.md) and what has been measured is in [docs/spike.md](docs/spike.md).

Part of [Lodestar](https://www.lodestar-dashboard.com) and of [Nuthatch](https://github.com/nuthatch-org).
Sibling of [gib](https://github.com/nuthatch-org/gib), the gateway in a box.
