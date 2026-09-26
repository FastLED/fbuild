//! Answer `/health` with `starting` while the daemon initializes
//! (FastLED/fbuild#1480).
//!
//! `main` binds the endpoint before any heavy init (FastLED/fbuild#1010), but
//! the full router only starts serving once the USB catalogue, the root
//! ownership lock and the embedded zccache backend are ready. Before this
//! module, a client probing in that window saw a bound port that never
//! answered, and after 3 × 10 s reported a slow but healthy daemon as failed
//! (FastLED/fbuild#1462).
//!
//! [`StartupGate::open`] duplicates the bound socket: a minimal responder
//! serves every request on the duplicate with
//! `503 {"status": "starting", "phase": ...}`, while the original stays
//! unregistered until [`StartupGate::finish`] stops the responder and hands
//! it to the full router. Both handles share one accept queue, so a
//! connection that arrives during the hand-over waits in the backlog rather
//! than being refused.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long [`StartupGate::finish`] waits for the responder to drain its
/// in-flight connections before aborting it.
const RESPONDER_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Body of every response the startup responder sends.
#[derive(Debug, Serialize)]
pub struct StartingResponse {
    pub status: &'static str,
    pub phase: &'static str,
    pub uptime_seconds: f64,
    pub version: &'static str,
    pub pid: u32,
}

struct Progress {
    phase: Mutex<&'static str>,
    started_at: Instant,
}

impl Progress {
    fn response(&self) -> StartingResponse {
        StartingResponse {
            status: fbuild_core::daemon_health::STARTING_STATUS,
            phase: *self.phase.lock().unwrap_or_else(|e| e.into_inner()),
            uptime_seconds: self.started_at.elapsed().as_secs_f64(),
            version: env!("CARGO_PKG_VERSION"),
            pid: std::process::id(),
        }
    }
}

async fn starting(State(progress): State<Arc<Progress>>) -> (StatusCode, Json<StartingResponse>) {
    (StatusCode::SERVICE_UNAVAILABLE, Json(progress.response()))
}

struct Responder {
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

/// The bound endpoint, held while the daemon initializes.
pub struct StartupGate {
    listener: std::net::TcpListener,
    progress: Arc<Progress>,
    responder: Option<Responder>,
}

impl StartupGate {
    /// Start answering `starting` on `listener`.
    ///
    /// If the socket cannot be duplicated (e.g. the process is out of file
    /// descriptors) the gate still holds the endpoint and startup proceeds as
    /// before, just without the `starting` answers.
    pub fn open(listener: tokio::net::TcpListener, phase: &'static str) -> std::io::Result<Self> {
        let listener = listener.into_std()?;
        let progress = Arc::new(Progress {
            phase: Mutex::new(phase),
            started_at: Instant::now(),
        });
        let responder = match listener
            .try_clone()
            .and_then(tokio::net::TcpListener::from_std)
        {
            Ok(duplicate) => Some(spawn_responder(duplicate, Arc::clone(&progress))),
            Err(error) => {
                tracing::warn!(
                    "cannot duplicate the daemon listener ({error}); /health stays silent until startup completes"
                );
                None
            }
        };
        Ok(Self {
            listener,
            progress,
            responder,
        })
    }

    /// Record the initialization phase now running; `/health` reports it.
    pub fn set_phase(&self, phase: &'static str) {
        tracing::info!("daemon startup phase: {phase}");
        *self
            .progress
            .phase
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = phase;
    }

    /// Stop the responder and return the endpoint for the full router.
    pub async fn finish(self) -> std::io::Result<tokio::net::TcpListener> {
        if let Some(Responder { stop, mut task }) = self.responder {
            let _ = stop.send(());
            if tokio::time::timeout(RESPONDER_DRAIN_TIMEOUT, &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
        tracing::info!(
            "daemon startup complete in {:.1}s",
            self.progress.started_at.elapsed().as_secs_f64()
        );
        tokio::net::TcpListener::from_std(self.listener)
    }
}

fn spawn_responder(listener: tokio::net::TcpListener, progress: Arc<Progress>) -> Responder {
    let app = Router::new()
        .route("/health", get(starting))
        .fallback(starting)
        .with_state(progress);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
            })
            .await
        {
            tracing::warn!("daemon startup responder failed: {error}");
        }
    });
    Responder { stop, task }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbuild_core::daemon_health::{DaemonHealth, probe};
    use fbuild_core::time::SHORT_HTTP_TIMEOUT;

    async fn bound() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/health", listener.local_addr().unwrap());
        (listener, url)
    }

    #[tokio::test]
    async fn health_reports_starting_with_the_current_phase() {
        let (listener, url) = bound().await;
        let gate = StartupGate::open(listener, "usb_overlay").unwrap();
        let client = fbuild_core::http::client();

        assert_eq!(
            probe(client, &url, SHORT_HTTP_TIMEOUT).await,
            DaemonHealth::Starting {
                phase: Some("usb_overlay".to_string())
            }
        );
        gate.set_phase("compile_backend");
        assert_eq!(
            probe(client, &url, SHORT_HTTP_TIMEOUT).await,
            DaemonHealth::Starting {
                phase: Some("compile_backend".to_string())
            }
        );

        let body: serde_json::Value = client.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["pid"], std::process::id());
        drop(gate);
    }

    #[tokio::test]
    async fn every_route_reports_starting() {
        let (listener, url) = bound().await;
        let _gate = StartupGate::open(listener, "compile_backend").unwrap();
        let build_url = url.replace("/health", "/api/build");
        let resp = fbuild_core::http::client()
            .post(&build_url)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn finish_hands_the_endpoint_to_the_full_router() {
        let (listener, url) = bound().await;
        let gate = StartupGate::open(listener, "compile_backend").unwrap();
        let client = fbuild_core::http::client();
        assert!(matches!(
            probe(client, &url, SHORT_HTTP_TIMEOUT).await,
            DaemonHealth::Starting { .. }
        ));

        let listener = gate.finish().await.unwrap();
        let app = Router::new().route("/health", get(|| async { "ok" }));
        tokio::spawn(async move { axum::serve(listener, app).await });

        assert_eq!(
            probe(client, &url, SHORT_HTTP_TIMEOUT).await,
            DaemonHealth::Healthy
        );
    }
}
