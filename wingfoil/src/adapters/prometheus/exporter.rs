use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{Element, GraphState, IntoNode, MutableNode, Node, Stream, UpStreams};

const READ_TIMEOUT: Duration = Duration::from_secs(5);

type MetricStore = Arc<Mutex<HashMap<String, String>>>;

/// Serves a Prometheus-compatible `GET /metrics` endpoint.
///
/// Register streams with [`register`](PrometheusExporter::register), call
/// [`serve`](PrometheusExporter::serve) to start the HTTP thread, then run
/// the returned sink nodes as part of your graph.
pub struct PrometheusExporter {
    addr: String,
    metrics: MetricStore,
}

impl PrometheusExporter {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: addr.into(),
            metrics: Arc::new(Mutex::new(HashMap::new())),
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
        let metrics = self.metrics.clone();
        std::thread::spawn(move || run_server(listener, metrics));
        Ok(port)
    }

    /// Register a stream as a Prometheus gauge metric.
    ///
    /// Returns a sink `Rc<dyn Node>` that must be included in your graph (or
    /// run directly). The node updates the metric value on every tick.
    ///
    /// `name` should follow Prometheus naming conventions, e.g.
    /// `wingfoil_counter_total`.
    pub fn register<T>(&self, name: impl Into<String>, stream: Rc<dyn Stream<T>>) -> Rc<dyn Node>
    where
        T: Element + std::fmt::Display,
    {
        PrometheusMetricNode {
            name: name.into(),
            stream,
            metrics: self.metrics.clone(),
        }
        .into_node()
    }
}

fn run_server(listener: TcpListener, metrics: MetricStore) {
    for stream in listener.incoming() {
        match stream {
            Ok(conn) => handle_connection(conn, &metrics),
            Err(_) => break,
        }
    }
}

fn handle_connection(mut conn: TcpStream, metrics: &MetricStore) {
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

    if !request_line.starts_with("GET /metrics") {
        if let Err(e) = conn.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n") {
            log::warn!("PrometheusExporter: failed to write 404 response: {e}");
        }
        return;
    }

    let body = build_metrics_body(metrics);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body,
    );
    if let Err(e) = conn.write_all(response.as_bytes()) {
        log::warn!("PrometheusExporter: failed to write metrics response: {e}");
    }
}

fn build_metrics_body(metrics: &MetricStore) -> String {
    let store = metrics.lock().unwrap();
    let mut out = String::new();
    for (name, value) in store.iter() {
        out.push_str(&format!("# TYPE {name} gauge\n{name} {value}\n"));
    }
    out
}

struct PrometheusMetricNode<T: Element> {
    name: String,
    stream: Rc<dyn Stream<T>>,
    metrics: MetricStore,
}

impl<T: Element + std::fmt::Display> MutableNode for PrometheusMetricNode<T> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        let value = self.stream.peek_value().to_string();
        self.metrics
            .lock()
            .map_err(|_| anyhow::anyhow!("PrometheusExporter: metrics lock poisoned"))?
            .insert(self.name.clone(), value);
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
    fn serves_registered_metric() {
        let port = 19091u16;
        let exporter = PrometheusExporter::new(format!("127.0.0.1:{port}"));
        exporter.serve().unwrap();

        let counter = ticker(Duration::from_millis(10)).count();
        let node = exporter.register("test_counter", counter);

        node.run(
            RunMode::HistoricalFrom(crate::NanoTime::ZERO),
            RunFor::Cycles(5),
        )
        .unwrap();

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
    fn returns_404_for_unknown_path() {
        let port = 19092u16;
        let exporter = PrometheusExporter::new(format!("127.0.0.1:{port}"));
        exporter.serve().unwrap();
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
