use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwapOption;

use crate::{Element, GraphState, IntoNode, MutableNode, Node, RunMode, Stream, UpStreams};

const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Latest stringified value for a single metric. `ArcSwapOption` lets `cycle()`
/// publish a new value with a lock-free atomic pointer swap, and the HTTP
/// scrape thread read it without coordinating with the graph thread. `None`
/// means "never ticked" — such slots are omitted from the scrape response so
/// historical-mode graphs produce an empty body.
type MetricSlot = Arc<ArcSwapOption<String>>;

/// Registry of all metrics the HTTP thread should render. Only locked when a
/// new metric is registered (wiring time) and once per HTTP scrape (off the
/// graph thread) — never from `cycle()`.
type Registry = Arc<Mutex<Vec<(String, MetricSlot)>>>;

/// Serves a Prometheus-compatible `GET /metrics` endpoint.
///
/// Register streams with [`register`](PrometheusExporter::register), call
/// [`serve`](PrometheusExporter::serve) to start the HTTP thread, then run
/// the returned sink nodes as part of your graph.
pub struct PrometheusExporter {
    addr: String,
    registry: Registry,
}

impl PrometheusExporter {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            registry: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Spawn the HTTP server thread. Binds the listener synchronously so bind
    /// errors are returned immediately, before the graph starts.
    ///
    /// Returns the port that was actually bound — useful when `addr` specifies
    /// port `0` for OS-assigned port selection.
    pub fn serve(&self) -> Result<u16, std::io::Error> {
        let listener = TcpListener::bind(&self.addr)?;
        let port = listener.local_addr()?.port();
        let registry = self.registry.clone();
        std::thread::spawn(move || run_server(listener, registry));
        Ok(port)
    }

    /// Register a stream as a Prometheus gauge metric.
    ///
    /// Returns a sink `Rc<dyn Node>` that must be included in your graph (or
    /// run directly). The node updates the metric value on every tick.
    ///
    /// `name` should follow Prometheus naming conventions, e.g.
    /// `wingfoil_counter_total`.
    #[must_use = "register returns a Node that must be added to the graph or run directly"]
    pub fn register<T>(&self, name: impl Into<String>, stream: Rc<dyn Stream<T>>) -> Rc<dyn Node>
    where
        T: Element + std::fmt::Display,
    {
        let name = name.into();
        let slot: MetricSlot = Arc::new(ArcSwapOption::empty());
        self.registry
            .lock()
            .expect("PrometheusExporter: registry lock poisoned")
            .push((name, slot.clone()));
        PrometheusMetricNode {
            stream,
            slot,
            historical: false,
        }
        .into_node()
    }
}

fn run_server(listener: TcpListener, registry: Registry) {
    for stream in listener.incoming() {
        match stream {
            Ok(conn) => handle_connection(conn, &registry),
            Err(_) => break,
        }
    }
}

fn handle_connection(mut conn: TcpStream, registry: &Registry) {
    if let Err(e) = conn.set_read_timeout(Some(READ_TIMEOUT)) {
        log::warn!("PrometheusExporter: failed to set read timeout: {e}");
    }
    let mut reader = BufReader::new(&conn);

    let mut request_line = String::new();
    if let Err(e) = reader.read_line(&mut request_line) {
        if !matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ) {
            log::warn!("PrometheusExporter: failed to read request: {e}");
        }
        return;
    }
    // Drain HTTP headers
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            _ => {}
        }
    }

    if !request_line.starts_with("GET /metrics HTTP") {
        if let Err(e) = conn.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n") {
            log::warn!("PrometheusExporter: failed to write 404 response: {e}");
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
        log::warn!("PrometheusExporter: failed to write metrics response: {e}");
    }
}

fn build_metrics_body(registry: &Registry) -> String {
    // Snapshot the registry under the lock, then release it before loading any
    // slots so a slow serialize can never block a concurrent register() call.
    let snapshot: Vec<(String, MetricSlot)> = match registry.lock() {
        Ok(g) => g.clone(),
        Err(_) => {
            log::warn!("PrometheusExporter: registry lock poisoned, serving empty body");
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

struct PrometheusMetricNode<T: Element> {
    stream: Rc<dyn Stream<T>>,
    slot: MetricSlot,
    historical: bool,
}

impl<T: Element + std::fmt::Display> MutableNode for PrometheusMetricNode<T> {
    fn setup(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.historical = matches!(state.run_mode(), RunMode::HistoricalFrom(_));
        Ok(())
    }

    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        if self.historical {
            return Ok(true);
        }
        let value = self.stream.peek_value().to_string();
        self.slot.store(Some(Arc::new(value)));
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.stream.clone().as_node()], vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        RunFor, RunMode,
        nodes::{NodeOperators, ticker},
    };
    use std::time::Duration;

    fn get_metrics(port: u16) -> String {
        // Retry briefly in case the server hasn't bound yet
        for _ in 0..20 {
            if let Ok(mut conn) = std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
                conn.write_all(b"GET /metrics HTTP/1.0\r\n\r\n").unwrap();
                let mut response = String::new();
                use std::io::Read;
                conn.read_to_string(&mut response).unwrap();
                // Strip HTTP headers
                if let Some(pos) = response.find("\r\n\r\n") {
                    return response[pos + 4..].to_string();
                }
                return response;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("could not connect to metrics server on port {port}");
    }

    #[test]
    fn connection_refused_when_port_occupied() {
        // Occupy a port so the exporter cannot bind it.
        let occupied =
            std::net::TcpListener::bind("127.0.0.1:0").expect("failed to bind test listener");
        let port = occupied.local_addr().unwrap().port();
        let exporter = PrometheusExporter::new(format!("127.0.0.1:{port}"));
        let result = exporter.serve();
        assert!(result.is_err(), "expected bind error when port is occupied");
    }

    #[test]
    fn serves_registered_metric() {
        let exporter = PrometheusExporter::new("127.0.0.1:0");
        let port = exporter.serve().unwrap();

        let counter = ticker(Duration::from_millis(10)).count();
        let node = exporter.register("test_counter", counter);

        node.run(RunMode::RealTime, RunFor::Cycles(5)).unwrap();

        let body = get_metrics(port);
        assert!(
            body.contains("test_counter 5"),
            "expected 'test_counter 5' in:\n{body}"
        );
        assert!(
            body.contains("# TYPE test_counter gauge"),
            "expected TYPE line in:\n{body}"
        );
    }

    #[test]
    fn historical_mode_produces_no_metrics() {
        let exporter = PrometheusExporter::new("127.0.0.1:0");
        let port = exporter.serve().unwrap();

        let counter = ticker(Duration::from_millis(10)).count();
        let node = exporter.register("hist_counter", counter);

        node.run(
            RunMode::HistoricalFrom(crate::NanoTime::ZERO),
            RunFor::Cycles(5),
        )
        .unwrap();

        let body = get_metrics(port);
        assert!(
            body.is_empty(),
            "expected no metrics in historical mode, got:\n{body}"
        );
    }

    #[test]
    fn returns_404_for_unknown_path() {
        let exporter = PrometheusExporter::new("127.0.0.1:0");
        let port = exporter.serve().unwrap();
        std::thread::sleep(Duration::from_millis(50));

        let mut conn = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        conn.write_all(b"GET /other HTTP/1.0\r\n\r\n").unwrap();
        let mut response = String::new();
        use std::io::Read;
        conn.read_to_string(&mut response).unwrap();
        assert!(
            response.starts_with("HTTP/1.1 404"),
            "expected 404, got: {response}"
        );
    }
}
