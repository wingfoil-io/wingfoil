//! prometheus adapter — a realtime, pull-based metrics **sink**: it serves a
//! hand-rolled `GET /metrics` endpoint in Prometheus text format so Grafana (or
//! any Prometheus-compatible system) can scrape stream values. It ports the
//! legacy `wingfoil::adapters::prometheus` module onto the Op model.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`statistics`](crate::adapters::statistics)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude) and is gated
//! behind the `prometheus` feature (it pulls in `arc-swap` only — the HTTP
//! server is `std::net`, no Prometheus client crate). Bring in what you need
//! explicitly:
//!
//! - **Exporter** — [`PrometheusExporter`]: owns the metric registry and spawns
//!   the HTTP server thread ([`serve`](PrometheusExporter::serve)).
//! - **Sink** — the [`PrometheusSinkOps`] extension trait on any `Stream<T>`
//!   whose values are [`Display`], enabled with
//!   `use wingfoil::adapters::prometheus::PrometheusSinkOps;`. Register a
//!   stream as a gauge with [`prometheus_gauge`](PrometheusSinkOps::prometheus_gauge)
//!   and add the returned sink `Stream<()>` to the graph.
//!
//! # Sink (the metric slot model)
//!
//! Each registered stream gets its own lock-free
//! `Arc<ArcSwapOption<String>>` slot. On every realtime cycle the sink
//! stringifies the stream's current value and publishes it into the slot with a
//! single atomic pointer swap — never a lock on the graph thread. The HTTP
//! scrape thread snapshots the registry (locked only at registration and once
//! per scrape, off the graph thread), then loads each slot to render the
//! response. A slot that has never been written (`None`) is omitted from the
//! response.
//!
//! # Historical / backtesting mode
//!
//! The exporter is a **realtime** concept. Under
//! [`RunMode::HistoricalFrom`](wingfoil::RunMode::HistoricalFrom) the sink is a
//! no-op — no slot is written — so a backtest never publishes fast-forwarded
//! values to a live endpoint. The HTTP server (if [`serve`](PrometheusExporter::serve)
//! was called) still answers, but with an empty body. This mirrors legacy,
//! whose metric node detected the run mode in `setup` and short-circuited
//! `cycle`; wingfoil reads it from [`Ctx::run_mode`](crate::op::Ctx::run_mode) in
//! the cycle itself.
//!
//! # Deviations from legacy
//!
//! Every legacy *capability* (the `GET /metrics` text endpoint, per-metric
//! slots omitted until first written, the historical no-op, the 404 for other
//! paths, synchronous bind so errors surface before the run) is preserved. The
//! surface differs in three deliberate ways:
//!
//! 1. **The sink is an extension trait, not an exporter method.** Legacy called
//!    `exporter.register(name, stream)`; wingfoil uses the sink-as-trait convention
//!    shared with [`lines`](crate::adapters::lines) / [`etcd`](crate::adapters::etcd):
//!    `stream.prometheus_gauge(&exporter, name)`. The exporter still owns the
//!    registry; the trait method registers a slot in it and wires the sink.
//! 2. **`serve` returns [`anyhow::Result`].** Legacy returned
//!    `Result<u16, std::io::Error>`; wingfoil attaches `.context` at the bind, per
//!    the fallible-with-context convention. The bind is still synchronous, so a
//!    bind error surfaces at `serve()` time, before the run.
//! 3. **The exporter owns the server lifetime.** Dropping it closes every
//!    listener and joins the HTTP threads instead of leaving them detached.
//!    Keep the exporter alive for as long as the endpoint is needed.
//!
//! # Setup (integration test)
//!
//! The end-to-end Prometheus scrape test reuses the telemetry example's stack,
//! which scrapes `WINGFOIL_METRICS_PORT` (9091) on the host:
//!
//! ```sh
//! docker compose -f crates/wingfoil/examples/adapters/telemetry/docker/docker-compose.yml up -d
//! ```

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use arc_swap::ArcSwapOption;
use wingfoil::RunMode;

use crate::fluent::Stream;
use crate::op::{Activation, Ctx, Tick};

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Latest stringified value for a single metric. `ArcSwapOption` lets the sink
/// cycle publish a new value with a lock-free atomic pointer swap, and the HTTP
/// scrape thread read it without coordinating with the graph thread. `None`
/// means "never ticked" — such slots are omitted from the scrape response so
/// historical-mode graphs produce an empty body.
type MetricSlot = Arc<ArcSwapOption<String>>;

/// Registry of all metrics the HTTP thread should render. Only locked when a
/// new metric is registered (wiring time) and once per HTTP scrape (off the
/// graph thread) — never from a sink cycle.
type Registry = Arc<Mutex<Vec<(String, MetricSlot)>>>;

/// Serves a Prometheus-compatible `GET /metrics` endpoint.
///
/// Register streams as gauges with
/// [`PrometheusSinkOps::prometheus_gauge`], call
/// [`serve`](PrometheusExporter::serve) to start the HTTP thread, then run the
/// returned sink streams as part of your graph.
/// Keep the exporter alive while scraping: dropping it closes all listeners,
/// interrupts any active scrape and joins the HTTP threads.
pub struct PrometheusExporter {
    addr: String,
    registry: Registry,
    servers: Mutex<Vec<Server>>,
}

struct Server {
    shutdown: Option<mpsc::Sender<()>>,
    thread: JoinHandle<()>,
}

impl PrometheusExporter {
    /// Create an exporter bound (on [`serve`](Self::serve)) to `addr`, e.g.
    /// `"0.0.0.0:9091"` or `"127.0.0.1:0"` for an OS-assigned port.
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            registry: Arc::new(Mutex::new(Vec::new())),
            servers: Mutex::new(Vec::new()),
        }
    }

    /// Spawn the HTTP server thread. Binds the listener synchronously so bind
    /// errors are returned immediately, before the graph starts.
    ///
    /// Returns the port that was actually bound — useful when `addr` specifies
    /// port `0` for OS-assigned port selection.
    ///
    /// # Errors
    ///
    /// Returns an error if the listener cannot bind `addr` (e.g. the port is
    /// already in use), cannot be configured, or the server thread cannot start.
    pub fn serve(&self) -> Result<u16> {
        let listener = TcpListener::bind(&self.addr)
            .with_context(|| format!("prometheus: binding metrics server to {}", self.addr))?;
        let port = listener
            .local_addr()
            .context("prometheus: reading bound local address")?
            .port();
        listener
            .set_nonblocking(true)
            .context("prometheus: setting listener nonblocking")?;
        let registry = self.registry.clone();
        let (shutdown, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("prometheus-exporter".into())
            .spawn(move || run_server(listener, registry, receiver))
            .context("prometheus: spawning metrics server thread")?;
        self.servers
            .lock()
            .expect("prometheus servers mutex poisoned")
            .push(Server {
                shutdown: Some(shutdown),
                thread,
            });
        Ok(port)
    }

    /// Register a metric name and return its (initially empty) slot, pushing the
    /// `(name, slot)` pair into the shared registry. Locks the registry once, at
    /// wiring time — never on the graph thread.
    fn register_metric(&self, name: String) -> MetricSlot {
        let slot: MetricSlot = Arc::new(ArcSwapOption::empty());
        self.registry
            .lock()
            .expect("prometheus registry mutex poisoned")
            .push((name, slot.clone()));
        slot
    }
}

impl Drop for PrometheusExporter {
    fn drop(&mut self) {
        let servers = self
            .servers
            .get_mut()
            .expect("prometheus servers mutex poisoned");
        for server in servers.iter_mut() {
            // Disconnection stays observable across every accept/read/write.
            drop(server.shutdown.take());
        }
        for server in servers.drain(..) {
            if server.thread.join().is_err() {
                log::warn!("prometheus: metrics server thread panicked");
            }
        }
    }
}

fn run_server(listener: TcpListener, registry: Registry, shutdown: mpsc::Receiver<()>) {
    loop {
        if !matches!(shutdown.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            break;
        }
        match listener.accept() {
            Ok((conn, _)) => {
                // Windows inherits the listener mode; the HTTP handler uses
                // blocking reads and writes on every platform.
                if let Err(e) = conn.set_nonblocking(false) {
                    log::warn!("prometheus: failed to set connection blocking: {e}");
                    continue;
                }
                handle_connection(conn, &registry, &shutdown);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if !matches!(
                    shutdown.recv_timeout(POLL_INTERVAL),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                log::warn!("prometheus: failed to accept connection: {e}");
                break;
            }
        }
    }
}

struct Connection<'a> {
    stream: TcpStream,
    shutdown: &'a mpsc::Receiver<()>,
}

impl Connection<'_> {
    fn until_shutdown(
        &self,
        mut operation: impl FnMut(&TcpStream) -> io::Result<usize>,
    ) -> io::Result<usize> {
        let start = Instant::now();
        loop {
            if !matches!(self.shutdown.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                return Ok(0);
            }
            match operation(&self.stream) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) && start.elapsed() < IO_TIMEOUT => {}
                result => return result,
            }
        }
    }
}

impl Read for Connection<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.until_shutdown(|mut stream| stream.read(buf))
    }
}

impl Write for Connection<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.until_shutdown(|mut stream| stream.write(buf))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn handle_connection(conn: TcpStream, registry: &Registry, shutdown: &mpsc::Receiver<()>) {
    // shutdown on a cloned Windows socket need not interrupt a pending recv.
    // Retry short timeouts below BufReader so partial UTF-8 lines stay intact.
    if let Err(e) = conn
        .set_read_timeout(Some(POLL_INTERVAL))
        .and_then(|()| conn.set_write_timeout(Some(POLL_INTERVAL)))
    {
        log::warn!("prometheus: failed to set socket timeouts: {e}");
        return;
    }
    let mut reader = BufReader::new(Connection {
        stream: conn,
        shutdown,
    });

    let mut request_line = String::new();
    if let Err(e) = reader.read_line(&mut request_line) {
        if !matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ) {
            log::warn!("prometheus: failed to read request: {e}");
        }
        return;
    }
    // Drain HTTP headers.
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            _ => {}
        }
    }

    let mut conn = reader.into_inner();

    if !request_line.starts_with("GET /metrics HTTP") {
        if let Err(e) = conn.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n") {
            log::warn!("prometheus: failed to write 404 response: {e}");
        }
        return;
    }

    let body = build_metrics_body(registry);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body,
    );
    if let Err(e) = conn.write_all(response.as_bytes()) {
        log::warn!("prometheus: failed to write metrics response: {e}");
    }
}

fn build_metrics_body(registry: &Registry) -> String {
    // Snapshot the registry under the lock, then release it before loading any
    // slots so a slow serialize can never block a concurrent registration.
    let snapshot: Vec<(String, MetricSlot)> = match registry.lock() {
        Ok(g) => g.clone(),
        Err(_) => {
            log::warn!("prometheus: registry lock poisoned, serving empty body");
            return String::new();
        }
    };
    let mut pairs: Vec<(String, Arc<String>)> = snapshot
        .into_iter()
        .filter_map(|(name, slot)| slot.load_full().map(|v| (name, v)))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (name, value) in pairs {
        out.push_str(&format!("# TYPE {name} gauge\n{name} {value}\n"));
    }
    out
}

/// A pull-based Prometheus metrics sink — an extension trait on any
/// `Stream<T>` whose values are [`Display`].
///
/// `use`ing it enables `stream.prometheus_gauge(&exporter, name)` chaining,
/// layered over the [`register_op1`](crate::interp::Builder::register_op1)
/// primitive the same way [`LinesSinkOps`](crate::adapters::lines::LinesSinkOps)
/// layers over `for_each`.
pub trait PrometheusSinkOps<T> {
    /// Register this stream as a Prometheus **gauge** named `name` on `exporter`
    /// and return the sink `Stream<()>` that publishes the stream's stringified
    /// value on every realtime cycle. The sink is a no-op under
    /// [`RunMode::HistoricalFrom`](wingfoil::RunMode::HistoricalFrom).
    ///
    /// `name` should follow Prometheus naming conventions, e.g.
    /// `wingfoil_counter_total`.
    ///
    /// The returned `Stream<()>` **must** be added to the graph (or built and
    /// run) for the metric to update.
    #[must_use = "prometheus_gauge returns a sink Stream that must be added to the graph"]
    fn prometheus_gauge(
        &self,
        exporter: &PrometheusExporter,
        name: impl Into<String>,
    ) -> Stream<()>;
}

impl<T> PrometheusSinkOps<T> for Stream<T>
where
    T: std::fmt::Display + 'static,
{
    fn prometheus_gauge(
        &self,
        exporter: &PrometheusExporter,
        name: impl Into<String>,
    ) -> Stream<()> {
        let slot = exporter.register_metric(name.into());
        self.wire(|b, h| {
            b.register_op1(
                h,
                "prometheus_gauge",
                Activation::NONE,
                slot,
                || (),
                move |slot: &mut MetricSlot, _state: &mut (), value: &T, ctx: &mut Ctx<'_>| {
                    // The exporter is a realtime endpoint; a backtest must not
                    // publish its fast-forwarded values. Leaving the slot as
                    // `None` keeps historical runs' scrape bodies empty.
                    if matches!(ctx.run_mode(), RunMode::HistoricalFrom(_)) {
                        return Ok(Tick::Value(()));
                    }
                    slot.store(Some(Arc::new(value.to_string())));
                    Ok(Tick::Value(()))
                },
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_timeouts_preserve_partial_utf8_lines() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(POLL_INTERVAL)).unwrap();
        let (_sender, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            client.write_all(b"\xc3").unwrap();
            std::thread::sleep(Duration::from_millis(150));
            client.write_all(b"\xa9\n").unwrap();
        });
        let mut reader = BufReader::new(Connection {
            stream,
            shutdown: &receiver,
        });
        let mut line = String::new();
        assert_eq!(reader.read_line(&mut line).unwrap(), 3);
        assert_eq!(line, "é\n");
        thread.join().unwrap();
    }

    #[test]
    fn idle_connection_keeps_the_five_second_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(POLL_INTERVAL)).unwrap();
        let (_sender, receiver) = mpsc::channel();
        let mut conn = Connection {
            stream,
            shutdown: &receiver,
        };
        let start = Instant::now();
        let error = conn.read(&mut [0]).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
        assert!(start.elapsed() >= IO_TIMEOUT);
    }

    #[test]
    fn shutdown_interrupts_a_pending_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(POLL_INTERVAL)).unwrap();
        let (shutdown, receiver) = mpsc::channel();
        let (started, ready) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let conn = Connection {
                stream,
                shutdown: &receiver,
            };
            let result = conn.until_shutdown(|mut stream| {
                let _ = started.send(());
                stream.read(&mut [0])
            });
            finished.send(result).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(shutdown);
        assert_eq!(
            done.recv_timeout(Duration::from_secs(2)).unwrap().unwrap(),
            0
        );
        thread.join().unwrap();
    }

    fn drop_promptly(exporter: PrometheusExporter) {
        let (finished, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            drop(exporter);
            finished.send(()).unwrap();
        });
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("drop must interrupt socket I/O, not wait for the read timeout");
        thread.join().unwrap();
    }

    #[test]
    fn drop_interrupts_idle_and_partial_requests() {
        for request in [b"".as_slice(), b"GET /met", b"GET /metrics HTTP/1.1\r\nX:"] {
            let exporter = PrometheusExporter::new("127.0.0.1:0");
            let port = exporter.serve().unwrap();
            let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
            client.write_all(request).unwrap();
            drop_promptly(exporter);
        }
    }

    #[test]
    fn drop_interrupts_a_scrape_that_is_not_reading() {
        let exporter = PrometheusExporter::new("127.0.0.1:0");
        exporter
            .register_metric("large".into())
            .store(Some(Arc::new("1".repeat(16 * 1024 * 1024))));
        let port = exporter.serve().unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.write_all(b"GET /metrics HTTP/1.0\r\n\r\n").unwrap();
        // Wait for the response to start without draining the receive buffer.
        assert_eq!(client.peek(&mut [0]).unwrap(), 1);
        drop_promptly(exporter);
    }

    #[test]
    fn drop_can_race_with_accept() {
        for _ in 0..50 {
            let exporter = PrometheusExporter::new("127.0.0.1:0");
            let port = exporter.serve().unwrap();
            let _client = TcpStream::connect(("127.0.0.1", port)).unwrap();
            drop_promptly(exporter);
        }
    }
}
