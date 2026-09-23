//! hyper/tokio adapter: accepts connections, converts requests to the
//! framework-independent [`Request`], routes them, and handles graceful
//! shutdown.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

use super::http::{MAX_BODY_BYTES, Request, Response, new_request_id};
use super::routes;
use super::state::State;

/// Slowloris guard: the full request head must arrive within this time.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// The body must arrive within this time.
const BODY_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long shutdown waits for in-flight requests.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Serve until SIGTERM or Ctrl-C, then stop accepting and drain.
pub async fn serve(listener: TcpListener, state: State) {
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown_signal());
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, remote) = match accepted {
                    Ok(x) => x,
                    Err(e) => {
                        // Transient accept errors (EMFILE, ECONNABORTED): keep serving.
                        warn!(error = %e, "accept failed");
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                };
                let _ = stream.set_nodelay(true);
                let state = state.clone();
                let service = service_fn(move |req| {
                    let state = state.clone();
                    async move { Ok::<_, Infallible>(handle(req, state, remote).await) }
                });
                let conn = http1::Builder::new()
                    .timer(TokioTimer::new())
                    .header_read_timeout(HEADER_READ_TIMEOUT)
                    .keep_alive(true)
                    .serve_connection(TokioIo::new(stream), service);
                let conn = graceful.watch(conn);
                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        debug!(error = %e, "connection ended with an error");
                    }
                });
            }
            _ = &mut shutdown => {
                info!("shutdown signal received; draining connections");
                break;
            }
        }
    }
    drop(listener);
    tokio::select! {
        _ = graceful.shutdown() => info!("all connections closed"),
        _ = tokio::time::sleep(DRAIN_TIMEOUT) => warn!("timed out waiting for connections to close"),
    }
}

async fn handle(
    req: hyper::Request<Incoming>,
    state: State,
    remote: SocketAddr,
) -> hyper::Response<Full<Bytes>> {
    let (parts, body) = req.into_parts();
    let declared = parts
        .headers
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|len| len > MAX_BODY_BYTES) {
        return to_hyper(Response::error(413, "payload too large"));
    }
    let body = match tokio::time::timeout(
        BODY_READ_TIMEOUT,
        Limited::new(body, MAX_BODY_BYTES).collect(),
    )
    .await
    {
        Ok(Ok(collected)) => collected.to_bytes(),
        Ok(Err(e)) => {
            return to_hyper(if e.is::<http_body_util::LengthLimitError>() {
                Response::error(413, "payload too large")
            } else {
                Response::error(400, "could not read request body")
            });
        }
        Err(_) => return to_hyper(Response::error(408, "timed out reading the request body")),
    };
    let request_id = parts
        .headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 128 && v.chars().all(|c| c.is_ascii_graphic()))
        .map(String::from)
        .unwrap_or_else(new_request_id);
    let request = Request {
        method: parts.method.as_str().to_ascii_uppercase(),
        path: parts.uri.path().to_string(),
        query: parts.uri.query().unwrap_or("").to_string(),
        headers: parts
            .headers
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect(),
        body,
        remote: Some(remote),
        request_id,
    };
    to_hyper(routes::route(&request, &state))
}

fn to_hyper(resp: Response) -> hyper::Response<Full<Bytes>> {
    let mut builder = hyper::Response::builder().status(resp.status);
    for (k, v) in &resp.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    builder.body(Full::new(resp.body)).unwrap_or_else(|e| {
        warn!(error = %e, "invalid response header; sending 500");
        let mut r = hyper::Response::new(Full::new(Bytes::from_static(
            br#"{"error":"internal error"}"#,
        )));
        *r.status_mut() = hyper::StatusCode::INTERNAL_SERVER_ERROR;
        r
    })
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}
