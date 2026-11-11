# sib: Studio in a box

Plan, drafted 2026-11-11 from three research passes (graph-node's source at v0.45.0, browser
rules for a web page talking to localhost, and what a local node needs from an RPC endpoint) and
from work already done on ephemeris-node, gib and the conformance stack. Every figure carries
where it came from. Anything marked *inferred* or *unverified* has not been tested.

## The problem, in one paragraph

Subgraph Studio ran a graph-node for developers. You deployed an unfinished subgraph, watched it
sync, saw your handler fail at a block, fixed it and redeployed, and nobody saw any of it. That
indexer is being switched off chain by chain: BNB Smart Chain and Polygon on 31 October 2026, Base
next. What is left is publishing to mainnet and finding the bug in public. People are asking, in
those words, for "their own Studio".

## What sib is

**One command that runs a real graph-node on your machine against your subgraph, and a local web
page that shows what Studio showed: sync progress, indexing errors, mapping logs and a query
playground.** When it works, one more command hands the build to Lodestar's `/move` to publish.

```sh
cd my-subgraph
sib up --rpc base:https://base-rpc.publicnode.com
# builds, deploys to a local graph-node, opens http://localhost:8787
```

It does not host anything, needs no account, and uploads nothing: the IPFS node is local too.

## What already exists, and why none of it is the answer yet

| | What it gives | Why it is not enough alone |
|---|---|---|
| `gnd dev` (graph-node's own, npm `@graphprotocol/gnd` 0.45.0) | graph-node in one process, reads the build from disk | Needs Postgres binaries installed; **deletes its database on every exit**; every rebuild re-indexes from the start block; cannot enable mapping logs over HTTP; no start-block override; no check of the RPC |
| graph-node's docker-compose | The real thing, with a config file | Three unpinned images, a README that is wrong in three places, no status page, nothing that tells you why it is stuck |
| redstart's `conformance/` | A working create, deploy and wait loop | Never reads the status endpoint, so errors are never surfaced |
| ephemeris-node | One binary, no Postgres, keeps state, verified against graph-node on 743 query shapes | Private, built from source, no releases; the mainnet corpus is unverified |
| nuthatch | Checks manifest, ABIs and events | Never runs mappings, by RFC-0038 §8 (nuthatch#2839) |

The gap is not an engine. **The gap is everything around the engine**: getting it up with settings
that survive a free RPC, telling the developer plainly what their RPC cannot do, and a page that
shows the result.

## Decisions

### 1. The engine for v0 is stock graph-node in Docker, behind an engine interface

Reasons, in order:

- **It is what indexers run.** The point of testing is "if it syncs here, it will sync there".
  Nothing else can make that claim without a footnote.
- **Only config-file mode can turn on mapping logs over HTTP** (`[log_store] backend = "file"`,
  then the `_logs` query on port 8000; v0.44.0+). `gnd dev` cannot use a config file at all.
  Without this the page has no logs, and logs are half of what Studio was for.
- **State persists** in a Docker volume, so stopping for lunch does not cost a re-index.
- Docker is a heavy ask, but subgraph developers overwhelmingly have it.

ephemeris-node becomes the second engine (`--engine ephemeris`) when it has release binaries and
can take a build from disk. It is the better experience, no Docker and no Postgres, and it is the
part nobody else has. Chief agreed on 2026-11-11 that it can be made public. It is still not v0:
its mainnet corpus is unverified, and the first claim sib makes has to be "this is what indexers
run".

### 2. The page is served from localhost by sib, not by lodestar-dashboard.com

A hosted HTTPS page fetching `http://localhost` works in Chrome 142+, Edge and Firefox 153+ only
after a permission prompt, is blocked by default in Brave, and **does not work in Safari at all**
(WebKit `MixedContentChecker.cpp` on main; bug 171934 still open, and WebKit's engineers are on
record against changing it). Served from `http://localhost:8787` it works everywhere with no
prompt, and localhost is a secure context, so clipboard and crypto still work. This is what Hasura,
Prisma and Supabase do.

graph-node already sends `Access-Control-Allow-Origin: *` on the query port (8000) and the status
port (8030), so the page can read both directly. The admin port (8020) sends no CORS headers, so
redeploy, pause and remove go through sib's own small server.

### 3. An RPC preflight, before anything starts

This is the highest-value thing sib adds, because **every RPC failure inside graph-node is silent
or misleading**: a range cap becomes a restart loop, missing state becomes a stall that looks like
slow sync, and a plain `network:url` silently declares `archive,traces` whether or not the
endpoint has either.

Before starting, sib probes the endpoint and reads the built manifest:

| Probe | Why |
|---|---|
| Largest `eth_getLogs` block range accepted | Sets `GRAPH_ETHEREUM_MAX_BLOCK_RANGE_SIZE`. Released v0.45.0 does not shrink ranges itself (fixed upstream in #6729, unreleased) |
| How far back logs go | Against the manifest's start block |
| How far back state goes | Only if the mappings import `ethereum.call` |
| `eth_getBlockReceipts` | Speed |
| `trace_filter` | Only if there are call handlers |
| `eth_getLogs` with no address | Only if a data source has none; publicnode refuses these |

Then it says what will not work, in words, and refuses to start a sync that cannot finish.
Measured on 11 November 2026, single-shot from one IP:

- `mainnet.base.org` rejected every `eth_getLogs` with a 429. It must not be a default.
- `base-rpc.publicnode.com` served 50,000-block ranges and state 3 million blocks back, keyless.
- `arb1.arbitrum.io/rpc` served 100,000-block ranges but state only 5,000 to 10,000 blocks back.
- Free Alchemy caps `eth_getLogs` at 10 blocks; free QuickNode at 5.

These limits moved between September and November, so sib measures them every time and ships no
table of them.

### 4. `--last N`: test on a recent window

Free endpoints make a full-history sync of an old subgraph impractical. `--last 200000` rewrites
`startBlock` in a temporary copy of the built manifest. The deployment ID changes, which is
harmless for a test. sib prints two warnings every time, because both are silent otherwise:
factory children created before the window are never discovered, and handlers that depend on
earlier state do nothing or abort.

### 5. Settings that are already known to matter

From ephemeris-node's notes and graph-node's source:

- `GRAPH_DISABLE_FAIL_FAST=true`. Without it a subgraph with a deterministic error stops on its
  first bad block while unsynced. Measured on 2026-09-10: 1 block processed against 4,620.
- `ETHEREUM_REORG_THRESHOLD=20`, `ETHEREUM_POLLING_INTERVAL=30000` on shared endpoints. The
  defaults burn about 8 requests a second on Arbitrum and get rate-limited within minutes.
- `GRAPH_STORE_WRITE_BATCH_DURATION=0`. With the default, the status API reported the start block
  and zero entities two and a half minutes into a healthy sync. Measured in the spike
  ([spike.md](spike.md)).
- Publish every port on `127.0.0.1` only. graph-node binds `0.0.0.0` and its admin port has no
  authentication.
- Pin every image. The official compose pins none.

### 6. It lives in `nuthatch-org/sib`, written in Rust

A new repo in gib's mould, public once it works end to end. The CLI is one Rust binary, `sib`,
decided by Chief on 2026-11-11 over a TypeScript recommendation. The price is a release pipeline:
prebuilt binaries for macOS (arm64, x64) and Linux (arm64, x64), with Windows through WSL at
first, and an npm package `studio-in-a-box` that only downloads the right one, as
`@graphprotocol/gnd` does, so `npx studio-in-a-box up` still works for people who live in Node.
The page is static files compiled into the binary.

## What the page shows

1. **Sync.** Latest block against chain head, blocks a second, an estimate. From
   `indexingStatuses` on 8030.
2. **Errors.** The fatal error with its block, handler and whether it is deterministic, in the
   words `/move` already uses: deterministic means your mapping, otherwise probably the RPC.
   Non-fatal errors listed.
3. **Logs.** `log.info()` output and handler errors, filterable by level, from `_logs`.
4. **Playground.** GraphiQL against the local endpoint.
5. **What your RPC cannot do**, kept on screen from the preflight.
6. **Publish.** Opens Lodestar's `/move` with the deployment, where the existing flow takes over:
   publish, signal or bounty, watch indexers.

The components for 1, 2 and 4 exist in Lodestar's `/move` and are reused, not rewritten.

## Phases

Estimates are working days and are mine, not measured.

| Phase | What | Days | Done when |
|---|---|---|---|
| 0. Spike | Compose with a generated config on one real Base subgraph. Confirm `_logs` with the file store, the write-batch setting and CORS from a localhost page | **done 2026-11-11** | See [spike.md](spike.md). No timings: the machine was busy |
| 1. CLI | `up`, `down`, `status`, `logs`; preflight; compose and config generation; build and deploy to the local node; `--last N`; `--watch`; release binaries | 4 | `sib up` works on a clean machine with Docker, on Base and Arbitrum |
| 2. Page | The six panels above, served by the CLI | 3 | Usable in Safari, Firefox, Chrome and Brave with no prompts |
| 3. Launch | `sib publish` handoff to `/move`; a page on Lodestar; the agent skill gains `sib`; a blog post and a video | 2 | Announced |
| 4. ephemeris engine | Open the repo, release binaries, build-from-disk, `--engine ephemeris` | open | A subgraph that passes on graph-node passes on it, by `ephemeris-node diff` |
| 5. Hosted | The same page with the node on our side, behind the Dock's deploy keys | open | See "Later: a hosted sib" |

Phases 0 to 3 are about ten days.

## Later: a hosted sib

Chief, 2026-11-11: self-hosted comes first, and there should be a hosted version afterwards for
people who do not want to run anything. Written down now so the self-hosted design does not
close it off. None of this is scheduled.

**What it is.** The same page and the same checks, with the graph-node on our machine. A
developer runs `graph deploy` against a Lodestar URL with a deploy key and gets a private
endpoint, a sync bar, errors and logs. This is what Studio's test indexer was.

**Most of the front half exists.** Lodestar's Dock already issues deploy keys and has a
`/api/studio/node` endpoint that speaks `graph deploy`'s dialect. Today it authenticates the CLI
and records the build, and there is no graph-node behind it. Hosted sib is, roughly, putting one
there.

**What it costs, which is why it was not built in November.** Three things, in order:

1. **RPC.** On a hosted node we pay for every block a stranger's subgraph reads. This is the
   whole of the economics. It needs a hard ceiling per deployment: a window (`--last N` is the
   same idea, enforced), a request budget, and a lifetime of days.
2. **Isolation.** graph-node is multi-tenant by design, one node and many deployments, which is
   how Studio ran. One tenant's runaway mapping still competes for the same Postgres.
3. **Abuse.** A free indexer of arbitrary code against a paid RPC is a target. It wants an
   account, and probably the paid tier.

**What the self-hosted work must keep true so this stays cheap to add:**

- The page reads only graph-node's own HTTP surfaces and sib's small server. It must not assume
  the node is on localhost; the base URL is configuration.
- The preflight and the governor are per endpoint, not per machine. A hosted node needs the
  governor more than a laptop does.
- The engine stays behind an interface. **ephemeris-node is what makes hosting affordable**: no
  Postgres per tenant, a few hundred megabytes at rest, state in one file. A hosted sib on stock
  graph-node is possible; on ephemeris-node it is the kind of thing a single box can do for many
  people.

**Open, for when it is scheduled:** free or paid, how long a deployment lives, which chains, and
whether it sits on the Helsinki boxes or somewhere new.

## What it will not do

- **Make a bad RPC good.** An old subgraph on a free endpoint will be slow, and sib will say how
  slow before it starts. Call handlers need `trace_filter`, which almost no free tier has.
- **Serve production queries.** It is a rehearsal room.
- **Guarantee what an indexer will see.** Same engine, different RPC, different history depth.
- **Run without Docker**, until the ephemeris engine exists.
- **Replace API keys, billing or publishing.** Those stay where they are.

## Decided with Chief, 2026-11-11

1. **Rust** for the CLI.
2. **`studio-in-a-box`** is the name; `sib` for short.
3. **ephemeris-node can be made public.**
4. **The spike runs on the ThinkPad.**

## Sources

- graph-node at `v0.45.0+patch.1` (`~/Projects/graph-node`, HEAD `84537ed`): `gnd/src/commands/dev.rs`,
  `server/http/src/service.rs`, `server/index-node/src/schema.graphql`, `node/src/config.rs`,
  `chain/ethereum/src/ethereum_adapter.rs`, `NEWS.md`.
- Browser behaviour: WICG Local Network Access spec (2026-08-07), Chrome's LNA post (2025-09-29),
  Firefox 153 release notes (2026-07-21), WebKit source and bugs 171934 and 279249. Real Safari was
  not run; the claim rests on WebKit's source.
- RPC limits: probed on 2026-11-11; provider pricing pages fetched the same day.
- ephemeris-node `docs/status.md` and the notes from its September sessions.
