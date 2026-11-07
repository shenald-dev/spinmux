use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::supervisor::ProcessStatus;

/// Extracts the virtual subdomain from a `Host` header value.
///
/// Supports:
/// - `<subdomain>.localhost` (e.g. `frontend.localhost:8080`, `api.localhost`)
/// - `<subdomain>.127.0.0.1.nip.io` (e.g. `api.127.0.0.1.nip.io:8080`)
///
/// Returns `None` for root hosts like `localhost`, `127.0.0.1`, `[::1]`, or empty string.
pub fn extract_subdomain(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || host.starts_with('[') {
        return None;
    }

    // Strip port if present (e.g., "frontend.localhost:8080" -> "frontend.localhost")
    let hostname = host.split(':').next()?.trim().to_ascii_lowercase();

    if let Some(prefix) = hostname.strip_suffix(".localhost") {
        let label = prefix.split('.').next()?;
        if !label.is_empty() {
            return Some(label.to_string());
        }
    }

    if let Some(prefix) = hostname.strip_suffix(".127.0.0.1.nip.io") {
        let label = prefix.split('.').next()?;
        if !label.is_empty() {
            return Some(label.to_string());
        }
    }

    None
}

/// Helper to extract subdomain from Hyper request headers
pub fn extract_subdomain_from_headers(headers: &hyper::HeaderMap) -> Option<String> {
    let host = headers.get(hyper::header::HOST)?.to_str().ok()?;
    extract_subdomain(host)
}

/// Dynamic routing record mapping a virtual subdomain to a local allocated port
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRoute {
    pub service_name: String,
    pub subdomain: String,
    pub local_port: u16,
    pub status: ProcessStatus,
}

/// Trait abstraction for routing registry
pub trait RouteRegistry: Send + Sync {
    fn get_route(&self, subdomain: &str) -> Option<ServiceRoute>;
    fn register_route(&self, route: ServiceRoute);
    fn update_status(&self, service_name: &str, status: ProcessStatus);
}

/// In-memory thread-safe route registry
#[derive(Debug, Default, Clone)]
pub struct MemoryRouteRegistry {
    routes: Arc<RwLock<HashMap<String, ServiceRoute>>>,
}

impl MemoryRouteRegistry {
    pub fn new() -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get(&self, subdomain: &str) -> Option<ServiceRoute> {
        let map = self.routes.read().await;
        map.get(subdomain).cloned()
    }

    pub async fn register(&self, route: ServiceRoute) {
        let mut map = self.routes.write().await;
        map.insert(route.subdomain.clone(), route);
    }

    pub async fn update(&self, service_name: &str, status: ProcessStatus) {
        let mut map = self.routes.write().await;
        for route in map.values_mut() {
            if route.service_name == service_name {
                route.status = status;
                break;
            }
        }
    }

    pub async fn all(&self) -> Vec<ServiceRoute> {
        let map = self.routes.read().await;
        map.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_subdomain_basic() {
        assert_eq!(
            extract_subdomain("frontend.localhost"),
            Some("frontend".to_string())
        );
        assert_eq!(extract_subdomain("api.localhost"), Some("api".to_string()));
        assert_eq!(extract_subdomain("web.localhost"), Some("web".to_string()));
    }

    #[test]
    fn test_extract_subdomain_ports() {
        assert_eq!(
            extract_subdomain("frontend.localhost:8080"),
            Some("frontend".to_string())
        );
        assert_eq!(
            extract_subdomain("api.localhost:3000"),
            Some("api".to_string())
        );
        assert_eq!(
            extract_subdomain("service-1.localhost:65535"),
            Some("service-1".to_string())
        );
    }

    #[test]
    fn test_extract_subdomain_nip_io() {
        assert_eq!(
            extract_subdomain("frontend.127.0.0.1.nip.io:8080"),
            Some("frontend".to_string())
        );
        assert_eq!(
            extract_subdomain("api.127.0.0.1.nip.io"),
            Some("api".to_string())
        );
    }

    #[test]
    fn test_extract_subdomain_root_hosts_return_none() {
        assert_eq!(extract_subdomain("localhost"), None);
        assert_eq!(extract_subdomain("localhost:8080"), None);
        assert_eq!(extract_subdomain("127.0.0.1"), None);
        assert_eq!(extract_subdomain("127.0.0.1:8080"), None);
        assert_eq!(extract_subdomain("[::1]:8080"), None);
        assert_eq!(extract_subdomain(""), None);
    }

    #[test]
    fn test_extract_subdomain_case_insensitive() {
        assert_eq!(
            extract_subdomain("FRONTEND.Localhost:8080"),
            Some("frontend".to_string())
        );
        assert_eq!(extract_subdomain("Api.Localhost"), Some("api".to_string()));
    }

    #[tokio::test]
    async fn test_memory_route_registry() {
        let registry = MemoryRouteRegistry::new();
        let route = ServiceRoute {
            service_name: "api".to_string(),
            subdomain: "api".to_string(),
            local_port: 45000,
            status: ProcessStatus::Starting,
        };

        registry.register(route.clone()).await;
        assert_eq!(registry.get("api").await, Some(route));

        registry.update("api", ProcessStatus::Running(1234)).await;
        let updated = registry.get("api").await.unwrap();
        assert_eq!(updated.status, ProcessStatus::Running(1234));
    }
}
