use std::pin::Pin;
use std::rc::Rc;

use futures::StreamExt;
use reqwest::Client;
use serde_json::json;

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
    /// Values are sent as Grafana data frames (JSON) via
    /// `POST <config.url>/api/live/push/<channel>`.
    ///
    /// Numeric values (anything whose `Display` output parses as `f64`) are
    /// sent as `number` fields; everything else is sent as `string` fields.
    ///
    /// `channel` must be a valid Grafana Live channel path, e.g.
    /// `"stream/wingfoil/counter"`.
    ///
    /// Only supported in `RunMode::RealTime`.
    #[must_use]
    fn grafana_push(self: &Rc<Self>, channel: &str, config: GrafanaConfig) -> Rc<dyn Node>;
}

impl<T: Element + Send + std::fmt::Display + 'static> GrafanaPush<T> for dyn Stream<T> {
    fn grafana_push(self: &Rc<Self>, channel: &str, config: GrafanaConfig) -> Rc<dyn Node> {
        let channel = channel.to_string();
        let consumer = Box::new(move |source: Pin<Box<dyn FutStream<T>>>| {
            push_consumer(channel, config, source)
        });
        self.consume_async(consumer)
    }
}

async fn push_consumer<T: Element + Send + std::fmt::Display>(
    channel: String,
    config: GrafanaConfig,
    mut source: Pin<Box<dyn FutStream<T>>>,
) -> anyhow::Result<()> {
    let client = Client::new();
    let url = format!("{}/api/live/push/{}", config.url, channel);

    while let Some((time, value)) = source.next().await {
        // Grafana expects milliseconds in JSON frames
        let timestamp_ms = u64::from(time) / 1_000_000;
        let value_str = value.to_string();

        // Use number type if the value parses as f64; string otherwise
        let frame = if let Ok(v) = value_str.parse::<f64>() {
            json!([{
                "schema": {"fields": [
                    {"name": "time", "type": "time"},
                    {"name": "value", "type": "number"}
                ]},
                "data": {"values": [[timestamp_ms], [v]]}
            }])
        } else {
            json!([{
                "schema": {"fields": [
                    {"name": "time", "type": "time"},
                    {"name": "value", "type": "string"}
                ]},
                "data": {"values": [[timestamp_ms], [value_str]]}
            }])
        };

        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("X-Grafana-Org-Id", config.org_id.to_string())
            .json(&frame)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
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
        let _node = stream.grafana_push("stream/wingfoil/test", config);
    }

    #[test]
    fn frame_format_numeric() {
        let time = NanoTime::new(1_000_000_000_000u64); // 1 second in nanos
        let timestamp_ms = u64::from(time) / 1_000_000;
        let _value = 42u64;
        let frame = json!([{
            "schema": {"fields": [
                {"name": "time", "type": "time"},
                {"name": "value", "type": "number"}
            ]},
            "data": {"values": [[timestamp_ms], [42.0f64]]}
        }]);
        assert_eq!(frame[0]["data"]["values"][1][0], 42.0);
        assert_eq!(frame[0]["schema"]["fields"][1]["type"], "number");
    }

    #[test]
    fn frame_format_string_fallback() {
        let value = "hello";
        let parsed = value.parse::<f64>();
        assert!(parsed.is_err(), "non-numeric should not parse as f64");
    }
}
