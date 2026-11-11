# The Drydock guide

From a subgraph on your disk to one you have watched sync and queried, on your own machine, and
then on to the network. Ten minutes if the endpoint is kind.

## What you need

- **Docker with Compose.** Docker Desktop, colima or plain Docker on Linux. Give it 4 GB.
- **A built subgraph.** `graph build` has to have run; Drydock deploys what is in `build/`.
- **An RPC endpoint for the chain.** A free public one is enough for most event subgraphs.
  Drydock tells you when it is not.

## 1. Install

Take the binary for your machine from the
[releases](https://github.com/nuthatch-org/drydock/releases), unpack it and put `drydock` on your
`PATH`:

```sh
tar -xzf drydock-v0.2.0-aarch64-apple-darwin.tar.gz
sudo mv drydock-v0.2.0-aarch64-apple-darwin/drydock /usr/local/bin/
drydock --version
```

`aarch64-apple-darwin` is an Apple Silicon Mac, `x86_64-apple-darwin` an Intel one. The Linux
binaries need glibc 2.35 or newer (Ubuntu 22.04, Debian 12).

## 2. Ask the endpoint first

```sh
cd my-subgraph
graph build
drydock preflight --rpc base:https://base-rpc.publicnode.com
```

The part before the colon is the network name your manifest uses. Preflight reads what your
subgraph needs from the build and asks the endpoint for exactly that. Each line is one of:

| Mark | Means |
|---|---|
| ✓ | Measured, and enough. |
| ! | It will run, with a consequence you should read. |
| ✗ | It will not sync on this endpoint. Exit status 1. |

The refusals you are most likely to meet:

- **History.** Free endpoints keep recent blocks only. If your start block is older, add
  `--last 200000` to start that many blocks back from the head. Your handlers will not see
  anything earlier, which matters if they rely on it.
- **Traces.** A subgraph with call handlers needs `trace_filter`. No free endpoint we tried has
  it. You need a paid endpoint or your own node.
- **Addresses per query.** A subgraph with templates asks for logs from every contract it has
  created at once. Some endpoints cap that (publicnode at ten). This is a `!`, not a `✗`:
  Drydock tells graph-node the limit and the sync is slower, where without it graph-node retries
  for ever and reports itself healthy.

## 3. Bring it up

```sh
drydock up --rpc base:https://base-rpc.publicnode.com
```

It runs the preflight again, starts graph-node, Postgres and a local IPFS node, deploys your
build, and prints where to look. The first run pulls about 1 GB of images.

Open **http://localhost:8787**. The page shows:

- **The sync**: the block it is on, the chain head, and how far there is to go.
- **A failure, if there is one**: the handler, the block and the message. A deterministic error
  is a bug in your mapping, and it would fail the same way on every indexer.
- **Your mapping's logs**: every `log.info` and its friends, by level.
- **A query box** against your subgraph.
- **What the preflight found**, and a notice when the endpoint is throttling.

From a terminal, `drydock status` gives the same in one line and exits 1 on a failed subgraph,
which is what a script wants. `drydock logs` is graph-node's own output.

## 4. Change the mapping, and go again

```sh
graph build
drydock up --rpc base:https://base-rpc.publicnode.com
```

A changed build is a new deployment and syncs from its start. An unchanged one resumes.
`drydock down` stops everything and keeps the database; `drydock down --wipe` deletes it.

## 5. Publish

When it syncs clean and the queries answer as you expect:

```sh
graph publish --webapp-url https://www.lodestar-dashboard.com/move
```

That opens Lodestar with your deployment, where you publish from your wallet, signal or post a
sync bounty, and watch indexers pick it up.

## Things worth knowing

**It holds your endpoint to 15 requests a second.** graph-node has no rate limit of its own and
a shared endpoint will throttle it into a stall. `--rps 100` raises the limit and `--rps 0`
removes it, for an endpoint you pay for.

**Nothing leaves your machine but RPC requests.** The IPFS node runs offline, and every port
listens on 127.0.0.1 only.

**A local chain works.** `--rpc mainnet:http://localhost:8545` reaches Anvil or Hardhat on your
machine. On Linux the node has to listen on all interfaces (`anvil --host 0.0.0.0`), because
containers reach the host by Docker's bridge address; Drydock says so if it cannot get through.

**Two subgraphs at once** need different ports: `--port-offset 1000` on the second moves it to
9000, 9020, 9030, 6001 and 9787.

**Where it keeps things.** A `.drydock/` folder beside your subgraph holds the generated
configuration, and ignores itself in git. The data is in Docker volumes named after the folder.

## When it goes wrong

| You see | It is |
|---|---|
| `graph-node ran out of memory and was killed` | Docker has too little memory, or graph-node is being emulated. See the README on Apple Silicon. |
| The block number has not moved for minutes, health "healthy" | The endpoint is refusing something. `drydock logs` and look for `WARN`. Run `drydock up` again: a newer preflight may now measure the limit. |
| `the containers cannot reach your RPC` | A local node bound to 127.0.0.1 on Linux. Bind it to 0.0.0.0. |
| `the status port did not answer` | The stack is not running. `drydock up`. |
| Ports already in use | `--port-offset 1000`. |
| A notice that the endpoint is rate-limiting | Lower `--rps`, or use another endpoint. |

If it is none of these, open an issue with the output of `drydock status` and the last fifty
lines of `drydock logs`.
