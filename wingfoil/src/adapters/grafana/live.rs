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
    /// Values are formatted as [Influx line protocol] and POSTed to
    /// `<config.url>/api/live/push/<channel>`.
    ///
    /// `channel` must be a valid Grafana Live channel path, e.g.
    /// `"stream/wingfoil/counter"`.
    ///
    /// Only supported in `RunMode::RealTime`.
    ///
    /// [Influx line protocol]: https://docs.influxdata.com/influxdb/v1/write_protocols/line_protocol_tutorial/
    #[must_use]
    fn grafana_push(self: &Rc<Self>, channel: &str, config: GrafanaConfig) -> Rc<dyn Node>;
}

impl<T: Element + Send + ToString + 'static> GrafanaPush<T> for dyn Stream<T> {
    fn grafana_push(self: &Rc<Self>, channel: &str, config: GrafanaConfig) -> Rc<dyn Node> {
        let channel = channel.to_string();
        let consumer = Box::new(move |source: Pin<Box<dyn FutStream<T>>>| {
            push_consumer(channel, config, source)
        });
        self.consume_async(consumer)
    }
}

async fn push_consumer<T: Element + Send + ToString>(
    channel: String,
    config: GrafanaConfig,
    mut source: Pin<Box<dyn FutStream<T>>>,
) -> anyhow::Result<()> {
    let client = Client::new();
    let url = format!("{}/api/live/push/{}", config.url, channel);
    // Influx measurement name: slashes aren't valid, replace with underscores
    let measurement = channel.replace('/', "_");

    while let Some((time, value)) = source.next().await {
        let ns = u64::from(time);
        // Influx line protocol: <measurement> value=<value> <timestamp_ns>
        let line = format!("{measurement} value={} {ns}\n", value.to_string());

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
            let body = resp.text().await.unwrap_or_default();
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
        // Verify that grafana_push creates a valid node without connecting
        let config = GrafanaConfig {
            url: "http://localhost:3000".into(),
            api_key: "test-key".into(),
            org_id: 1,
        };
        let stream = constant(42u64);
        // Node creation must not require a network connection
        let _node = stream.grafana_push("stream/wingfoil/test", config);
    }

    #[test]
    fn influx_line_format() {
        // Verify the line protocol format by inspecting what push_consumer would send.
        // We test the format string directly since the async consumer is hard to unit test.
        let channel = "stream/wingfoil/counter".to_string();
        let measurement = channel.replace('/', "_");
        let value = 42u64;
        let time = NanoTime::new(1_000_000_000u64);
        let ns = u64::from(time);
        let line = format!("{measurement} value={} {ns}\n", value.to_string());
        assert_eq!(line, "stream_wingfoil_counter value=42 1000000000\n");
    }
}
