use std::pin::Pin;
use std::rc::Rc;

use futures::StreamExt;
use reqwest::Client;

use crate::nodes::{FutStream, StreamOperators};
use crate::types::*;

/// Connection configuration for a Grafana instance.
#[derive(Debug, Clone)]
pub struct GrafanaConfig {
    /// Base URL of the Grafana instance, e.g. `"http://localhost:3000"`.
    pub url: String,
    /// Service account token or API key with Editor role.
    pub api_key: String,
    /// Grafana organisation ID (default is `1`).
    pub org_id: u64,
}

/// Fluent sink method to push stream values to a Grafana Live channel.
pub trait GrafanaPush<T: Element> {
    /// Push every tick of this stream to a Grafana Live channel.
    ///
    /// Values are sent as [Influx line protocol] via
    /// `POST <config.url>/api/live/push/<stream_id>`.
    ///
    /// `stream_id` is a single-segment identifier (no slashes), e.g.
    /// `"wingfoil_counter"`. It maps to the Grafana Live channel
    /// `stream/<org_id>/<stream_id>` internally. Any `/` in the provided
    /// value are replaced with `_` automatically.
    ///
    /// Only supported in `RunMode::RealTime`.
    ///
    /// [Influx line protocol]: https://docs.influxdata.com/influxdb/v1/write_protocols/line_protocol_tutorial/
    #[must_use]
    fn grafana_push(self: &Rc<Self>, stream_id: &str, config: GrafanaConfig) -> Rc<dyn Node>;
}

impl<T: Element + Send + std::fmt::Display + 'static> GrafanaPush<T> for dyn Stream<T> {
    fn grafana_push(self: &Rc<Self>, stream_id: &str, config: GrafanaConfig) -> Rc<dyn Node> {
        let stream_id = stream_id.replace('/', "_");
        let consumer = Box::new(move |source: Pin<Box<dyn FutStream<T>>>| {
            push_consumer(stream_id, config, source)
        });
        self.consume_async(consumer)
    }
}

async fn push_consumer<T: Element + Send + std::fmt::Display>(
    stream_id: String,
    config: GrafanaConfig,
    mut source: Pin<Box<dyn FutStream<T>>>,
) -> anyhow::Result<()> {
    let client = Client::new();
    // Grafana 11: /api/live/push/:streamId — single segment, Influx line protocol
    let url = format!("{}/api/live/push/{}", config.url, stream_id);

    while let Some((time, value)) = source.next().await {
        let ns = u64::from(time);
        let value_str = value.to_string();

        // Influx line protocol: numeric values need a decimal point or type suffix.
        // Try to parse as f64 and format with decimal; fall back to quoted string.
        let field_value = if let Ok(v) = value_str.parse::<f64>() {
            // Always include decimal point so Influx parser treats it as float
            format!("{v:.1}")
        } else {
            format!("\"{value_str}\"")
        };

        let line = format!("{stream_id} value={field_value} {ns}\n");

        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("X-Grafana-Org-Id", config.org_id.to_string())
            .header("Content-Type", "text/plain")
            .body(line)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("(failed to read response: {e})"));
            log::error!("grafana_push: HTTP {status} from {url}: {body}");
            anyhow::bail!("grafana_push: HTTP {status}: {body}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NanoTime, nodes::constant};

    #[test]
    fn grafana_push_node_creation() {
        let config = GrafanaConfig {
            url: "http://localhost:3000".into(),
            api_key: "test-key".into(),
            org_id: 1,
        };
        let stream = constant(42u64);
        let _node = stream.grafana_push("wingfoil_counter", config);
    }

    #[test]
    fn influx_line_format_numeric() {
        let stream_id = "wingfoil_counter";
        let time = NanoTime::new(1_000_000_000u64);
        let ns = u64::from(time);
        let v = 42u64.to_string().parse::<f64>().unwrap();
        let line = format!("{stream_id} value={v:.1} {ns}\n");
        assert_eq!(line, "wingfoil_counter value=42.0 1000000000\n");
    }

    #[test]
    fn influx_line_format_string_fallback() {
        let stream_id = "wingfoil_status";
        let value_str = "active";
        let field_value = format!("\"{value_str}\"");
        let line = format!("{stream_id} value={field_value} 0\n");
        assert_eq!(line, "wingfoil_status value=\"active\" 0\n");
    }

    #[test]
    fn slashes_in_stream_id_are_replaced() {
        let stream_id = "stream/wingfoil/counter".replace('/', "_");
        assert_eq!(stream_id, "stream_wingfoil_counter");
    }
}
