#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil --features statistics --example slo_monitor
//! ```

use std::thread;
use std::time::Duration;

use wingfoil::adapters::statistics::StatisticsOps;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// One served HTTP request, as a load balancer would report it.
#[derive(Clone, Debug, Default)]
struct Request {
    endpoint: &'static str,
    latency_ms: f64,
    status: u16,
}

/// The endpoint this SLO covers.
const ENDPOINT: &str = "/checkout";
/// A request is "good" if it did not error and came back inside this budget.
const SLO_LATENCY_MS: f64 = 250.0;
/// The SLO: 99% of requests must be good, so 1% is the error budget.
const ERROR_BUDGET: f64 = 0.01;
/// How many recent requests the burn rate is measured over.
const WINDOW: usize = 64;
/// Page above this burn rate, and stay paged until it falls back below
/// `CLEAR` — a single threshold would flap once the rate sits on it.
const PAGE: f64 = 14.4;
const CLEAR: f64 = 6.0;

/// How often the status line is printed, and the width of the rate window.
const TICK: Duration = Duration::from_secs(1);

fn main() -> anyhow::Result<()> {
    let g = GraphBuilder::new();

    // Requests arrive from a producer thread. `send_at` stamps each one, which
    // is what makes this replay deterministic — the same feed always produces
    // the same alerts at the same instants.
    let (incoming, tx) = g.channel::<Request>();
    // One SLO covers one endpoint: the health check is served from cache and
    // would dilute the burn rate to nothing if it were counted.
    let requests = incoming
        .collapse::<Request>()
        .filter_value(|r| r.endpoint == ENDPOINT);

    // Did each request violate the SLO? 1.0 for a violation, 0.0 for a good one.
    let violation = requests.map(|r| {
        if r.status >= 500 || r.latency_ms > SLO_LATENCY_MS {
            1.0
        } else {
            0.0
        }
    });

    // Burn rate: how many times faster than budget we are spending it. 1.0x is
    // exactly on budget; 14.4x exhausts a 30-day budget in about two days.
    let burn = violation.rolling_mean(WINDOW).map(|r| r / ERROR_BUDGET);

    // Latency shape over the same window.
    let latency = requests.map(|r| r.latency_ms);
    let p50 = latency.rolling_median(WINDOW);
    let worst = latency.rolling_max(WINDOW);

    // Requests per second. A `time_windowed_*` op only re-evaluates — and so
    // only evicts aged samples — when its *input* ticks, so on its own it
    // freezes at the last value once traffic stops rather than decaying to
    // zero. Merging the clock in as a 0.0 sample keeps the window honest: it
    // contributes nothing to the sum but forces the eviction pass.
    let clock = g.ticker(TICK);
    let throughput = requests
        .map(|_| 1.0)
        .merge(&clock.map(|_| 0.0))
        .time_windowed_sum(TICK);

    // Hysteresis: page above PAGE, clear below CLEAR, hold in between. Deciding
    // the next alert state needs the previous one, which is a cycle — and a
    // feedback edge is the only construct that can express one. `join_passive`
    // reads the fed-back state without the read itself driving a tick.
    let (was_alerting, alert_sink) = g.feedback::<bool>();
    let alerting = burn
        .join_passive(
            &was_alerting,
            |b, prev| {
                if *prev { *b > CLEAR } else { *b > PAGE }
            },
        )
        .feedback(&alert_sink);

    // Only the edges are worth paging on, not every re-evaluation. `distinct`
    // emits its first value too, which here is the initial "not alerting" —
    // `skip(1)` drops that so only genuine transitions reach the sink.
    let transitions = alerting.distinct().skip(1);

    let _pages = transitions
        .with_time()
        .join_passive(&burn, |(t, on), b| {
            let state = if *on { "PAGE " } else { "CLEAR" };
            let at = Duration::from(*t).as_secs_f64();
            format!("[{at:>5.1}s] {state} {ENDPOINT}  burn={b:.1}x budget")
        })
        .for_each(|line| {
            println!("{line}");
            Ok(())
        });

    // A once-a-second status line, sampled off the same clock.
    let _status = p50
        .join3(&worst, &throughput, |p, w, t| (*p, *w, *t))
        .sample(&clock)
        .with_time()
        .for_each(|(t, (p, w, rps))| {
            let at = Duration::from(*t).as_secs_f64();
            println!("[{at:>5.1}s]        rps={rps:>4.0}  p50={p:>6.1}ms  max={w:>6.1}ms");
            Ok(())
        });

    let mut runner = g.build();

    let producer = thread::spawn(move || {
        // 6s of traffic at 500 rps, degrading between t=2s and t=3s.
        for i in 0..3_000u64 {
            let at = Duration::from_millis(i * 2);
            let degraded = (2_000..3_000).contains(&at.as_millis());
            let latency_ms = if degraded {
                180.0 + (i % 400) as f64
            } else {
                40.0 + (i % 60) as f64
            };
            let status = if degraded && i % 9 == 0 { 503 } else { 200 };
            tx.send_at(
                Request {
                    endpoint: ENDPOINT,
                    latency_ms,
                    status,
                },
                NanoTime::from(at),
            );
            // Background health-check traffic on another endpoint, which the
            // graph filters out.
            tx.send_at(
                Request {
                    endpoint: "/health",
                    latency_ms: 1.0,
                    status: 200,
                },
                NanoTime::from(at + Duration::from_micros(500)),
            );
        }
        // End-of-stream. Without this the historical run blocks waiting for
        // input that never comes — `RunFor` cannot bound that wait.
        tx.close();
    });

    println!("replaying 3000 requests to {ENDPOINT} (plus /health noise) ...");
    runner.run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Duration(Duration::from_secs(7)),
    )?;
    producer.join().expect("producer thread panicked");

    Ok(())
}
