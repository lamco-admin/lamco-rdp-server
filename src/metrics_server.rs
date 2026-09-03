//! HTTP Metrics Server — Prometheus /metrics + JSON /health
//!
//! **Feature:** `metrics-server`
//! **Status:** Active (v1.5.0+)
//!
//! Lightweight HTTP server on a dedicated thread serving two endpoints:
//! - `GET /metrics` — OpenMetrics text format for Prometheus scraping
//! - `GET /health` — JSON health summary for Cloudflare tunnels, load balancers
//!
//! Uses `tiny-http` (synchronous, no async runtime needed) because metrics
//! endpoints serve infrequent, small responses. Runs on its own thread to
//! avoid burdening the tokio runtime.

use std::{sync::Arc, thread};

use tracing::{debug, info, warn};

use crate::{
    health::{SessionHealthState, SubsystemHealth, snapshot_collector::SnapshotCollector},
    runtime::metrics::MetricsCollector,
};

/// HTTP metrics server handle.
///
/// Dropping this stops the server by closing the `tiny_http::Server`
/// (which causes the accept loop to exit).
pub struct MetricsServer {
    /// Thread handle for the HTTP server
    #[expect(dead_code, reason = "Kept alive to run server background thread")]
    thread_handle: thread::JoinHandle<()>,
}

impl MetricsServer {
    /// Start the metrics server on a dedicated thread.
    ///
    /// Binds to the configured address and serves until the server
    /// is dropped or the process exits.
    pub fn start(
        bind_addr: &str,
        metrics: Arc<MetricsCollector>,
        snapshot_collector: Arc<SnapshotCollector>,
        health_state: tokio::sync::watch::Receiver<SessionHealthState>,
    ) -> Result<Self, std::io::Error> {
        let server = tiny_http::Server::http(bind_addr)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::AddrInUse, e.to_string()))?;

        info!("Metrics server listening on http://{bind_addr}");
        info!("  GET /metrics — Prometheus/OpenMetrics text format");
        info!("  GET /health  — JSON health summary");

        let server = Arc::new(server);

        let thread_handle = {
            let server = Arc::clone(&server);
            thread::Builder::new()
                .name("metrics-http".into())
                .spawn(move || {
                    serve_loop(server, metrics, snapshot_collector, health_state);
                })?
        };

        Ok(Self { thread_handle })
    }
}

fn serve_loop(
    server: Arc<tiny_http::Server>,
    metrics: Arc<MetricsCollector>,
    snapshot_collector: Arc<SnapshotCollector>,
    health_state: tokio::sync::watch::Receiver<SessionHealthState>,
) {
    loop {
        let request = match server.recv() {
            Ok(req) => req,
            Err(e) => {
                // Server was closed (shutdown)
                debug!("Metrics server recv error (likely shutdown): {e}");
                break;
            }
        };

        let url = request.url().to_string();
        debug!("Metrics HTTP {} {}", request.method(), url);

        let (response_body, content_type, status_code) = match url.as_str() {
            "/metrics" => (
                metrics.export_prometheus(),
                "text/plain; version=0.0.4; charset=utf-8",
                200,
            ),
            "/health" => (
                build_health_json(&snapshot_collector, &health_state),
                "application/json",
                200,
            ),
            _ => ("Not Found\n".into(), "text/plain", 404),
        };

        // from_bytes rejects only malformed header names or values, and both
        // arguments here are compile-time constants, so this cannot fail in
        // practice. Serve the body without the header rather than panic on a
        // request thread if it ever does.
        let mut response =
            tiny_http::Response::from_string(&response_body).with_status_code(status_code);
        match tiny_http::Header::from_bytes(b"Content-Type", content_type.as_bytes()) {
            Ok(header) => response = response.with_header(header),
            Err(()) => warn!("Could not build the Content-Type header for {url}"),
        }

        if let Err(e) = request.respond(response) {
            warn!("Failed to respond to {url}: {e}");
        }
    }
    info!("Metrics server stopped");
}

fn build_health_json(
    snapshot_collector: &SnapshotCollector,
    health_state: &tokio::sync::watch::Receiver<SessionHealthState>,
) -> String {
    let health = health_state.borrow();
    let snap = snapshot_collector.snapshot();

    let status = match health.overall {
        crate::health::OverallHealth::Healthy => "healthy",
        crate::health::OverallHealth::Degraded => "degraded",
        crate::health::OverallHealth::Invalid => "unhealthy",
        crate::health::OverallHealth::Recovering => "recovering",
    };

    let subsystem_str = |s: &SubsystemHealth| -> &str {
        match s {
            SubsystemHealth::Healthy => "healthy",
            SubsystemHealth::Degraded(_) => "degraded",
            SubsystemHealth::Failed(_) => "failed",
            SubsystemHealth::NotApplicable => "not_applicable",
        }
    };

    let egfx_version = snap.egfx.negotiated_version.as_deref().unwrap_or("none");

    let body = serde_json::json!({
        "status": status,
        "subsystems": {
            "video": subsystem_str(&health.video),
            "input": subsystem_str(&health.input),
            "clipboard": subsystem_str(&health.clipboard),
            "session": subsystem_str(&health.session),
        },
        "protocol_versions": {
            "egfx": egfx_version,
        },
        "uptime_seconds": snap.uptime.as_secs(),
        "active_connections": snap.metrics.active_connections,
        "version": env!("CARGO_PKG_VERSION"),
        "performance": {
            "fps": snap.fps.current_fps,
            "activity_level": snap.fps.activity_level,
            "latency_mode": snap.latency.mode,
            "total_latency_avg_ms": snap.latency.total_latency_avg_ms,
            "encoder": snap.encoder.as_ref().map(|e| serde_json::json!({
                "backend": e.backend,
                "fps": e.fps,
                "bitrate_kbps": e.bitrate_kbps,
            })),
        },
    });

    body.to_string()
}
