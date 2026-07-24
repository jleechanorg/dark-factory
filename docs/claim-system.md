# CLAIMED Tag Coordination (`bin/claimd`)

Multi-machine coordination layer that prevents two factory daemons running on
different machines (e.g. `jeff-ubuntu` and `/mac`) from dispatching the same
bead concurrently. Lives on top of the SQLite overlay (`daemon-cxdb.sqlite`)
and the GitHub label plane; both must agree before either machine picks up a
bead.

## Why it exists

The auto-factory daemon was originally single-machine. Two machines running
the same `daemon-cxdb.sqlite` over a shared filesystem would race to
`dispatch_ready` the same bead (the daemon's tile-of-the-state lock only
serializes *one* daemon process). The cost of a duplicate dispatch is real:

- Two AO sessions spend token budget on the same bead.
- Two PRs race to open / close / merge against the same target branch.
- One machine's "DISPATCHED" write can clobber the other's "RE_ROLL" write
  in the overlay.

The CLAIMED tag system is the lowest-cost fix: a machine that wants to
dispatch a bead must first win a CAS-style claim on it, both locally
(SQLite) and against the peer's last-known claims (HTTP).

## Three layers

### 1. Bead overlay (authoritative)

Two new columns on `bead_overlay`:

| Column        | Type    | Purpose                                                |
|---------------|---------|--------------------------------------------------------|
| `claimed_by`  | TEXT    | Hostname of the machine that holds the claim. NULL = free. |
| `claimed_at`  | INTEGER | Unix epoch seconds when the claim was acquired / last heartbeated. |

Migration is idempotent: `ensure_claimed_by_columns` in `SqliteStateStore::open`
probes `pragma_table_info` then `ALTER TABLE`, same pattern as every other
bead-overlay column addition. Older DBs get the columns on first open.
`schema.sql` declares them for fresh DBs.

`bead_overlay` writes go through the `StateStore` trait. The new methods
(all default to no-op for fakes so test scaffolding stays unchanged):

| Method                  | Semantics                                                       |
|-------------------------|-----------------------------------------------------------------|
| `try_claim`             | Atomic CAS: if `claimed_by IS NULL` OR `claimed_at < now - ttl`, sets `claimed_by = machine`, returns `true`. Else returns `false`. Also sweeps stale local claims first. |
| `release_claim`         | Only the holder can release. Returns `true` iff the row was ours. |
| `heartbeat_claim`       | Only the holder can refresh. Updates `claimed_at = now`.       |
| `list_live_local_claims`| Rows with `claimed_by = self` AND `claimed_at >= now - ttl`.    |
| `replace_peer_claims`   | Wholescale delete + insert (`peer_claims` table) in a transaction. |
| `peer_claim_taken`      | True iff a peer row exists and `expires_at > now`. Uses the peer's reported expiry, not the local TTL. |
| `claim_blocks_dispatch` | True iff `claimed_by IS NOT NULL AND claimed_at >= now - ttl AND claimed_by != self`. The dispatch-gate predicate. |

The tick loop (`daemon/src/tick.rs`) calls `claim_blocks_dispatch` immediately
before `dispatch_ready` and drops any bead whose claim is held by another
machine. Fakes default to `Ok(false)`, so unit tests that don't care about
multi-machine coordination stay unchanged.

### 2. Peer sync (HTTP)

A small std-only HTTP server in `daemon/src/bin/claimd.rs` exposes:

| Method | Path     | Body                                            |
|--------|----------|-------------------------------------------------|
| GET    | /healthz | (none) → 200 OK `{"status":"ok"}`               |
| GET    | `/sync`    | (none) → 200 OK with the live claim list       |
| POST   | `/sync`    | `{claims:[{machine,bead_id,claimed_at,expires_at},...]}` |
| POST   | `/config`  | Push a per-machine config update (port / host). |

Periodically (default 60s), `claimd daemon` walks the local live claim set
and pushes it to the peer via `POST /sync`. Each peer stores the result in
its own `peer_claims` table via `replace_peer_claims`. A machine that wants
to claim a bead asks `peer_claim_taken` first: if the peer's last /sync
shows the bead as held (within the peer's reported `expires_at`), the claim
is refused before the local CAS even runs.

The HTTP layer is deliberately minimal — no `serde_json` in the binary
parser body, just `std::net::TcpListener` and a hand-rolled extractor. The
shared `daemon-cxdb.sqlite` contains the full ground truth; the peer sync is
a 60-second-aged snapshot, not a live channel.

### 3. GitHub labels (audit + UI)

Each machine applies a `claimed-by:<machine>` label (e.g. `claimed-by:jeff-ubuntu`)
alongside the umbrella `CLAIMED` label. Heartbeat (`bin/claim-heartbeat`)
re-applies the labels every 10 min by default; gh labels are cheap to write
and serve as a human-visible audit trail in the GitHub UI. The daemon
release path removes both labels on terminal state.

The `gh_reapply_minutes` knob in `config/daemon.toml` controls the reapply
interval. Defaults to 10 min so label TTL outlasts the typical 30-min claim
window without spamming the gh API.

## Tool surface

All under `bin/`:

| Command                              | Purpose                                                            |
|--------------------------------------|--------------------------------------------------------------------|
| `bin/claim <bead_id> [ttl_minutes=30]` | Acquire a claim. Exit 0 = won, 1 = stale local, 2 = peer-held, 3 = usage/runtime. |
| `bin/release <bead_id>`              | Release a claim (must be the holder).                              |
| `bin/claim-heartbeat [bead_id]`      | Refresh `claimed_at` and re-apply gh labels. No arg = all live claims. |
| `bin/claim-daemon`                   | Long-running daemon: serves `/sync`, periodically pushes, runs heartbeats. |
| `bin/claim list`                     | List live (non-expired) local claims.                              |
| `bin/claim sync-once`                | One-shot pull-then-push to peer.                                   |
| `bin/claim ensure-schema`            | Run the `pragma_table_info` + `ALTER TABLE` migration. Idempotent. |

Exit-code semantics are essential for the dispatch loop and the cron path:

| Code | Meaning                                                              |
|------|----------------------------------------------------------------------|
| 0    | Claim acquired / released / heartbeat refreshed.                     |
| 1    | Stale local claim (someone else holds it within TTL).                |
| 2    | Peer held (peer's last /sync reports it).                            |
| 3    | Usage / runtime error (bad arg, DB unreachable, etc.).               |

## Configuration

`config/daemon.toml` carries a `[claim]` section:

```toml
[claim]
ttl_minutes = 30
heartbeat_seconds = 600
gh_reapply_minutes = 10
sync_seconds = 60

[claim.jeff-ubuntu]
hostname = "jeff-ubuntu"
daemon_port = 7821
peer_host = "mac.lan"
peer_port = 7822

[claim.mac]
hostname = "mac"
daemon_port = 7822
peer_host = "jeff-ubuntu.lan"
peer_port = 7821
```

`bin/claimd` reads the same file via env vars: `CLAIM_DB` (default
`~/.dark-factory/daemon-cxdb.sqlite`, shared with the daemon), `CLAIM_MACHINE`
(default `jeff-ubuntu`), `CLAIM_TTL_SECS` (default 1800),
`CLAIM_HEARTBEAT_SECS` (default 600), `CLAIM_SYNC_SECS` (default 60),
`CLAIM_DAEMON_PORT` (default 7821), `CLAIM_PEER_URL` (no default — unset =
single-machine mode with no peer sync).

Unset `CLAIM_PEER_URL` is the single-machine mode. The daemon still applies
claims and uses the overlay gate in `tick.rs`, but no peer sync runs.
Useful for testing or for short-lived CI machines.

## Running on each machine

### jeff-ubuntu

```bash
export CLAIM_DB=~/.dark-factory/daemon-cxdb.sqlite
export CLAIM_MACHINE=jeff-ubuntu
export CLAIM_DAEMON_PORT=7821
export CLAIM_PEER_URL=http://mac.lan:7822
nohup bin/claimd daemon > ~/.dark-factory/claimd.jeff-ubuntu.log 2>&1 &
```

### /mac

```bash
export CLAIM_DB=~/.dark-factory/daemon-cxdb.sqlite
export CLAIM_MACHINE=mac
export CLAIM_DAEMON_PORT=7822
export CLAIM_PEER_URL=http://jeff-ubuntu.lan:7821
nohup bin/claimd daemon > ~/.dark-factory/claimd.mac.log 2>&1 &
```

The factory daemon itself does not need the env vars — it reads the same
overlay state and gates at `tick.rs`. The claim daemon is the only thing
that talks to the peer.

## Testing multi-machine coordination locally

Two claimd daemons on the same host, two different DBs, but the same
overlay state dir for short-lived tests:

```bash
# terminal 1 — pretend to be jeff-ubuntu
export CLAIM_DB=/tmp/claim-test.sqlite
export CLAIM_MACHINE=jeff-ubuntu
export CLAIM_DAEMON_PORT=17821
export CLAIM_PEER_URL=http://127.0.0.1:17822
sqlite3 "$CLAIM_DB" < daemon/contracts/schema.sql
bin/claimd ensure-schema
bin/claimd daemon

# terminal 2 — pretend to be mac
export CLAIM_DB=/tmp/claim-test.sqlite
export CLAIM_MACHINE=mac
export CLAIM_DAEMON_PORT=17822
export CLAIM_PEER_URL=http://127.0.0.1:17821
bin/claimd daemon

# terminal 3 — drive claims
bin/claim bead-abc             # jeff-ubuntu (TTL 30 min)
bin/claim bead-abc             # mac: should exit 2 (peer-held) after sync
bin/claim heartbeat bead-abc   # jeff-ubuntu refreshes
bin/claim release bead-abc     # jeff-ubuntu releases
bin/claim bead-abc             # mac: should exit 0 now
```

Stale-claim path (set TTL to 1s for the test):

```bash
CLAIM_TTL_SECS=1 bin/claim bead-abc           # jeff-ubuntu wins
sleep 2
CLAIM_TTL_SECS=1 bin/claim bead-abc           # mac exits 0 (stale swept)
```

## Failure modes

| Symptom                                    | Likely cause                                        |
|--------------------------------------------|-----------------------------------------------------|
| Both machines refuse to claim same bead    | Stale local claim on each side; sweep timing.       |
| Peer sync returns 200 but rows never appear | `replace_peer_claims` ran but transaction was rolled back. Check `~/.dark-factory/claimd.*.log`. |
| `claimed_by` column missing                | Older DB; `bin/claimd ensure-schema` adds it. `SqliteStateStore::open` does the same on daemon startup. |
| gh label never applied                     | `gh` not authenticated, or `gh_reapply_minutes=0`. |
| `claim_blocks_dispatch` returns true but dispatch still runs | Tick gate is bypassed. Check `CLAIM_BYPASS=1` env; must NOT be set. |

## Cross-references

- `daemon/src/state.rs` — `try_claim` / `release_claim` / `heartbeat_claim` / `replace_peer_claims` / `peer_claim_taken` / `claim_blocks_dispatch` impls on `SqliteStateStore`.
- `daemon/src/bin/claimd.rs` — single-binary CLI + daemon.
- `daemon/src/tick.rs` — dispatch gate (`CLAIM_BLOCKED_DISPATCH` telemetry).
- `daemon/contracts/schema.sql` — overlay column additions + `peer_claims` table.
- `config/daemon.toml` — `[claim]` section.
- `docs/auto-factory-daemon-spec.md` §4.2.8 — budget / dispatch governance (this doc extends §4.2.8 with the multi-machine chapter).
- Bead `jleechan-g1ib` — original ticket.
