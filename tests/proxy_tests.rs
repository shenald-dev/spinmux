mod common;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use common::MockHttpServer;
use http_body_util::BodyExt;
use hyper::header::{ACCEPT, HOST};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioExecutor;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use spinmux::discovery::{ServiceDefinition, ServiceSource};
use spinmux::proxy::{full_body, ProxyBody, ProxyServer};
use spinmux::ringbuf::{LogEntry, LogStream};
use spinmux::supervisor::{ProcessStatus, Supervisor};

fn create_mock_service_def(name: &str, subdomain: &str, port: u16) -> ServiceDefinition {
    ServiceDefinition {
        id: name.to_string(),
        name: name.to_string(),
        command: "mock_cmd".to_string(),
        args: vec![],
        working_dir: PathBuf::from("."),
        env: HashMap::new(),
        source: ServiceSource::Procfile {
            path: PathBuf::from("Procfile"),
            variant: "dev".to_string(),
        },
        target_port: Some(port),
        is_http: true,
        subdomain: subdomain.to_string(),
    }
}

fn build_test_client(
) -> hyper_util::client::legacy::Client<hyper_util::client::legacy::connect::HttpConnector, ProxyBody>
{
    hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        .build(hyper_util::client::legacy::connect::HttpConnector::new())
}

#[tokio::test]
async fn test_route_subdomain_frontend() {
    let mock = MockHttpServer::start("frontend").await;
    let mock_port = mock.port();

    let supervisor = Arc::new(Supervisor::new().unwrap());
    let svc_def = create_mock_service_def("frontend_app", "frontend", mock_port);
    let supervised = supervisor
        .add_service(svc_def, Some(mock_port))
        .await
        .unwrap();
    let _ = supervised.status_tx.send(ProcessStatus::Running(1001));

    let proxy = ProxyServer::spawn(0, supervisor).await.unwrap();
    let proxy_port = proxy.port();

    let client = build_test_client();
    let host_header = format!("frontend.localhost:{proxy_port}");
    let req = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/health"))
        .header(HOST, &host_header)
        .body(full_body(""))
        .unwrap();

    let res = client.request(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let echo_fwd_host = res.headers().get("x-echo-forwarded-host").unwrap();
    assert_eq!(echo_fwd_host.to_str().unwrap(), &host_header);

    let echo_fwd_proto = res.headers().get("x-echo-forwarded-proto").unwrap();
    assert_eq!(echo_fwd_proto.to_str().unwrap(), "http");

    let body = res.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body);
    assert!(body_str.contains("Hello from mock service 'frontend'"));

    mock.shutdown().await;
    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_route_multiple_distinct_subdomains() {
    let mock_api = MockHttpServer::start("api").await;
    let mock_worker = MockHttpServer::start("worker").await;

    let supervisor = Arc::new(Supervisor::new().unwrap());

    let def_api = create_mock_service_def("api_service", "api", mock_api.port());
    let svc_api = supervisor
        .add_service(def_api, Some(mock_api.port()))
        .await
        .unwrap();
    let _ = svc_api.status_tx.send(ProcessStatus::Running(2001));

    let def_worker = create_mock_service_def("worker_service", "worker", mock_worker.port());
    let svc_worker = supervisor
        .add_service(def_worker, Some(mock_worker.port()))
        .await
        .unwrap();
    let _ = svc_worker.status_tx.send(ProcessStatus::Running(2002));

    let proxy = ProxyServer::spawn(0, supervisor).await.unwrap();
    let proxy_port = proxy.port();
    let client = build_test_client();

    // 1. Query api.localhost
    let req_api = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/hello"))
        .header(HOST, format!("api.localhost:{proxy_port}"))
        .body(full_body(""))
        .unwrap();
    let res_api = client.request(req_api).await.unwrap();
    assert_eq!(res_api.status(), StatusCode::OK);
    let body_api = res_api.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body_api).contains("Hello from mock service 'api'"));

    // 2. Query worker.localhost
    let req_worker = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/hello"))
        .header(HOST, format!("worker.localhost:{proxy_port}"))
        .body(full_body(""))
        .unwrap();
    let res_worker = client.request(req_worker).await.unwrap();
    assert_eq!(res_worker.status(), StatusCode::OK);
    let body_worker = res_worker.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body_worker).contains("Hello from mock service 'worker'"));

    // 3. Query case-insensitive API.LOCALHOST
    let req_case = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/hello"))
        .header(HOST, format!("API.Localhost:{proxy_port}"))
        .body(full_body(""))
        .unwrap();
    let res_case = client.request(req_case).await.unwrap();
    assert_eq!(res_case.status(), StatusCode::OK);

    // 4. Query nip.io format
    let req_nipio = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/hello"))
        .header(HOST, format!("api.127.0.0.1.nip.io:{proxy_port}"))
        .body(full_body(""))
        .unwrap();
    let res_nipio = client.request(req_nipio).await.unwrap();
    assert_eq!(res_nipio.status(), StatusCode::OK);

    mock_api.shutdown().await;
    mock_worker.shutdown().await;
    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_transparent_streaming_large_payload_and_sse() {
    let mock = MockHttpServer::start("streamer").await;
    let mock_port = mock.port();

    let supervisor = Arc::new(Supervisor::new().unwrap());
    let svc_def = create_mock_service_def("stream_app", "stream", mock_port);
    let supervised = supervisor
        .add_service(svc_def, Some(mock_port))
        .await
        .unwrap();
    let _ = supervised.status_tx.send(ProcessStatus::Running(3001));

    let proxy = ProxyServer::spawn(0, supervisor).await.unwrap();
    let proxy_port = proxy.port();
    let client = build_test_client();

    // 1. Large 1MB streaming download (GET /stream/large)
    let req_download = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/stream/large"))
        .header(HOST, format!("stream.localhost:{proxy_port}"))
        .body(full_body(""))
        .unwrap();

    let res_download = client.request(req_download).await.unwrap();
    assert_eq!(res_download.status(), StatusCode::OK);

    let mut body_stream = res_download.into_body();
    let mut total_received = 0usize;
    while let Some(chunk_res) = body_stream.frame().await {
        let frame = chunk_res.unwrap();
        if let Some(data) = frame.data_ref() {
            total_received += data.len();
        }
    }
    assert_eq!(total_received, 1024 * 1024, "Must receive full 1MB stream");

    // 2. Large streaming upload & echo (POST /echo)
    let payload = vec![b'Z'; 512 * 1024]; // 512 KB
    let req_upload = Request::builder()
        .method(Method::POST)
        .uri(format!("http://127.0.0.1:{proxy_port}/echo"))
        .header(HOST, format!("stream.localhost:{proxy_port}"))
        .body(full_body(payload.clone()))
        .unwrap();

    let res_upload = client.request(req_upload).await.unwrap();
    assert_eq!(res_upload.status(), StatusCode::OK);
    let echoed_bytes = res_upload.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(echoed_bytes.len(), payload.len());
    assert_eq!(&echoed_bytes[..], &payload[..]);

    // 3. Server-Sent Events (GET /sse)
    let req_sse = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/sse"))
        .header(HOST, format!("stream.localhost:{proxy_port}"))
        .body(full_body(""))
        .unwrap();

    let res_sse = client.request(req_sse).await.unwrap();
    assert_eq!(res_sse.status(), StatusCode::OK);
    let content_type = res_sse
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.contains("text/event-stream"));

    let sse_bytes = res_sse.into_body().collect().await.unwrap().to_bytes();
    let sse_str = String::from_utf8_lossy(&sse_bytes);
    assert!(sse_str.contains("data: event 1"));
    assert!(sse_str.contains("data: event 2"));
    assert!(sse_str.contains("data: event 3"));

    mock.shutdown().await;
    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_502_bad_gateway_with_stderr_logs() {
    let supervisor = Arc::new(Supervisor::new().unwrap());

    // Allocate an unused port by binding and immediately closing a listener
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = listener.local_addr().unwrap().port();
    drop(listener);

    let svc_def = create_mock_service_def("crash_service", "crashed", closed_port);
    let supervised = supervisor
        .add_service(svc_def, Some(closed_port))
        .await
        .unwrap();
    let _ = supervised.status_tx.send(ProcessStatus::Running(4001));

    // Inject diagnostic stderr telemetry into ring buffer
    supervised.buffer.push(LogEntry::new(
        "crash_service",
        LogStream::Stderr,
        "FATAL: Database connection refused on localhost:5432",
    ));
    supervised.buffer.push(LogEntry::new(
        "crash_service",
        LogStream::Stderr,
        "Panicked at src/main.rs:42: connection timeout after 5000ms",
    ));

    let proxy = ProxyServer::spawn(0, supervisor).await.unwrap();
    let proxy_port = proxy.port();
    let client = build_test_client();

    // 1. JSON error response
    let req_json = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/api/users"))
        .header(HOST, format!("crashed.localhost:{proxy_port}"))
        .header(ACCEPT, "application/json")
        .body(full_body(""))
        .unwrap();

    let res_json = client.request(req_json).await.unwrap();
    assert_eq!(res_json.status(), StatusCode::BAD_GATEWAY);

    let body_json = res_json.into_body().collect().await.unwrap().to_bytes();
    let body_str = String::from_utf8_lossy(&body_json);
    assert!(
        body_str.contains("Database connection refused on localhost:5432"),
        "502 JSON body must include recent stderr output: {body_str}"
    );
    assert!(
        body_str.contains("Panicked at src/main.rs:42"),
        "502 JSON body must include second stderr line: {body_str}"
    );

    // 2. HTML error response
    let req_html = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/api/users"))
        .header(HOST, format!("crashed.localhost:{proxy_port}"))
        .header(ACCEPT, "text/html,application/xhtml+xml")
        .body(full_body(""))
        .unwrap();

    let res_html = client.request(req_html).await.unwrap();
    assert_eq!(res_html.status(), StatusCode::BAD_GATEWAY);
    let html_content_type = res_html
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(html_content_type.contains("text/html"));

    let body_html = res_html.into_body().collect().await.unwrap().to_bytes();
    let html_str = String::from_utf8_lossy(&body_html);
    assert!(html_str.contains("502 Bad Gateway"));
    assert!(html_str.contains("Database connection refused on localhost:5432"));
    assert!(html_str.contains("<pre>"));

    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_503_service_unavailable() {
    let supervisor = Arc::new(Supervisor::new().unwrap());

    // 1. Stopped service
    let def_stopped = create_mock_service_def("stopped_app", "stopped", 41001);
    let svc_stopped = supervisor
        .add_service(def_stopped, Some(41001))
        .await
        .unwrap();
    let _ = svc_stopped
        .status_tx
        .send(ProcessStatus::Stopped(Some(137)));

    // 2. Starting service
    let def_starting = create_mock_service_def("starting_app", "starting", 41002);
    let svc_starting = supervisor
        .add_service(def_starting, Some(41002))
        .await
        .unwrap();
    let _ = svc_starting.status_tx.send(ProcessStatus::Starting);

    let proxy = ProxyServer::spawn(0, supervisor).await.unwrap();
    let proxy_port = proxy.port();
    let client = build_test_client();

    // Test unknown subdomain
    let req_unknown = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/"))
        .header(HOST, format!("unknown-subdomain.localhost:{proxy_port}"))
        .body(full_body(""))
        .unwrap();
    let res_unknown = client.request(req_unknown).await.unwrap();
    assert_eq!(res_unknown.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body_unknown = res_unknown.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body_unknown).contains("not registered"));

    // Test stopped service
    let req_stopped = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/"))
        .header(HOST, format!("stopped.localhost:{proxy_port}"))
        .body(full_body(""))
        .unwrap();
    let res_stopped = client.request(req_stopped).await.unwrap();
    assert_eq!(res_stopped.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body_stopped = res_stopped.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body_stopped).contains("stopped"));

    // Test starting service with Retry-After: 1
    let req_starting = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/"))
        .header(HOST, format!("starting.localhost:{proxy_port}"))
        .body(full_body(""))
        .unwrap();
    let res_starting = client.request(req_starting).await.unwrap();
    assert_eq!(res_starting.status(), StatusCode::SERVICE_UNAVAILABLE);
    let retry_after = res_starting
        .headers()
        .get("retry-after")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(retry_after, "1");

    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_websocket_upgrade_handshake_forwarding() {
    let mock = MockHttpServer::start("ws_backend").await;
    let mock_port = mock.port();

    let supervisor = Arc::new(Supervisor::new().unwrap());
    let svc_def = create_mock_service_def("ws_app", "ws", mock_port);
    let supervised = supervisor
        .add_service(svc_def, Some(mock_port))
        .await
        .unwrap();
    let _ = supervised.status_tx.send(ProcessStatus::Running(5001));

    let proxy = ProxyServer::spawn(0, supervisor).await.unwrap();
    let proxy_port = proxy.port();

    // Connect raw TCP socket to the reverse proxy
    let proxy_addr = SocketAddr::from(([127, 0, 0, 1], proxy_port));
    let mut stream = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();

    let client_key = "dGhlIHNhbXBsZSBub25jZQ==";
    let expected_accept = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

    // Send HTTP 1.1 WebSocket Upgrade request
    let handshake_req = format!(
        "GET /ws HTTP/1.1\r\n\
        Host: ws.localhost:{proxy_port}\r\n\
        Upgrade: websocket\r\n\
        Connection: Upgrade\r\n\
        Sec-WebSocket-Key: {client_key}\r\n\
        Sec-WebSocket-Version: 13\r\n\
        \r\n"
    );
    stream.write_all(handshake_req.as_bytes()).await.unwrap();

    // Read response headers
    let mut response_buf = Vec::new();
    let mut temp = [0u8; 1024];
    loop {
        let n = stream.read(&mut temp).await.unwrap();
        assert!(n > 0, "Server closed connection during handshake");
        response_buf.extend_from_slice(&temp[..n]);
        if response_buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let response_text = String::from_utf8_lossy(&response_buf);
    assert!(
        response_text.contains("101 Switching Protocols"),
        "Must respond 101: {response_text}"
    );
    assert!(
        response_text.to_lowercase().contains("upgrade: websocket"),
        "Must contain Upgrade header: {response_text}"
    );
    assert!(
        response_text.contains(expected_accept),
        "Must contain computed Sec-WebSocket-Accept token {expected_accept}: {response_text}"
    );

    // Send data over full-duplex tunnel and verify echo
    let payload = b"Hello through spinmux WebSocket tunnel!";
    stream.write_all(payload).await.unwrap();

    let mut echo_buf = vec![0u8; payload.len()];
    stream.read_exact(&mut echo_buf).await.unwrap();
    assert_eq!(&echo_buf[..], &payload[..]);

    drop(stream);
    mock.shutdown().await;
    proxy.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_root_domain_cockpit_overview() {
    let supervisor = Arc::new(Supervisor::new().unwrap());

    let def1 = create_mock_service_def("web_app", "web", 3000);
    let s1 = supervisor.add_service(def1, Some(3000)).await.unwrap();
    let _ = s1.status_tx.send(ProcessStatus::Running(6001));

    let def2 = create_mock_service_def("api_app", "api", 4000);
    let s2 = supervisor.add_service(def2, Some(4000)).await.unwrap();
    let _ = s2.status_tx.send(ProcessStatus::Stopped(None));

    let proxy = ProxyServer::spawn(0, supervisor).await.unwrap();
    let proxy_port = proxy.port();
    let client = build_test_client();

    // 1. Explicit admin status path /_spinmux returns HTML overview
    let req_html = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/_spinmux"))
        .header(HOST, format!("localhost:{proxy_port}"))
        .header(ACCEPT, "text/html")
        .body(full_body(""))
        .unwrap();

    let res_html = client.request(req_html).await.unwrap();
    assert_eq!(res_html.status(), StatusCode::OK);
    let body_html = res_html.into_body().collect().await.unwrap().to_bytes();
    let html_str = String::from_utf8_lossy(&body_html);
    assert!(html_str.contains("spinmux"));
    assert!(html_str.contains("Cockpit Overview"));
    assert!(html_str.contains("web.localhost"));
    assert!(html_str.contains("api.localhost"));

    // 2. Explicit admin status path /_status returns JSON
    let req_json = Request::builder()
        .uri(format!("http://127.0.0.1:{proxy_port}/_status"))
        .header(HOST, format!("localhost:{proxy_port}"))
        .header(ACCEPT, "application/json")
        .body(full_body(""))
        .unwrap();

    let res_json = client.request(req_json).await.unwrap();
    assert_eq!(res_json.status(), StatusCode::OK);
    let body_json = res_json.into_body().collect().await.unwrap().to_bytes();
    let json_val: serde_json::Value = serde_json::from_slice(&body_json).unwrap();
    assert_eq!(json_val["spinmux"], true);
    assert_eq!(json_val["proxy_port"], proxy_port);
    let services = json_val["services"].as_array().unwrap();
    assert_eq!(services.len(), 2);

    proxy.shutdown().await.unwrap();
}
