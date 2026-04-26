// End-to-end latency demo — shared payload, latency schema, and wire types.
//
// Two binaries (ws_server, fix_gw) are linked against this module via
// `#[path = "shared.rs"] mod shared;`. Both processes must agree on the
// layout — the `latency_stages!` macro guarantees this when both sides
// declare the same stages in the same order.

// Each binary only uses a subset of the symbols here; the other half
// looks "dead" to the compiler. Silence at module scope.
#![allow(dead_code)]

use iceoryx2::prelude::ZeroCopySend;
use serde::{Deserialize, Serialize};
use wingfoil::*;

// ── iceoryx2 services (shared memory pipes between ws_server and fix_gw) ──
pub const SVC_ORDERS: &str = "wingfoil/latency_e2e/orders";
pub const SVC_FILLS: &str = "wingfoil/latency_e2e/fills";

// ── WebSocket topics (ws_server ↔ browser) ────────────────────────────────
pub const TOPIC_ORDERS: &str = "orders";
pub const TOPIC_FILLS: &str = "fills";
pub const TOPIC_ECHO: &str = "latency_echo";
pub const TOPIC_SESSION: &str = "session";

pub type SessionId = [u8; 16];

pub const SIDE_BUY: u8 = 0;
pub const SIDE_SELL: u8 = 1;

/// Shared-memory payload that traverses ws_server → iceoryx2 → fix_gw
/// and then, after fill matching, fix_gw → iceoryx2 → ws_server.
///
/// `#[repr(C)]` + `ZeroCopySend` so the whole `Traced<RoundTrip, RoundTripLatency>`
/// can be transported by iceoryx2 without serialization.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend, Serialize, Deserialize)]
pub struct RoundTrip {
    pub session: SessionId,
    pub client_seq: u64,
    pub qty: u64,
    pub filled_qty: u64,
    /// Fill price × 10 000 (four decimal places). Zero until filled.
    pub fill_price_bps: i64,
    /// Browser `performance.now()` in nanoseconds at order submit.
    /// This is in the client clock domain, not the server's.
    pub t_client_send: u64,
    pub side: u8,
    /// Explicit padding to keep the struct layout and `[u64; N]` alignment
    /// stable across compilers and make the `ZeroCopySend` contract obvious.
    pub _pad: [u8; 7],
}

// Stage order = offset order in the on-wire [u64; 9] view of the record.
// The first five stages are stamped on the outbound leg, the last four on
// the inbound leg after the fill is matched.
latency_stages! {
    pub RoundTripLatency {
        ws_recv,
        ws_publish,
        gw_recv,
        gw_price,
        fix_send,
        fix_recv,
        gw_publish,
        ws_sub_recv,
        ws_send,
    }
}

// ── WebSocket wire types (JSON by default for easy browser devtools) ──────

/// Browser → ws_server on TOPIC_ORDERS.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrderFrame {
    pub session: SessionId,
    pub client_seq: u64,
    pub side: u8,
    pub qty: u64,
    pub t_client_send: u64,
}

/// ws_server → browser on TOPIC_FILLS. Carries the full set of server-side
/// stamps so the browser can render per-hop latency bars.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FillFrame {
    pub session: SessionId,
    pub client_seq: u64,
    pub side: u8,
    pub filled_qty: u64,
    pub fill_price_bps: i64,
    pub t_client_send: u64,
    pub stamps: [u64; 9],
}

/// Browser → ws_server on TOPIC_ECHO, carrying `t_client_recv` stamped in
/// the browser plus the full server-side stamps vector. Used for
/// round-trip aggregation (LatencyReport on the server).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EchoFrame {
    pub session: SessionId,
    pub client_seq: u64,
    pub t_client_send: u64,
    pub t_client_recv: u64,
    pub stamps: [u64; 9],
}

/// ws_server → browser on TOPIC_SESSION. Lets the browser render queue
/// position / remaining time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStatus {
    pub session: SessionId,
    pub state: String,
    pub remaining_secs: u32,
    pub queue_pos: u32,
}

// ── Env var helpers ───────────────────────────────────────────────────────

pub fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
        .unwrap_or(false)
}

pub fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub fn env_string(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// Are intra-cycle precise TSC reads enabled? Flip via `--precise` CLI
/// flag or `WINGFOIL_PRECISE_STAMPS=1`. The `.stamp_precise_if` /
/// `.stamp_if` ops compile to zero cost when disabled.
pub fn precise_stamps_enabled() -> bool {
    env_flag("WINGFOIL_PRECISE_STAMPS") || std::env::args().any(|a| a == "--precise")
}

// ── Core pinning ──────────────────────────────────────────────────────────
//
// Both binaries run their hot graph cycle on the main thread (the one that
// calls `Graph::run()`). Pinning that thread to an isolated core is the
// dominant E2E-latency win — the adapter worker threads (web server, FIX
// session, iceoryx2 sub) are I/O-blocked most of the time. Pin the whole
// process with `taskset` if you also want to keep those off the hot core.
//
// Call `pin_current_from_env` *after* spawning adapter/runtime worker
// threads so those workers keep the default affinity mask. On Linux, a
// new thread inherits the creating thread's affinity, so pinning early
// would force every adapter worker onto the hot core.

/// Outcome of parsing a `WINGFOIL_PIN_*` core list.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ParsedCoreList {
    pub cores: Vec<usize>,
    pub bad_tokens: Vec<String>,
}

/// Parse a comma-separated core list. Empty/whitespace tokens are
/// ignored; non-numeric tokens are collected in `bad_tokens` so the
/// caller can fail closed and tell the operator what went wrong.
pub fn parse_core_list(raw: &str) -> ParsedCoreList {
    let mut out = ParsedCoreList::default();
    for token in raw.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed.parse::<usize>() {
            Ok(c) => out.cores.push(c),
            Err(_) => out.bad_tokens.push(trimmed.to_string()),
        }
    }
    out
}

/// Pin the calling thread to the given set of CPU cores. No-op on
/// non-Linux. Empty `cores` is a no-op. Errors if any core is `>=
/// CPU_SETSIZE` (libc's `CPU_SET` is UB on out-of-range indices).
#[cfg(target_os = "linux")]
pub fn pin_current(cores: &[usize]) -> anyhow::Result<()> {
    if cores.is_empty() {
        return Ok(());
    }
    let max = libc::CPU_SETSIZE as usize;
    if let Some(&bad) = cores.iter().find(|&&c| c >= max) {
        anyhow::bail!("core {bad} is out of range (CPU_SETSIZE = {max})");
    }
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        for &c in cores {
            libc::CPU_SET(c, &mut set);
        }
        let rc = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn pin_current(_cores: &[usize]) -> anyhow::Result<()> {
    Ok(())
}

/// Read a comma-separated core list from `name` (e.g. `WINGFOIL_PIN_GRAPH=2`
/// or `=2,3`) and pin the calling thread. Logs the outcome; never panics.
///
/// Fails closed: if any token in the env var doesn't parse as a core
/// index, no pinning happens. Silent partial pinning would be a footgun
/// for latency tuning — a typo shouldn't quietly land the graph thread
/// on a different core than the operator intended.
pub fn pin_current_from_env(name: &str) {
    let Ok(raw) = std::env::var(name) else {
        return;
    };
    let parsed = parse_core_list(&raw);
    if !parsed.bad_tokens.is_empty() {
        log::warn!(
            "{name}={raw:?} has unparseable token(s) {:?}; not pinning",
            parsed.bad_tokens,
        );
        return;
    }
    if parsed.cores.is_empty() {
        log::warn!("{name}={raw:?} parsed as empty core list; not pinning");
        return;
    }
    match pin_current(&parsed.cores) {
        Ok(()) => log::info!(
            "pinned current thread to cores {:?} (via {name})",
            parsed.cores,
        ),
        Err(e) => log::warn!("failed to pin to cores {:?} from {name}: {e}", parsed.cores,),
    }
}

/// Format a session UUID as 32 hex chars (e.g. for log lines / metric keys).
pub fn session_hex(id: &SessionId) -> String {
    let mut s = String::with_capacity(32);
    for b in id {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_core_list_single() {
        let p = parse_core_list("2");
        assert_eq!(p.cores, vec![2]);
        assert!(p.bad_tokens.is_empty());
    }

    #[test]
    fn parse_core_list_multiple_and_whitespace() {
        let p = parse_core_list("2, 3 ,5");
        assert_eq!(p.cores, vec![2, 3, 5]);
        assert!(p.bad_tokens.is_empty());
    }

    #[test]
    fn parse_core_list_empty_tokens_skipped() {
        let p = parse_core_list("2,,3, ,4");
        assert_eq!(p.cores, vec![2, 3, 4]);
        assert!(p.bad_tokens.is_empty());
    }

    #[test]
    fn parse_core_list_collects_bad_tokens() {
        let p = parse_core_list("2,abc,3,-1");
        assert_eq!(p.cores, vec![2, 3]);
        assert_eq!(p.bad_tokens, vec!["abc".to_string(), "-1".to_string()]);
    }

    #[test]
    fn parse_core_list_all_bad() {
        let p = parse_core_list("foo,bar");
        assert!(p.cores.is_empty());
        assert_eq!(p.bad_tokens, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn parse_core_list_empty_input() {
        let p = parse_core_list("");
        assert!(p.cores.is_empty());
        assert!(p.bad_tokens.is_empty());
    }

    #[test]
    fn pin_current_empty_is_noop() {
        // Must succeed regardless of platform — empty input never touches
        // the kernel.
        assert!(pin_current(&[]).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pin_current_rejects_out_of_range_core() {
        let huge = libc::CPU_SETSIZE as usize;
        let err = pin_current(&[huge]).unwrap_err();
        assert!(
            err.to_string().contains("out of range"),
            "unexpected error: {err}",
        );
    }
}
