use super::MetricsCollector;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use prometheus_client::encoding::text::encode;
use std::sync::Arc;

pub fn metrics_handler(metrics: &Arc<MetricsCollector>) -> Response {
    let mut buffer = String::new();
    if encode(&mut buffer, &metrics.registry).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to encode metrics").into_response();
    }
    (
        StatusCode::OK,
        [(
            "content-type",
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )],
        buffer,
    )
        .into_response()
}
