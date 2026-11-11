# sib

Studio in a box: one command that runs a real graph-node on your own machine against your
subgraph, with a local page showing sync progress, indexing errors, mapping logs and a query
playground. For testing a subgraph before publishing it, now that Subgraph Studio's test indexer
is going chain by chain.

**Status: early.** One command works today, `sib preflight`. The rest is planned in
[docs/plan.md](docs/plan.md); what the first spike found is in [docs/spike.md](docs/spike.md).

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
