use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::header::{CONNECTION, CONTENT_TYPE, HOST, UPGRADE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, RwLock};

pub type MockBody = BoxBody<Bytes, hyper::Error>;

fn empty_body() -> MockBody {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

fn full_body(bytes: impl Into<Bytes>) -> MockBody {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed()
}

pub struct ChannelBody {
    rx: tokio::sync::mpsc::Receiver<Result<hyper::body::Frame<Bytes>, hyper::Error>>,
}

impl hyper::body::Body for ChannelBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        self.rx.poll_recv(cx)
    }
}

pub fn channel_body() -> (
    tokio::sync::mpsc::Sender<Result<hyper::body::Frame<Bytes>, hyper::Error>>,
    MockBody,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    (tx, ChannelBody { rx }.boxed())
}

/// Computes SHA-1 hash (RFC 3174) in pure Rust with zero external dependencies
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x67452301;
    let mut h1: u32 = 0xEFCDAB89;
    let mut h2: u32 = 0x98BADCFE;
    let mut h3: u32 = 0x10325476;
    let mut h4: u32 = 0xC3D2E1F0;

    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&ml.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;

        for (i, &item) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(item);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

/// Standard Base64 encoder in pure Rust
pub fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };

        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);

        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }

        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }

    out
}

/// Computes RFC 6455 Sec-WebSocket-Accept token
pub fn compute_ws_accept(key: &str) -> String {
    const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let mut concat = key.trim().to_string();
    concat.push_str(WS_GUID);
    let hash = sha1(concat.as_bytes());
    base64_encode(&hash)
}

pub type CustomResponse = (StatusCode, String, Vec<(String, String)>);

/// Internal state configuration for mock HTTP server
#[allow(dead_code)]
#[derive(Clone)]
struct MockState {
    name: String,
    request_count: Arc<AtomicUsize>,
    custom_response: Arc<RwLock<Option<CustomResponse>>>,
}

/// In-process hermetic mock HTTP server for reverse proxy testing
#[allow(dead_code)]
pub struct MockHttpServer {
    port: u16,
    local_addr: SocketAddr,
    state: MockState,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task_handle: tokio::task::JoinHandle<()>,
}

#[allow(dead_code)]
impl MockHttpServer {
    /// Starts a new mock server listening on 127.0.0.1:0 with an optional service name
    pub async fn start(name: impl Into<String>) -> Self {
        let name = name.into();
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("Failed to bind mock server on 127.0.0.1:0");
        let local_addr = listener
            .local_addr()
            .expect("Failed to get mock server local addr");
        let port = local_addr.port();

        let state = MockState {
            name,
            request_count: Arc::new(AtomicUsize::new(0)),
            custom_response: Arc::new(RwLock::new(None)),
        };

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let server_state = state.clone();

        let task_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        break;
                    }
                    accept_res = listener.accept() => {
                        match accept_res {
                            Ok((stream, _)) => {
                                let io = TokioIo::new(stream);
                                let state_clone = server_state.clone();

                                tokio::spawn(async move {
                                    let service = service_fn(move |req| {
                                        let state_inner = state_clone.clone();
                                        async move {
                                            handle_mock_request(req, state_inner).await
                                        }
                                    });

                                    let _ = http1::Builder::new()
                                        .serve_connection(io, service)
                                        .with_upgrades()
                                        .await;
                                });
                            }
                            Err(_) => {
                                break;
                            }
                        }
                    }
                }
            }
        });

        Self {
            port,
            local_addr,
            state,
            shutdown_tx: Some(shutdown_tx),
            task_handle,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn request_count(&self) -> usize {
        self.state.request_count.load(Ordering::SeqCst)
    }

    /// Sets a fixed custom status code, body, and headers returned for all subsequent requests
    pub async fn set_custom_response(
        &self,
        status: StatusCode,
        body: impl Into<String>,
        headers: &[(&str, &str)],
    ) {
        let hdrs = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut guard = self.state.custom_response.write().await;
        *guard = Some((status, body.into(), hdrs));
    }

    /// Clears any configured custom response
    pub async fn clear_custom_response(&self) {
        let mut guard = self.state.custom_response.write().await;
        *guard = None;
    }

    /// Gracefully stops the mock HTTP server
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task_handle.await;
    }
}

async fn handle_mock_request(
    mut req: Request<hyper::body::Incoming>,
    state: MockState,
) -> Result<Response<MockBody>, hyper::Error> {
    state.request_count.fetch_add(1, Ordering::SeqCst);

    // 1. WebSocket Upgrade handling
    let is_upgrade = req
        .headers()
        .get(UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    if is_upgrade {
        let ws_key = req
            .headers()
            .get("sec-websocket-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let accept = compute_ws_accept(&ws_key);

        let mut res = Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header(UPGRADE, "websocket")
            .header(CONNECTION, "Upgrade")
            .header("sec-websocket-accept", accept)
            .body(empty_body())
            .unwrap();

        // Echo forwarded headers for test assertion
        if let Some(fwd_host) = req.headers().get("x-forwarded-host") {
            res.headers_mut()
                .insert("x-echo-forwarded-host", fwd_host.clone());
        }

        tokio::spawn(async move {
            match hyper::upgrade::on(&mut req).await {
                Ok(upgraded) => {
                    let mut stream = TokioIo::new(upgraded);
                    let mut buf = [0u8; 1024];
                    loop {
                        match stream.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                if stream.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
                Err(err) => {
                    tracing::error!("Mock server upgrade error: {err}");
                }
            }
        });

        return Ok(res);
    }

    // 2. Custom Response override
    {
        let guard = state.custom_response.read().await;
        if let Some((status, body, headers)) = &*guard {
            let mut builder = Response::builder().status(*status);
            for (k, v) in headers {
                builder = builder.header(k.as_str(), v.as_str());
            }
            return Ok(builder.body(full_body(body.clone())).unwrap());
        }
    }

    let path = req.uri().path();

    // 3. Status code route: /status/{code}
    if let Some(code_str) = path.strip_prefix("/status/") {
        if let Ok(code) = code_str.parse::<u16>() {
            let status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
            return Ok(Response::builder()
                .status(status)
                .body(full_body(format!("Status: {code}")))
                .unwrap());
        }
    }

    // 4. Server-Sent Events (SSE) route: /sse
    if path == "/sse" {
        let (sender, body) = channel_body();
        tokio::spawn(async move {
            for i in 1..=3 {
                let data = format!("data: event {}\n\n", i);
                let _ = sender
                    .send(Ok(hyper::body::Frame::data(Bytes::from(data))))
                    .await;
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        });

        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/event-stream")
            .header("cache-control", "no-cache")
            .body(body)
            .unwrap());
    }

    // 5. Large streaming payload (GET /stream/large): sends 1MB in 16 chunks
    if path == "/stream/large" {
        let (sender, body) = channel_body();
        tokio::spawn(async move {
            let chunk = vec![b'X'; 64 * 1024]; // 64KB chunk
            for _ in 0..16 {
                let _ = sender
                    .send(Ok(hyper::body::Frame::data(Bytes::copy_from_slice(&chunk))))
                    .await;
            }
        });

        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .unwrap());
    }

    // 6. Echo route: /echo
    if path == "/echo" {
        let (parts, body) = req.into_parts();
        let whole_body = body.collect().await?.to_bytes();

        let mut res = Response::builder()
            .status(StatusCode::OK)
            .body(full_body(whole_body))
            .unwrap();

        // Echo request headers
        if let Some(fwd_for) = parts.headers.get("x-forwarded-for") {
            res.headers_mut()
                .insert("x-echo-forwarded-for", fwd_for.clone());
        }
        if let Some(fwd_host) = parts.headers.get("x-forwarded-host") {
            res.headers_mut()
                .insert("x-echo-forwarded-host", fwd_host.clone());
        }
        if let Some(fwd_proto) = parts.headers.get("x-forwarded-proto") {
            res.headers_mut()
                .insert("x-echo-forwarded-proto", fwd_proto.clone());
        }
        if let Some(host) = parts.headers.get(HOST) {
            res.headers_mut().insert("x-echo-host", host.clone());
        }

        return Ok(res);
    }

    // 7. Default health / hello response
    let response_text = format!("Hello from mock service '{}'", state.name);
    let mut res = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/plain")
        .body(full_body(response_text))
        .unwrap();

    if let Some(fwd_for) = req.headers().get("x-forwarded-for") {
        res.headers_mut()
            .insert("x-echo-forwarded-for", fwd_for.clone());
    }
    if let Some(fwd_host) = req.headers().get("x-forwarded-host") {
        res.headers_mut()
            .insert("x-echo-forwarded-host", fwd_host.clone());
    }
    if let Some(fwd_proto) = req.headers().get("x-forwarded-proto") {
        res.headers_mut()
            .insert("x-echo-forwarded-proto", fwd_proto.clone());
    }

    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rfc6455_sec_websocket_accept() {
        // Standard test vector from RFC 6455 Section 1.3
        let client_key = "dGhlIHNhbXBsZSBub25jZQ==";
        let expected_accept = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
        assert_eq!(compute_ws_accept(client_key), expected_accept);
    }

    #[test]
    fn test_sha1_standard() {
        let digest = sha1(b"abc");
        let hex = digest
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
        assert_eq!(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
}
