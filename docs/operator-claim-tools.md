# /af CLAIMED tools — operator side (this PR)

The `claim` and `release` shell wrappers in `bin/` are the operator-side
entry points for the CLAIMED tag system (PR #475). The daemon side lives in
`daemon/src/bin/claimd.rs`.

## Usage

```bash
# Claim a bead (default TTL 30 min)
bin/claim jleechan-5y4k

# Claim with custom TTL
bin/claim jleechan-5y4k 60

# Release your claim
bin/release jleechan-5y4k
```

## Multi-machine setup

- jeff-ubuntu: bin/claim, runs `claimd` daemon on port 7821
- mac:        bin/claim, runs `claimd` daemon on port 7822
- Each machine sets `PEER_PORT` to the other machine's port:
  ```bash
  export PEER_PORT=7822  # jeff-ubuntu → mac
  export PEER_PORT=7821  # mac → jeff-ubuntu
  ```

## Multi-machine claim logic

1. `bin/claim` checks the peer's last-known claims via HTTP.
2. If the peer holds a non-expired claim for the same bead, returns 2.
3. Otherwise atomically writes the claim to the local SQLite overlay
   (CAS via `INSERT ... ON CONFLICT DO UPDATE ... WHERE expires_at < $NOW`).
4. Returns 0 on success, 1 on stale-local-conflict (other machine holds
   an active claim), 2 on peer-conflict.

The `release` wrapper only succeeds for the holder (`claimed_by = $MACHINE`).
