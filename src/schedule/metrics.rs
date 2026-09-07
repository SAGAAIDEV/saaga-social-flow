//! Reading one post's performance back out of Buffer.
//!
//! Per-post numbers come from `post(input:)`, not `aggregatedPostMetrics` — that
//! one takes an org and a date range and has no `postIds` argument at all, so it
//! structurally cannot answer "how did this post do". The join key is the
//! `buffer_post_id` the ledger recorded when it was queued.
//!
//! `metrics` is a heterogeneous list, not a fixed set: each network emits its own
//! subset of `PostMetricType`, so nothing here flattens it into columns. The
//! report decides what to show; this layer preserves whatever came back.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::buffer::{graphql, BufferClient};

const POST_QUERY: &str = r#"query Post($input: PostInput!) {
  post(input: $input) {
    id status sentAt metricsUpdatedAt
    metrics { name type unit value }
  }
}"#;

/// One metric as Buffer reports it. `unit` is `count` or `percentage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricValue {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub unit: String,
    pub value: f64,
}

/// A post's status and numbers at one moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostMetrics {
    pub post_id: String,
    /// `draft | error | needs_approval | scheduled | sending | sent`
    pub status: String,
    /// When it actually went out. `None` until Buffer sends it — which is what
    /// every reporting window has to be measured from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics_updated_at: Option<String>,
    #[serde(default)]
    pub metrics: Vec<MetricValue>,
}

impl PostMetrics {
    pub fn is_sent(&self) -> bool {
        self.status == "sent"
    }
}

pub fn fetch(client: &BufferClient, post_id: &str) -> Result<PostMetrics> {
    let vars = json!({ "input": { "id": post_id } });
    let data = graphql(client.api_key(), POST_QUERY, vars)?;
    let payload = data
        .get("post")
        .filter(|value| !value.is_null())
        .with_context(|| format!("buffer returned no post for {post_id}"))?;
    Ok(parse(payload))
}

pub fn parse(raw: &Value) -> PostMetrics {
    PostMetrics {
        post_id: raw
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        status: raw
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        sent_at: opt_str(raw, "sentAt"),
        metrics_updated_at: opt_str(raw, "metricsUpdatedAt"),
        metrics: raw
            .get("metrics")
            .and_then(Value::as_array)
            .map(|items| items.iter().map(parse_metric).collect())
            .unwrap_or_default(),
    }
}

fn parse_metric(raw: &Value) -> MetricValue {
    MetricValue {
        name: raw
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        kind: raw
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        unit: raw
            .get("unit")
            .and_then(Value::as_str)
            .unwrap_or("count")
            .to_string(),
        value: raw.get("value").and_then(Value::as_f64).unwrap_or(0.0),
    }
}

fn opt_str(raw: &Value, key: &str) -> Option<String> {
    raw.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Metrics are a heterogeneous list, so tests look a name up rather than
    /// indexing — the order is the server's choice.
    fn metric(got: &PostMetrics, name: &str) -> Option<f64> {
        got.metrics
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(name))
            .map(|m| m.value)
    }

    fn sent_payload() -> Value {
        json!({
            "id": "post-9",
            "status": "sent",
            "sentAt": "2026-08-09T20:44:00Z",
            "metricsUpdatedAt": "2026-08-15T06:00:00Z",
            "metrics": [
                { "name": "impressions", "type": "impressions", "unit": "count", "value": 8120.0 },
                { "name": "saves", "type": "saves", "unit": "count", "value": 210.0 },
                { "name": "engagementRate", "type": "engagementRate", "unit": "percentage", "value": 6.8 }
            ]
        })
    }

    #[test]
    fn a_sent_post_parses_with_its_send_time_and_metrics() {
        let got = parse(&sent_payload());
        assert_eq!(got.post_id, "post-9");
        assert!(got.is_sent());
        assert_eq!(got.sent_at.as_deref(), Some("2026-08-09T20:44:00Z"));
        assert_eq!(got.metrics_updated_at.as_deref(), Some("2026-08-15T06:00:00Z"));
        assert_eq!(got.metrics.len(), 3);
        assert_eq!(metric(&got, "impressions"), Some(8120.0));
        assert_eq!(metric(&got, "engagementRate"), Some(6.8));
    }

    /// Networks emit different subsets, so a missing metric is normal.
    #[test]
    fn a_metric_this_network_does_not_emit_is_none_not_zero() {
        let got = parse(&sent_payload());
        assert_eq!(metric(&got, "totalTimeWatched"), None);
        assert_eq!(metric(&got, "views"), None);
    }

    #[test]
    fn the_lookup_ignores_case() {
        let got = parse(&sent_payload());
        assert_eq!(metric(&got, "EngagementRate"), Some(6.8));
    }

    /// A queued-but-unsent post has no sentAt, which is what keeps it out of the
    /// sampling schedule until Buffer actually fires it.
    #[test]
    fn a_scheduled_post_has_no_send_time_yet() {
        let got = parse(&json!({ "id": "post-1", "status": "scheduled", "metrics": [] }));
        assert!(!got.is_sent());
        assert_eq!(got.sent_at, None);
        assert!(got.metrics.is_empty());
    }

    #[test]
    fn a_malformed_payload_degrades_rather_than_panicking() {
        let got = parse(&json!({}));
        assert_eq!(got.status, "unknown");
        assert_eq!(got.post_id, "");
        assert!(!got.is_sent());
        // An empty sentAt string is treated as absent, not as a timestamp.
        let blank = parse(&json!({ "id": "x", "status": "sent", "sentAt": "" }));
        assert_eq!(blank.sent_at, None);
    }

    #[test]
    fn a_metric_missing_its_unit_defaults_to_a_count() {
        let got = parse(&json!({
            "id": "p", "status": "sent",
            "metrics": [{ "name": "views", "value": 12.0 }]
        }));
        assert_eq!(got.metrics[0].unit, "count");
        assert_eq!(metric(&got, "views"), Some(12.0));
    }
}
