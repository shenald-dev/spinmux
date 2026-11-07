use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::supervisor::Supervisor;

pub mod handler;
pub mod router;

pub use handler::{empty_body, full_body, handle_request, ProxyBody};
pub use router::{
    extract_subdomain, extract_subdomain_from_headers, MemoryRouteRegistry, RouteRegistry,
    ServiceRoute,
};

/// Errors occurring within the reverse proxy
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("Failed to bind reverse proxy on {0}: {1}")]
    BindFailed(SocketAddr, std::io::Error),

    #[error("I/O error in reverse proxy: {0}")]
    Io(#[from] std::io::Error),

    #[error("Hyper error: {0}")]
    Hyper(#[from] hyper::Error),

    #[error("HTTP client error: {0}")]
    Client(#[from] hyper_util::client::legacy::Error),

    #[error("Server shutdown encountered an error")]
    ServerShutdown,
}

/// Reverse proxy server routing virtual localhost subdomains to dynamic local ports
pub struct ProxyServer {
    listener: TcpListener,
    supervisor: Arc<Supervisor>,
    port: u16,
    client: hyper_util::client::legacy::Client<
        hyper_util::client::legacy::connect::HttpConnector,
        ProxyBody,
    >,
}

impl ProxyServer {
    /// Binds the reverse proxy server to 127.0.0.1 on the specified port.
    /// If `port == 0`, the OS dynamically allocates an ephemeral port.
    pub async fn bind(port: u16, supervisor: Arc<Supervisor>) -> Result<Self, ProxyError> {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| ProxyError::BindFailed(addr, e))?;
        let actual_port = listener
            .local_addr()
            .map_err(|e| ProxyError::BindFailed(addr, e))?
            .port();

        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(hyper_util::client::legacy::connect::HttpConnector::new());

        Ok(Self {
            listener,
            supervisor,
            port: actual_port,
            client,
        })
    }

    /// Returns the port the proxy is listening on
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Returns the local socket address
    pub fn local_addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([127, 0, 0, 1], self.port)))
    }

    /// Spawns the proxy server into a background Tokio task with a managed graceful shutdown handle
    pub async fn spawn(port: u16, supervisor: Arc<Supervisor>) -> Result<RunningProxy, ProxyError> {
        let server = Self::bind(port, supervisor).await?;
        let actual_port = server.port();
        let local_addr = server.local_addr();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task_handle = tokio::spawn(async move {
            server
                .run(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });

        Ok(RunningProxy {
            port: actual_port,
            local_addr,
            shutdown_trigger: Some(shutdown_tx),
            task_handle,
        })
    }

    /// Executes the proxy accept loop until the given shutdown signal fires
    pub async fn run(
        self,
        shutdown_signal: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), ProxyError> {
        tokio::pin!(shutdown_signal);

        loop {
            tokio::select! {
                _ = &mut shutdown_signal => {
                    tracing::info!("ProxyServer received shutdown signal. Ceasing accept loop.");
                    break;
                }
                accept_res = self.listener.accept() => {
                    match accept_res {
                        Ok((stream, client_addr)) => {
                            let io = TokioIo::new(stream);
                            let supervisor = self.supervisor.clone();
                            let client = self.client.clone();
                            let proxy_port = self.port;

                            tokio::spawn(async move {
                                let service = service_fn(move |req| {
                                    let supervisor = supervisor.clone();
                                    let client = client.clone();
                                    async move {
                                        handle_request(req, client_addr, supervisor, client, proxy_port).await
                                    }
                                });

                                if let Err(err) = http1::Builder::new()
                                    .serve_connection(io, service)
                                    .with_upgrades()
                                    .await
                                {
                                    tracing::debug!("Proxy connection closed or error: {err}");
                                }
                            });
                        }
                        Err(err) => {
                            tracing::warn!("Failed accepting connection: {err}");
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// A running proxy instance in a background task with graceful shutdown capability
pub struct RunningProxy {
    port: u16,
    local_addr: SocketAddr,
    shutdown_trigger: Option<tokio::sync::oneshot::Sender<()>>,
    task_handle: tokio::task::JoinHandle<Result<(), ProxyError>>,
}

impl RunningProxy {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Gracefully signals the proxy server to shut down and awaits task completion
    pub async fn shutdown(mut self) -> Result<(), ProxyError> {
        if let Some(trigger) = self.shutdown_trigger.take() {
            let _ = trigger.send(());
        }
        match self.task_handle.await {
            Ok(res) => res,
            Err(_) => Err(ProxyError::ServerShutdown),
        }
    }
}
