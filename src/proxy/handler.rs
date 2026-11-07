use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::header::{ACCEPT, HOST, RETRY_AFTER, UPGRADE};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::router::extract_subdomain;
use crate::ringbuf::LogStream;
use crate::supervisor::{ProcessStatus, SupervisedService, Supervisor};

/// Boxed body type used for all proxy responses
pub type ProxyBody = BoxBody<Bytes, hyper::Error>;

/// Returns an empty boxed body
pub fn empty_body() -> ProxyBody {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

/// Returns a full boxed body with given byte contents
pub fn full_body(bytes: impl Into<Bytes>) -> ProxyBody {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed()
}

/// Core request handler for the Hyper 1.x reverse proxy
pub async fn handle_request(
    req: Request<hyper::body::Incoming>,
    client_addr: SocketAddr,
    supervisor: Arc<Supervisor>,
    client: hyper_util::client::legacy::Client<
        hyper_util::client::legacy::connect::HttpConnector,
        ProxyBody,
    >,
    proxy_port: u16,
) -> Result<Response<ProxyBody>, hyper::Error> {
    let host_str = req
        .headers()
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();

    let subdomain_opt = extract_subdomain(&host_str);
    let path = req.uri().path();
    let is_admin_path = path == "/_spinmux" || path == "/_status" || path == "/_overview";

    // 1. Root domain routing (localhost:<port> or 127.0.0.1:<port> with no subdomain)
    if subdomain_opt.is_none() {
        if is_admin_path {
            return Ok(build_overview_page(&supervisor, proxy_port, req.headers()).await);
        }

        // Check for default service (web/frontend)
        if let Some(default_svc) = supervisor.get_default_service().await {
            return forward_to_service(
                req,
                client_addr,
                &host_str,
                default_svc,
                client,
                proxy_port,
            )
            .await;
        }

        // No default service registered: serve overview dashboard
        return Ok(build_overview_page(&supervisor, proxy_port, req.headers()).await);
    }

    let subdomain = subdomain_opt.unwrap();

    // 2. Lookup service by subdomain
    let service = match supervisor.get_service_by_subdomain(&subdomain).await {
        Some(svc) => svc,
        None => {
            let all = supervisor.all_services().await;
            let available: Vec<String> =
                all.iter().map(|s| s.definition.subdomain.clone()).collect();
            return Ok(build_503_service_unavailable(
                &format!("Service with virtual subdomain '{subdomain}' is not registered."),
                Some(&subdomain),
                false,
                &available,
                req.headers(),
            ));
        }
    };

    // 3. Inspect service lifecycle status
    let status = service.status();
    match status {
        ProcessStatus::Starting => {
            return Ok(build_503_service_unavailable(
                &format!("Service '{}' is starting up...", service.definition.name),
                Some(&subdomain),
                true, // Retry-After: 1
                &[],
                req.headers(),
            ));
        }
        ProcessStatus::Crashing(backoff) => {
            return Ok(build_503_service_unavailable(
                &format!(
                    "Service '{}' crashed and is waiting out restart backoff ({}ms).",
                    service.definition.name, backoff
                ),
                Some(&subdomain),
                true,
                &[],
                req.headers(),
            ));
        }
        ProcessStatus::CrashLoopBackOff => {
            return Ok(build_503_service_unavailable(
                &format!(
                    "Service '{}' has entered CrashLoopBackOff after repeated failures.",
                    service.definition.name
                ),
                Some(&subdomain),
                false,
                &[],
                req.headers(),
            ));
        }
        ProcessStatus::Stopped(code) => {
            let code_str = match code {
                Some(c) => format!("exit code: {c}"),
                None => "not started".to_string(),
            };
            return Ok(build_503_service_unavailable(
                &format!(
                    "Service '{}' is currently stopped ({code_str}).",
                    service.definition.name
                ),
                Some(&subdomain),
                false,
                &[],
                req.headers(),
            ));
        }
        ProcessStatus::Running(_) => {
            // Process is actively executing; proceed to forward
        }
    }

    // 4. Forward traffic to the running upstream service
    forward_to_service(req, client_addr, &host_str, service, client, proxy_port).await
}

/// Forwards an HTTP or WebSocket request to the target service
async fn forward_to_service(
    req: Request<hyper::body::Incoming>,
    client_addr: SocketAddr,
    host_str: &str,
    service: Arc<SupervisedService>,
    client: hyper_util::client::legacy::Client<
        hyper_util::client::legacy::connect::HttpConnector,
        ProxyBody,
    >,
    _proxy_port: u16,
) -> Result<Response<ProxyBody>, hyper::Error> {
    let target_port = service.assigned_port;

    // Check for WebSocket Upgrade
    let is_websocket = req
        .headers()
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    if is_websocket {
        return handle_websocket_upgrade(req, client_addr, host_str, service, target_port).await;
    }

    // Standard HTTP Request Forwarding
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let target_uri_str = format!("http://127.0.0.1:{}{}", target_port, path_and_query);
    let target_uri: hyper::Uri = match target_uri_str.parse() {
        Ok(uri) => uri,
        Err(err) => {
            let error_res = Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(hyper::header::CONTENT_TYPE, "application/json")
                .body(full_body(format!(r#"{{"error": "Invalid URI: {err}"}}"#)))
                .unwrap();
            return Ok(error_res);
        }
    };

    let (parts, incoming_body) = req.into_parts();
    let mut upstream_req = Request::new(incoming_body.boxed());
    *upstream_req.method_mut() = parts.method;
    *upstream_req.uri_mut() = target_uri;
    *upstream_req.version_mut() = parts.version;

    // Copy original headers
    for (name, value) in &parts.headers {
        upstream_req
            .headers_mut()
            .insert(name.clone(), value.clone());
    }

    // Inject standard proxy headers
    let client_ip = client_addr.ip().to_string();
    if let Some(existing_for) = upstream_req.headers_mut().get_mut("x-forwarded-for") {
        let appended = format!("{}, {}", existing_for.to_str().unwrap_or(""), client_ip);
        if let Ok(val) = hyper::header::HeaderValue::from_str(&appended) {
            *existing_for = val;
        }
    } else if let Ok(val) = hyper::header::HeaderValue::from_str(&client_ip) {
        upstream_req.headers_mut().insert("x-forwarded-for", val);
    }

    if !upstream_req.headers().contains_key("x-forwarded-host") {
        if let Ok(val) = hyper::header::HeaderValue::from_str(host_str) {
            upstream_req.headers_mut().insert("x-forwarded-host", val);
        }
    }

    if !upstream_req.headers().contains_key("x-forwarded-proto") {
        upstream_req.headers_mut().insert(
            "x-forwarded-proto",
            hyper::header::HeaderValue::from_static("http"),
        );
    }

    // Execute upstream request with unbuffered streaming body
    match client.request(upstream_req).await {
        Ok(upstream_res) => {
            let (res_parts, res_body) = upstream_res.into_parts();
            let mut response = Response::new(res_body.boxed());
            *response.status_mut() = res_parts.status;
            *response.version_mut() = res_parts.version;
            *response.headers_mut() = res_parts.headers;
            Ok(response)
        }
        Err(err) => {
            // Connection failed: 502 Bad Gateway with diagnostic logs
            Ok(build_502_bad_gateway(
                &service,
                target_port,
                &err,
                &parts.headers,
            ))
        }
    }
}

/// Handles WebSocket upgrade handshake forwarding and bidirectional streaming
async fn handle_websocket_upgrade(
    mut req: Request<hyper::body::Incoming>,
    client_addr: SocketAddr,
    host_str: &str,
    service: Arc<SupervisedService>,
    target_port: u16,
) -> Result<Response<ProxyBody>, hyper::Error> {
    let target_addr = SocketAddr::from(([127, 0, 0, 1], target_port));

    // 1. Connect to upstream TCP backend with timeout
    let mut backend_stream = match tokio::time::timeout(
        Duration::from_millis(3000),
        tokio::net::TcpStream::connect(target_addr),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            return Ok(build_502_bad_gateway(
                &service,
                target_port,
                &err,
                req.headers(),
            ));
        }
        Err(_) => {
            let timeout_err = std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Connection to upstream service timed out",
            );
            return Ok(build_502_bad_gateway(
                &service,
                target_port,
                &timeout_err,
                req.headers(),
            ));
        }
    };

    // 2. Forward WebSocket handshake HTTP request to upstream
    let method = req.method().as_str();
    let path = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let version = match req.version() {
        hyper::Version::HTTP_10 => "HTTP/1.0",
        hyper::Version::HTTP_11 => "HTTP/1.1",
        _ => "HTTP/1.1",
    };

    let mut header_buf = Vec::with_capacity(1024);
    header_buf.extend_from_slice(format!("{method} {path} {version}\r\n").as_bytes());

    for (name, value) in req.headers() {
        header_buf.extend_from_slice(name.as_str().as_bytes());
        header_buf.extend_from_slice(b": ");
        header_buf.extend_from_slice(value.as_bytes());
        header_buf.extend_from_slice(b"\r\n");
    }

    let client_ip = client_addr.ip().to_string();
    if !req.headers().contains_key("x-forwarded-for") {
        header_buf.extend_from_slice(format!("x-forwarded-for: {client_ip}\r\n").as_bytes());
    }
    if !req.headers().contains_key("x-forwarded-host") {
        header_buf.extend_from_slice(format!("x-forwarded-host: {host_str}\r\n").as_bytes());
    }
    if !req.headers().contains_key("x-forwarded-proto") {
        header_buf.extend_from_slice(b"x-forwarded-proto: http\r\n");
    }

    header_buf.extend_from_slice(b"\r\n");

    if let Err(err) = backend_stream.write_all(&header_buf).await {
        return Ok(build_502_bad_gateway(
            &service,
            target_port,
            &err,
            req.headers(),
        ));
    }

    // 3. Read upstream handshake response headers
    let mut resp_buf = Vec::with_capacity(2048);
    let mut temp = [0u8; 1024];
    let header_end;

    loop {
        let n = match backend_stream.read(&mut temp).await {
            Ok(n) if n > 0 => n,
            Ok(_) => {
                let err = std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Upstream server closed connection during handshake",
                );
                return Ok(build_502_bad_gateway(
                    &service,
                    target_port,
                    &err,
                    req.headers(),
                ));
            }
            Err(err) => {
                return Ok(build_502_bad_gateway(
                    &service,
                    target_port,
                    &err,
                    req.headers(),
                ));
            }
        };

        resp_buf.extend_from_slice(&temp[..n]);
        if let Some(pos) = resp_buf.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
        if resp_buf.len() > 64 * 1024 {
            let err = std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Upstream WebSocket response headers exceeded 64KB",
            );
            return Ok(build_502_bad_gateway(
                &service,
                target_port,
                &err,
                req.headers(),
            ));
        }
    }

    let header_bytes = &resp_buf[..header_end];
    let leftover_bytes = resp_buf[header_end..].to_vec();

    // Parse status line and headers from upstream
    let header_text = String::from_utf8_lossy(header_bytes);
    let mut lines = header_text.lines();
    let status_line = lines.next().unwrap_or("");
    let status_code = parse_status_code(status_line).unwrap_or(StatusCode::SWITCHING_PROTOCOLS);

    let mut response_builder = Response::builder().status(status_code);

    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            let v = v.trim();
            if let (Ok(name), Ok(val)) = (
                hyper::header::HeaderName::from_bytes(k.as_bytes()),
                hyper::header::HeaderValue::from_str(v),
            ) {
                response_builder = response_builder.header(name, val);
            }
        }
    }

    if status_code == StatusCode::SWITCHING_PROTOCOLS {
        let response = response_builder.body(empty_body()).unwrap();

        // 4. Hijack client connection and stream full-duplex with copy_bidirectional
        tokio::spawn(async move {
            match hyper::upgrade::on(&mut req).await {
                Ok(upgraded_client) => {
                    let mut client_io = TokioIo::new(upgraded_client);
                    if !leftover_bytes.is_empty() {
                        if let Err(e) = client_io.write_all(&leftover_bytes).await {
                            tracing::warn!("Failed writing initial WebSocket bytes to client: {e}");
                            return;
                        }
                    }
                    let _ =
                        tokio::io::copy_bidirectional(&mut client_io, &mut backend_stream).await;
                }
                Err(err) => {
                    tracing::error!("WebSocket upgrade error on client side: {err}");
                }
            }
        });

        Ok(response)
    } else {
        // Upstream returned an error or non-101 status
        let body = if !leftover_bytes.is_empty() {
            full_body(leftover_bytes)
        } else {
            empty_body()
        };
        Ok(response_builder.body(body).unwrap())
    }
}

/// Parses the 3-digit status code from an HTTP status line (e.g. "HTTP/1.1 101 Switching Protocols")
fn parse_status_code(line: &str) -> Option<StatusCode> {
    let mut parts = line.split_whitespace();
    parts.next()?; // Skip HTTP version
    let code_str = parts.next()?;
    let code_num: u16 = code_str.parse().ok()?;
    StatusCode::from_u16(code_num).ok()
}

/// Constructs a rich diagnostic HTTP 502 Bad Gateway response enriched with ring buffer stderr
fn build_502_bad_gateway(
    service: &SupervisedService,
    target_port: u16,
    err: &dyn std::fmt::Display,
    request_headers: &hyper::HeaderMap,
) -> Response<ProxyBody> {
    // Collect recent stderr lines from ring buffer
    let recent_stderr: Vec<String> = service
        .buffer
        .snapshot(50)
        .into_iter()
        .filter(|e| e.stream == LogStream::Stderr)
        .map(|e| e.plain_text())
        .rev()
        .take(10)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let display_logs = if !recent_stderr.is_empty() {
        recent_stderr
    } else {
        service
            .buffer
            .snapshot(10)
            .into_iter()
            .map(|e| format!("[{}] {}", e.stream, e.plain_text()))
            .collect()
    };

    let wants_html = request_headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/html"))
        .unwrap_or(false);

    let status_str = format!("{}", service.status());

    if wants_html {
        let stderr_html = if display_logs.is_empty() {
            "<em>No logs recorded yet.</em>".to_string()
        } else {
            display_logs
                .iter()
                .map(|line| html_escape(line))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>502 Bad Gateway - spinmux</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace; background: #121214; color: #E0E0E0; margin: 0; padding: 2rem; }}
    h1 {{ color: #FF5252; margin-top: 0; }}
    .badge {{ background: #26262B; padding: 0.2rem 0.6rem; border-radius: 4px; color: #00E5FF; font-weight: bold; }}
    pre {{ background: #1A1A1E; border: 1px solid #333; border-radius: 6px; padding: 1rem; overflow-x: auto; color: #FF8A80; font-size: 0.9rem; line-height: 1.4; }}
    .info {{ background: #1E1E24; padding: 1rem; border-radius: 6px; margin: 1rem 0; }}
    .footer {{ margin-top: 2rem; color: #777; font-size: 0.85rem; border-top: 1px solid #333; padding-top: 0.5rem; }}
  </style>
</head>
<body>
  <h1>502 Bad Gateway</h1>
  <div class="info">
    <p>The reverse proxy could not connect to service <span class="badge">{}</span> on port <code>{}</code>.</p>
    <p><strong>Process Status:</strong> {}</p>
    <p><strong>Connection Error:</strong> {}</p>
  </div>
  <h3>Recent Stderr Output (Ring Buffer):</h3>
  <pre>{}</pre>
  <div class="footer">spinmux ephemeral virtual localhost reverse proxy</div>
</body>
</html>"#,
            service.definition.name, target_port, status_str, err, stderr_html
        );

        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(full_body(html))
            .unwrap()
    } else {
        let payload = serde_json::json!({
            "error": "Bad Gateway",
            "status": 502,
            "service": service.definition.name,
            "subdomain": service.definition.subdomain,
            "target_port": target_port,
            "process_status": status_str,
            "connection_error": format!("{err}"),
            "recent_stderr": display_logs,
        });

        Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(full_body(payload.to_string()))
            .unwrap()
    }
}

/// Constructs a diagnostic HTTP 503 Service Unavailable response
fn build_503_service_unavailable(
    reason: &str,
    subdomain: Option<&str>,
    retry_after: bool,
    available_subdomains: &[String],
    request_headers: &hyper::HeaderMap,
) -> Response<ProxyBody> {
    let wants_html = request_headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/html"))
        .unwrap_or(false);

    let mut builder = Response::builder().status(StatusCode::SERVICE_UNAVAILABLE);

    if retry_after {
        builder = builder.header(RETRY_AFTER, "1");
    }

    if wants_html {
        let refresh_meta = if retry_after {
            r#"<meta http-equiv="refresh" content="1">"#
        } else {
            ""
        };

        let available_html = if available_subdomains.is_empty() {
            String::new()
        } else {
            let list = available_subdomains
                .iter()
                .map(|s| format!("<li><code>{s}.localhost</code></li>"))
                .collect::<Vec<_>>()
                .join("\n");
            format!("<h3>Available Virtual Subdomains:</h3><ul>{list}</ul>")
        };

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>503 Service Unavailable - spinmux</title>
  {refresh_meta}
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace; background: #121214; color: #E0E0E0; margin: 0; padding: 2rem; }}
    h1 {{ color: #FFB300; margin-top: 0; }}
    .badge {{ background: #26262B; padding: 0.2rem 0.6rem; border-radius: 4px; color: #00E5FF; font-weight: bold; }}
    .footer {{ margin-top: 2rem; color: #777; font-size: 0.85rem; border-top: 1px solid #333; padding-top: 0.5rem; }}
  </style>
</head>
<body>
  <h1>503 Service Unavailable</h1>
  <p>{}</p>
  {}
  <div class="footer">spinmux ephemeral virtual localhost reverse proxy</div>
</body>
</html>"#,
            reason, available_html
        );

        builder
            .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(full_body(html))
            .unwrap()
    } else {
        let payload = serde_json::json!({
            "error": "Service Unavailable",
            "status": 503,
            "message": reason,
            "subdomain": subdomain,
            "retry_after": if retry_after { Some(1) } else { None },
            "available_subdomains": available_subdomains,
        });

        builder
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(full_body(payload.to_string()))
            .unwrap()
    }
}

/// Constructs the Spinmux Cockpit Overview dashboard page (200 OK)
async fn build_overview_page(
    supervisor: &Supervisor,
    proxy_port: u16,
    request_headers: &hyper::HeaderMap,
) -> Response<ProxyBody> {
    let services = supervisor.all_services().await;

    let wants_html = request_headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/html"))
        .unwrap_or(false);

    if wants_html {
        let mut rows = String::new();
        for svc in &services {
            let status = svc.status();
            let status_badge = match status {
                ProcessStatus::Running(pid) => {
                    format!(r#"<span style="color:#00E5FF">● Running (PID: {pid})</span>"#)
                }
                ProcessStatus::Starting => {
                    r#"<span style="color:#FFB300">◌ Starting</span>"#.to_string()
                }
                ProcessStatus::Crashing(_) => {
                    r#"<span style="color:#FF5252">✕ Crashing</span>"#.to_string()
                }
                ProcessStatus::CrashLoopBackOff => {
                    r#"<span style="color:#FF1744">✕ CrashLoopBackOff</span>"#.to_string()
                }
                ProcessStatus::Stopped(_) => {
                    r#"<span style="color:#777">✕ Stopped</span>"#.to_string()
                }
            };

            let link = format!(
                "http://{}.localhost:{}",
                svc.definition.subdomain, proxy_port
            );
            rows.push_str(&format!(
                r#"<tr>
  <td><strong>{}</strong></td>
  <td><a href="{}" style="color:#00E5FF;text-decoration:none;">{}</a></td>
  <td>{}</td>
  <td><code>{}</code></td>
  <td>{}</td>
</tr>"#,
                svc.definition.name,
                link,
                link,
                svc.assigned_port,
                svc.definition.command,
                status_badge
            ));
        }

        let html = format!(
            r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>spinmux - Cockpit Overview</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace; background: #121214; color: #E0E0E0; margin: 0; padding: 2rem; }}
    h1 {{ color: #00E5FF; margin-top: 0; }}
    .subtitle {{ color: #BD00FF; margin-bottom: 2rem; font-weight: bold; }}
    table {{ width: 100%; border-collapse: collapse; margin-top: 1rem; background: #1A1A1E; border-radius: 6px; overflow: hidden; }}
    th, td {{ padding: 0.8rem 1rem; text-align: left; border-bottom: 1px solid #2B2B30; }}
    th {{ background: #26262B; color: #A0A0A0; font-size: 0.85rem; text-transform: uppercase; }}
    tr:hover {{ background: #222227; }}
    code {{ background: #2B2B30; padding: 0.2rem 0.4rem; border-radius: 4px; font-size: 0.85rem; }}
    .footer {{ margin-top: 2rem; color: #777; font-size: 0.85rem; border-top: 1px solid #333; padding-top: 0.5rem; }}
  </style>
</head>
<body>
  <h1>⚡ spinmux</h1>
  <div class="subtitle">Autonomous Multi-Service Development Cockpit</div>
  <p>Reverse Proxy running on <code>http://localhost:{}</code></p>
  <table>
    <thead>
      <tr>
        <th>Service</th>
        <th>Virtual Host</th>
        <th>Local Port</th>
        <th>Command</th>
        <th>Status</th>
      </tr>
    </thead>
    <tbody>
      {}
    </tbody>
  </table>
  <div class="footer">spinmux ephemeral virtual localhost reverse proxy</div>
</body>
</html>"#,
            proxy_port, rows
        );

        Response::builder()
            .status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(full_body(html))
            .unwrap()
    } else {
        let service_records: Vec<_> = services
            .iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.definition.name,
                    "subdomain": s.definition.subdomain,
                    "url": format!("http://{}.localhost:{}", s.definition.subdomain, proxy_port),
                    "local_port": s.assigned_port,
                    "status": format!("{}", s.status()),
                    "is_running": s.status().is_running(),
                })
            })
            .collect();

        let payload = serde_json::json!({
            "spinmux": true,
            "proxy_port": proxy_port,
            "services": service_records,
        });

        Response::builder()
            .status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(full_body(payload.to_string()))
            .unwrap()
    }
}

/// Escapes HTML special characters for safe output formatting
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
