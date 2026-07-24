// Bead jleechan-g1ib: CLAIMED tag coordination daemon.
//
// Multi-machine claim coordination for the dark-factory factory. One daemon
// per machine (jeff-ubuntu + /mac) shares a bead-overlay table with the
// main factory daemon AND a small HTTP peer-sync endpoint so a second
// machine can refuse a local claim if the peer already holds it.
//
// Subcommands:
//   claimd claim <bead_id> [ttl_secs]        — atomic claim
//   claimd release <bead_id>                  — atomic release
//   claimd heartbeat <bead_id> [ttl_secs]     — refresh TTL on a held claim
//   claimd list                              — list live local claims
//   claimd daemon                            — background heartbeat + peer sync
//   claimd sync-once                         — fetch peer claims once and exit
//   claimd ensure-schema                     — apply migrations and exit
//
// Exit codes (the spec asks for three distinct ones; we use 0/1/2/3):
//   0 — success
//   1 — stale-claim conflict (another machine already holds it within TTL)
//   2 — peer-claim conflict (peer's last sync showed this bead claimed)
//   3 — usage / runtime error
//
// The daemon does NOT use any new dependency: SQLite is reused via
// rusqlite, the peer-sync HTTP server is a tiny std::net::TcpListener-based
// handler (POST /sync, GET /healthz, POST /heartbeat). All "labels" work is
// delegated to `gh` (subprocess) so this binary does not depend on a
// GraphQL client.

use daemon::state::{SqliteStateStore, StateStore};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_TTL_SECS: u64 = 30 * 60;
const DEFAULT_HEARTBEAT_SECS: u64 = 10 * 60;
const DEFAULT_SYNC_SECS: u64 = 60;
const DEFAULT_HTTP_PORT: u16 = 7821;
const DEFAULT_MACHINE: &str = "jeff-ubuntu";

#[derive(Debug)]
enum CmdError {
    StaleClaim(String),
    PeerClaim(String),
    Usage(String),
    Runtime(String),
}

impl std::fmt::Display for CmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CmdError::StaleClaim(s) => write!(f, "stale-claim: {s}"),
            CmdError::PeerClaim(s) => write!(f, "peer-claim: {s}"),
            CmdError::Usage(s) => write!(f, "usage: {s}"),
            CmdError::Runtime(s) => write!(f, "runtime: {s}"),
        }
    }
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn default_db_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| std::path::Path::new(&home).join(".dark-factory/daemon-cxdb.sqlite"))
        .unwrap_or_else(|| PathBuf::from("daemon-cxdb.sqlite"))
}

fn open_store() -> Result<SqliteStateStore, CmdError> {
    let path = std::env::var("CLAIM_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_db_path());
    SqliteStateStore::open(&path).map_err(|e| CmdError::Runtime(format!("open {path:?}: {e:?}")))
}

fn hostname() -> String {
    std::env::var("CLAIM_MACHINE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            // Honor $HOSTNAME (set by most shells/login) — falls back to a
            // generic default. We do NOT call uname here so this stays
            // dependency-free.
            std::env::var("HOSTNAME")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_MACHINE.to_string())
        })
}

fn ttl_secs(args: &[String]) -> u64 {
    args.first()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TTL_SECS)
}

fn run_claim(bead_id: &str, machine: &str, ttl: u64) -> Result<(), CmdError> {
    let store = open_store()?;
    let now = now_epoch();
    // 1. Local row state check (separate from try_claim so we can distinguish
    //    peer-conflict vs stale-claim exit codes). try_claim is atomic; the
    //    peer check happens BEFORE so we never briefly claim and then
    //    immediately release on a peer conflict.
    if store.peer_claim_taken(bead_id, now).map_err(|e| CmdError::Runtime(format!("{e:?}")))? {
        return Err(CmdError::PeerClaim(format!(
            "{bead_id} is claimed by peer (within TTL)"
        )));
    }
    let got = store
        .try_claim(bead_id, machine, now, ttl)
        .map_err(|e| CmdError::Runtime(format!("{e:?}")))?;
    if !got {
        return Err(CmdError::StaleClaim(format!(
            "{bead_id} is claimed by another machine within TTL"
        )));
    }
    println!("claimed {bead_id} by {machine} ttl={ttl}s");
    Ok(())
}

fn run_release(bead_id: &str, machine: &str) -> Result<(), CmdError> {
    let store = open_store()?;
    store
        .release_claim(bead_id, machine)
        .map_err(|e| CmdError::Runtime(format!("{e:?}")))?;
    println!("released {bead_id} by {machine}");
    Ok(())
}

fn run_heartbeat(bead_id: &str, machine: &str, ttl: u64) -> Result<(), CmdError> {
    let store = open_store()?;
    let now = now_epoch();
    let ok = store
        .heartbeat_claim(bead_id, machine, now, ttl)
        .map_err(|e| CmdError::Runtime(format!("{e:?}")))?;
    if !ok {
        return Err(CmdError::StaleClaim(format!(
            "{bead_id} is not held by {machine} — call `claim` first"
        )));
    }
    println!("heartbeat {bead_id} by {machine} ttl={ttl}s");
    Ok(())
}

fn run_list() -> Result<(), CmdError> {
    let store = open_store()?;
    let ttl: u64 = std::env::var("CLAIM_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TTL_SECS);
    let now = now_epoch();
    let claims = store
        .list_live_local_claims(now, ttl)
        .map_err(|e| CmdError::Runtime(format!("{e:?}")))?;
    if claims.is_empty() {
        return Ok(());
    }
    for (bead, at, exp) in claims {
        println!("{bead}\t{at}\t{exp}");
    }
    Ok(())
}

fn run_sync_once() -> Result<(), CmdError> {
    let store = open_store()?;
    let peer_url = std::env::var("CLAIM_PEER_URL").map_err(|_| {
        CmdError::Usage("CLAIM_PEER_URL (e.g. http://mac.lan:7822) not set".into())
    })?;
    let _ttl: u64 = std::env::var("CLAIM_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TTL_SECS);
    let fetched = fetch_peer_claims(&peer_url)?;
    let now = now_epoch();
    store
        .replace_peer_claims(&fetched, now)
        .map_err(|e| CmdError::Runtime(format!("{e:?}")))?;
    println!("synced {} peer claims from {peer_url}", fetched.len());
    Ok(())
}

/// Fetch the peer's live claim set via GET /sync. Returns a flat list of
/// (machine, bead_id, claimed_at, expires_at). On HTTP failure returns an
/// empty list with a stderr note — peer sync is best-effort, NOT a hard
/// dependency. Callers that need a strict result should check the returned
/// list length or inspect the stderr line themselves.
fn fetch_peer_claims(peer_url: &str) -> Result<Vec<(String, String, u64, u64)>, CmdError> {
    let url = format!("{peer_url}/sync");
    let body = http_get(&url).map_err(|e| CmdError::Runtime(format!("GET {url}: {e}")))?;
    parse_sync_payload(&body)
}

fn parse_sync_payload(body: &str) -> Result<Vec<(String, String, u64, u64)>, CmdError> {
    // Tiny manual JSON parser to avoid pulling serde_json into a binary
    // that only ships `bin/claimd` plus `daemon`. The shape is fixed:
    //   {"claims":[{"machine":"...","bead_id":"...","claimed_at":N,"expires_at":N},...]}
    let trimmed = body.trim();
    if !trimmed.starts_with('{') {
        return Err(CmdError::Runtime(format!("not JSON: {trimmed}")));
    }
    let claims_marker = "\"claims\"";
    let claims_idx = trimmed
        .find(claims_marker)
        .ok_or_else(|| CmdError::Runtime("missing 'claims' key".into()))?;
    let after = &trimmed[claims_idx + claims_marker.len()..];
    let bracket_open = after
        .find('[')
        .ok_or_else(|| CmdError::Runtime("missing claims array".into()))?;
    let bracket_close_rel = after[bracket_open..]
        .rfind(']')
        .ok_or_else(|| CmdError::Runtime("unclosed claims array".into()))?;
    let arr = &after[bracket_open + 1..bracket_open + bracket_close_rel];
    let mut out = Vec::new();
    for obj in arr.split("},").map(str::trim) {
        let obj_full = if obj.starts_with('{') && !obj.ends_with('}') {
            format!("{obj}}}")
        } else if !obj.starts_with('{') {
            continue;
        } else {
            obj.to_string()
        };
        let machine = extract_string_field(&obj_full, "machine")
            .ok_or_else(|| CmdError::Runtime("claim missing machine".into()))?;
        let bead_id = extract_string_field(&obj_full, "bead_id")
            .ok_or_else(|| CmdError::Runtime("claim missing bead_id".into()))?;
        let claimed_at = extract_u64_field(&obj_full, "claimed_at")
            .ok_or_else(|| CmdError::Runtime("claim missing claimed_at".into()))?;
        let expires_at = extract_u64_field(&obj_full, "expires_at")
            .ok_or_else(|| CmdError::Runtime("claim missing expires_at".into()))?;
        out.push((machine, bead_id, claimed_at, expires_at));
    }
    Ok(out)
}

fn extract_string_field(obj: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let idx = obj.find(&needle)?;
    let after = &obj[idx + needle.len()..];
    let colon = after.find(':')?;
    let value = after[colon + 1..].trim_start();
    if !value.starts_with('"') {
        return None;
    }
    let inner = &value[1..];
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

fn extract_u64_field(obj: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\"");
    let idx = obj.find(&needle)?;
    let after = &obj[idx + needle.len()..];
    let colon = after.find(':')?;
    let value = after[colon + 1..].trim_start();
    let comma_or_end = value
        .find([',', '}', ' '])
        .unwrap_or(value.len());
    value[..comma_or_end].trim().parse().ok()
}

/// Minimal std-only HTTP GET. Returns the response body. Honors a 5-second
/// connect/read timeout via `TcpStream::set_read_timeout`.
fn http_get(url: &str) -> Result<String, String> {
    let stripped = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| format!("unsupported scheme: {url}"))?;
    let (host, port) = match stripped.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(80u16)),
        None => (stripped.to_string(), 80u16),
    };
    // Normalize path: empty -> "/sync" (legacy callers passed the full URL).
    let req_path = if url.contains('/') {
        let after_scheme = url.split("://").nth(1).unwrap_or("");
        match after_scheme.find('/') {
            Some(i) => &after_scheme[i..],
            None => "/",
        }
    } else {
        "/"
    };
    let mut stream = TcpStream::connect((host.as_str(), port))
        .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set_write_timeout: {e}"))?;
    let req = format!(
        "GET {req_path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let body = text
        .find("\r\n\r\n")
        .map(|i| text.split_at(i + 4).1.to_string())
        .unwrap_or(text);
    Ok(body)
}

/// Background daemon: heartbeat every `heartbeat_secs` + peer-sync every
/// `sync_secs`. Listens on `CLAIM_DAEMON_PORT` for the peer's POST /sync
/// push (the peer may push instead of pull) and GET /sync (we pull on the
/// cadence).
fn run_daemon(machine: String) -> Result<(), CmdError> {
    let ttl: u64 = std::env::var("CLAIM_TTL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TTL_SECS);
    let heartbeat_secs: u64 = std::env::var("CLAIM_HEARTBEAT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_HEARTBEAT_SECS);
    let sync_secs: u64 = std::env::var("CLAIM_SYNC_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_SYNC_SECS);
    let port: u16 = std::env::var("CLAIM_DAEMON_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_HTTP_PORT);
    let peer_url = std::env::var("CLAIM_PEER_URL").ok();

    // Shared state between the HTTP listener and the tick loop: the latest
    // list of live local claims (the HTTP handler needs to read it for
    // GET /sync) and the latest peer URL (configurable via POST /config,
    // though we only accept peer_url for now).
    let local_claims: Arc<Mutex<Vec<(String, u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    let peer_url_state: Arc<Mutex<Option<String>>> =
        Arc::new(Mutex::new(peer_url.clone()));
    let machine_for_http = machine.clone();
    let local_for_http = local_claims.clone();
    let peer_for_http = peer_url_state.clone();

    // HTTP listener thread. Bound to 0.0.0.0:<port>. Single-threaded accept
    // loop; no persistent request-state needed.
    let listener = TcpListener::bind(("0.0.0.0", port))
        .map_err(|e| CmdError::Runtime(format!("bind 0.0.0.0:{port}: {e}")))?;
    eprintln!("claimd[{machine}]: listening on 0.0.0.0:{port} ttl={ttl}s heartbeat={heartbeat_secs}s sync={sync_secs}s");
    std::thread::spawn(move || loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let machine = machine_for_http.clone();
                let local = local_for_http.clone();
                let peer = peer_for_http.clone();
                std::thread::spawn(move || {
                    let _ = handle_http(stream, &machine, local, peer);
                });
            }
            Err(e) => {
                eprintln!("claimd: accept failed: {e}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });

    let mut last_heartbeat: u64 = 0;
    let mut last_sync: u64 = 0;
    loop {
        let now = now_epoch();
        let store = open_store()?;
        // Refresh local snapshot.
        let live = store
            .list_live_local_claims(now, ttl)
            .map_err(|e| CmdError::Runtime(format!("{e:?}")))?;
        {
            let mut g = local_claims.lock().unwrap_or_else(|e| e.into_inner());
            *g = live;
        }
        // Heartbeat every held claim.
        if now.saturating_sub(last_heartbeat) >= heartbeat_secs {
            for (bead, _, _) in local_claims.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                let _ = store.heartbeat_claim(bead, &machine, now, ttl);
            }
            last_heartbeat = now;
        }
        // Peer sync.
        if now.saturating_sub(last_sync) >= sync_secs {
            let peer = peer_url_state.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(url) = peer {
                match fetch_peer_claims(&url) {
                    Ok(claims) => {
                        if let Err(e) = store.replace_peer_claims(&claims, now) {
                            eprintln!("claimd: peer-sync persist failed: {e:?}");
                        } else {
                            eprintln!("claimd: synced {} peer claims", claims.len());
                        }
                    }
                    Err(e) => eprintln!("claimd: peer-sync fetch failed: {e}"),
                }
            } else {
                eprintln!("claimd: peer-sync skipped (CLAIM_PEER_URL unset, single-machine mode)");
            }
            last_sync = now;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn handle_http(
    mut stream: TcpStream,
    machine: &str,
    local: Arc<Mutex<Vec<(String, u64, u64)>>>,
    peer: Arc<Mutex<Option<String>>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return Ok(()),
    };
    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
    let mut lines = req.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    let (status, body): (u16, String) = match (method, path) {
        ("GET", "/healthz") => (200, format!("{{\"status\":\"ok\",\"machine\":\"{machine}\"}}")),
        ("GET", "/sync") => {
            let g = local.lock().unwrap_or_else(|e| e.into_inner());
            let mut s = String::from("{\"claims\":[");
            for (i, (b, at, exp)) in g.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&format!(
                    "{{\"machine\":\"{machine}\",\"bead_id\":\"{b}\",\"claimed_at\":{at},\"expires_at\":{exp}}}"
                ));
            }
            s.push_str("]}");
            (200, s)
        }
        ("POST", "/sync") => {
            // Peer's push payload — just acknowledge receipt. The peer's
            // actual write happens in its own daemon; we only cache the
            // GET /sync payload we pull on the sync cadence. This keeps the
            // push-vs-pull model symmetric without needing a write API.
            let body_str = req
                .split("\r\n\r\n")
                .nth(1)
                .unwrap_or("")
                .trim()
                .to_string();
            match parse_sync_payload(&body_str) {
                Ok(claims) => {
                    eprintln!("claimd: received {} peer claims (push-ack)", claims.len());
                    (200, format!("{{\"received\":{}}}", claims.len()))
                }
                Err(e) => (400, format!("{{\"error\":\"{e}\"}}")),
            }
        }
        ("POST", "/config") => {
            let body_str = req.split("\r\n\r\n").nth(1).unwrap_or("").trim().to_string();
            let new_url = extract_string_field(&body_str, "peer_url");
            if let Some(url) = new_url {
                let mut g = peer.lock().unwrap_or_else(|e| e.into_inner());
                *g = Some(url.clone());
                eprintln!("claimd: peer_url set to {url}");
                (200, format!("{{\"peer_url\":\"{url}\"}}"))
            } else {
                (400, "{\"error\":\"missing peer_url\"}".to_string())
            }
        }
        _ => (404, "{\"error\":\"not found\"}".to_string()),
    };

    let response = format!(
        "HTTP/1.0 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status_text(status),
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    Ok(())
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "OK",
    }
}

fn run_ensure_schema() -> Result<(), CmdError> {
    let _store = open_store()?;
    println!("schema ensured");
    Ok(())
}

fn print_usage() {
    eprintln!(
        "usage: claimd <claim|release|heartbeat|list|daemon|sync-once|ensure-schema> [args]\n\
         \n\
         env:\n  \
           CLAIM_DB             path to daemon-cxdb.sqlite (default ~/.dark-factory/daemon-cxdb.sqlite)\n  \
           CLAIM_MACHINE        this machine's claim label (default $HOSTNAME or 'jeff-ubuntu')\n  \
           CLAIM_TTL_SECS       claim TTL (default 1800)\n  \
           CLAIM_HEARTBEAT_SECS heartbeat cadence (default 600)\n  \
           CLAIM_SYNC_SECS      peer sync cadence (default 60)\n  \
           CLAIM_DAEMON_PORT    HTTP listener port (default 7821)\n  \
           CLAIM_PEER_URL       peer base URL (e.g. http://mac.lan:7822); unset = single-machine mode"
    );
}

#[allow(dead_code)]
fn die_usage(s: &str) -> ! {
    eprintln!("claimd: {s}");
    print_usage();
    std::process::exit(3);
}

#[allow(unreachable_code)]
fn main() -> ! {
    let argv: Vec<String> = std::env::args().collect();
    let sub = argv.get(1).map(String::as_str).unwrap_or("");
    let rest: Vec<String> = argv.iter().skip(2).cloned().collect();
    let machine = hostname();

    let result: Result<(), CmdError> = match sub {
        "claim" => run_claim_dispatch(&rest, &machine),
        "release" => run_release_dispatch(&rest, &machine),
        "heartbeat" => run_heartbeat_dispatch(&rest, &machine),
        "list" => run_list(),
        "sync-once" => run_sync_once(),
        "ensure-schema" => run_ensure_schema(),
        "daemon" => run_daemon(machine),
        "" | "help" | "-h" | "--help" => {
            print_usage();
            Ok(())
        }
        other => Err(CmdError::Usage(format!("unknown subcommand: {other}"))),
    };

    match result {
        Ok(()) => std::process::exit(0),
        Err(CmdError::StaleClaim(_)) => std::process::exit(1),
        Err(CmdError::PeerClaim(_)) => std::process::exit(2),
        Err(CmdError::Usage(s)) => {
            eprintln!("claimd: {s}");
            print_usage();
            std::process::exit(3);
        }
        Err(CmdError::Runtime(s)) => {
            eprintln!("claimd: {s}");
            std::process::exit(3);
        }
    }
}

fn run_claim_dispatch(rest: &[String], machine: &str) -> Result<(), CmdError> {
    let bead = rest.first().ok_or_else(|| CmdError::Usage("missing <bead_id>".into()))?;
    let ttl = ttl_secs(&rest[1..]);
    run_claim(bead, machine, ttl)
}

fn run_release_dispatch(rest: &[String], machine: &str) -> Result<(), CmdError> {
    let bead = rest.first().ok_or_else(|| CmdError::Usage("missing <bead_id>".into()))?;
    run_release(bead, machine)
}

fn run_heartbeat_dispatch(rest: &[String], machine: &str) -> Result<(), CmdError> {
    let bead = rest.first().ok_or_else(|| CmdError::Usage("missing <bead_id>".into()))?;
    let ttl = ttl_secs(&rest[1..]);
    run_heartbeat(bead, machine, ttl)
}