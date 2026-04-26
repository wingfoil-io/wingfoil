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

/// Pin the calling thread to the given set of CPU cores. No-op on
/// non-Linux. Empty `cores` is a no-op.
#[cfg(target_os = "linux")]
pub fn pin_current(cores: &[usize]) -> anyhow::Result<()> {
    if cores.is_empty() {
        return Ok(());
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
pub fn pin_current_from_env(name: &str) {
    let Ok(raw) = std::env::var(name) else {
        return;
    };
    let cores: Vec<usize> = raw
        .split(',')
        .filter_map(|c| c.trim().parse().ok())
        .collect();
    if cores.is_empty() {
        log::warn!("{name}={raw:?} parsed as empty core list; not pinning");
        return;
    }
    match pin_current(&cores) {
        Ok(()) => log::info!("pinned current thread to cores {cores:?} (via {name})"),
        Err(e) => log::warn!("failed to pin to cores {cores:?} from {name}: {e}"),
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
