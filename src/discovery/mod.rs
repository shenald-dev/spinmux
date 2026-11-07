pub mod compose;
pub mod package_json;
pub mod procfile;

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub use compose::parse_compose_file;
pub use package_json::parse_package_json;
pub use procfile::parse_procfile;

/// Represents the origin of a discovered service definition
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceSource {
    Procfile { path: PathBuf, variant: String },
    PackageJson { path: PathBuf, script: String },
    DockerCompose { path: PathBuf, service_name: String },
    CliOverride { path: PathBuf },
}

impl std::fmt::Display for ServiceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceSource::Procfile { path, variant } => {
                write!(f, "Procfile ({}, file: {})", variant, path.display())
            }
            ServiceSource::PackageJson { path, script } => {
                write!(
                    f,
                    "package.json (script: {}, file: {})",
                    script,
                    path.display()
                )
            }
            ServiceSource::DockerCompose { path, service_name } => {
                write!(
                    f,
                    "docker-compose ({}, file: {})",
                    service_name,
                    path.display()
                )
            }
            ServiceSource::CliOverride { path } => {
                write!(f, "CLI override ({})", path.display())
            }
        }
    }
}

/// Canonical representation of a supervised service
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDefinition {
    /// Canonical unique service identifier (e.g. "web", "api", "worker", "npm:web")
    pub id: String,
    /// Service name
    pub name: String,
    /// Program executable or command
    pub command: String,
    /// Arguments passed to the command
    pub args: Vec<String>,
    /// Working directory for execution
    pub working_dir: PathBuf,
    /// Environment variables injected into the process
    pub env: HashMap<String, String>,
    /// Originating discovery source
    pub source: ServiceSource,
    /// Statically configured or discovered target port (if any)
    pub target_port: Option<u16>,
    /// Whether this service is an HTTP service expected to register with the reverse proxy
    pub is_http: bool,
    /// Subdomain assigned for virtual localhost routing (e.g. "api" -> "api.localhost")
    pub subdomain: String,
}

pub type DiscoveredService = ServiceDefinition;

/// Errors that can occur during service discovery
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error in {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },

    #[error("YAML parse error in {path}: {source}")]
    Yaml {
        path: PathBuf,
        source: serde_yaml::Error,
    },

    #[error("Malformed configuration file at {path}: {message}")]
    MalformedConfig { path: PathBuf, message: String },

    #[error("Config file not found: {0}")]
    NotFound(PathBuf),

    #[error("No services discovered in directory: {0}")]
    NoServicesFound(PathBuf),
}

/// Sanitizes an arbitrary service or script name to comply with RFC 1123 DNS subdomain label rules.
/// Subdomain labels must be at most 63 octets and not start or end with a hyphen.
pub fn sanitize_subdomain(name: &str) -> String {
    let mut out = String::new();
    let mut last_was_hyphen = false;

    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            out.push('-');
            last_was_hyphen = true;
        }
    }

    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "service".to_string()
    } else if trimmed.len() > 63 {
        let truncated = trimmed[..63].trim_end_matches('-');
        if truncated.is_empty() {
            "service".to_string()
        } else {
            truncated.to_string()
        }
    } else {
        trimmed.to_string()
    }
}

/// Discovers services within the target directory adhering to strict precedence:
/// Explicit CLI Config > Procfile.dev > Procfile.local > Procfile > package.json > compose.yaml / docker-compose.yml
pub fn discover_services(
    root_dir: &Path,
    config_override: Option<&Path>,
    filter_services: Option<&[String]>,
) -> Result<Vec<ServiceDefinition>, DiscoveryError> {
    let mut discovered: Vec<ServiceDefinition> = Vec::new();

    if let Some(config_path) = config_override {
        let full_path = if config_path.is_absolute() {
            config_path.to_path_buf()
        } else {
            root_dir.join(config_path)
        };

        if !full_path.exists() {
            return Err(DiscoveryError::NotFound(full_path));
        }

        let file_name = full_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("")
            .to_lowercase();

        let mut services = if file_name.contains("procfile") {
            parse_procfile(&full_path, "cli-config")?
        } else if file_name == "package.json" {
            parse_package_json(&full_path)?
        } else if file_name.contains("compose") {
            parse_compose_file(&full_path)?
        } else {
            // Attempt JSON, then Procfile, then Compose
            if let Ok(s) = parse_package_json(&full_path) {
                s
            } else if let Ok(s) = parse_procfile(&full_path, "cli-config") {
                s
            } else if let Ok(s) = parse_compose_file(&full_path) {
                s
            } else {
                return Err(DiscoveryError::MalformedConfig {
                    path: full_path,
                    message: "Unsupported or unparseable configuration file format".to_string(),
                });
            }
        };

        for s in &mut services {
            s.source = ServiceSource::CliOverride {
                path: full_path.clone(),
            };
        }

        discovered = services;
    } else {
        // Multi-source discovery with precedence ordering
        let procfile_candidates = [
            ("Procfile.dev", "dev"),
            ("Procfile.local", "local"),
            ("Procfile", "standard"),
        ];

        for (filename, variant) in procfile_candidates {
            let p = root_dir.join(filename);
            if p.exists() {
                let services = parse_procfile(&p, variant)?;
                discovered.extend(services);
                break; // Only the highest precedence Procfile variant is chosen
            }
        }

        // package.json discovery
        let pkg_path = root_dir.join("package.json");
        if pkg_path.exists() {
            match parse_package_json(&pkg_path) {
                Ok(services) => discovered.extend(services),
                Err(DiscoveryError::Json { path, source }) => {
                    return Err(DiscoveryError::Json { path, source });
                }
                Err(e) => return Err(e),
            }
        }

        // compose.yaml / docker-compose.yml discovery
        let compose_candidates = [
            "compose.yaml",
            "compose.yml",
            "docker-compose.yaml",
            "docker-compose.yml",
        ];

        for filename in compose_candidates {
            let p = root_dir.join(filename);
            if p.exists() {
                match parse_compose_file(&p) {
                    Ok(services) => {
                        discovered.extend(services);
                        break; // Only first matching compose file
                    }
                    Err(DiscoveryError::Yaml { path, source }) => {
                        return Err(DiscoveryError::Yaml { path, source });
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }

    // Deduplication & Conflict Resolution
    let mut resolved_services: Vec<ServiceDefinition> = Vec::new();
    let mut primary_names: HashSet<String> = HashSet::new();
    let mut used_subdomains: HashSet<String> = HashSet::new();

    for mut service in discovered {
        let original_name = service.name.clone();

        if primary_names.contains(&original_name) {
            // Collision: namespace the duplicate service
            let namespace_prefix = match &service.source {
                ServiceSource::PackageJson { .. } => "npm",
                ServiceSource::DockerCompose { .. } => "compose",
                ServiceSource::Procfile { variant, .. } => variant.as_str(),
                ServiceSource::CliOverride { .. } => "cli",
            };

            let namespaced_id = format!("{}:{}", namespace_prefix, original_name);
            let raw_namespaced_subdomain = format!("{}-{}", namespace_prefix, service.subdomain);
            let namespaced_subdomain = sanitize_subdomain(&raw_namespaced_subdomain);

            tracing::warn!(
                "Service name collision: '{}' already defined by higher precedence source. Lower precedence definition namespaced as '{}'",
                original_name,
                namespaced_id
            );

            service.id = namespaced_id;
            service.subdomain = namespaced_subdomain;
        } else {
            primary_names.insert(original_name.clone());
        }

        // Subdomain Collision Disambiguation
        // Ensure every service receives a deterministic, globally unique subdomain in the registry
        if used_subdomains.contains(&service.subdomain) {
            let base_subdomain = service.subdomain.clone();
            let mut suffix_idx = 2;
            loop {
                let suffix = format!("-{}", suffix_idx);
                // Ensure candidate length does not exceed RFC 1123 63-character limit
                let candidate = if base_subdomain.len() + suffix.len() > 63 {
                    let max_base_len = 63 - suffix.len();
                    let trimmed_base = base_subdomain[..max_base_len].trim_end_matches('-');
                    format!("{}{}", trimmed_base, suffix)
                } else {
                    format!("{}{}", base_subdomain, suffix)
                };

                if !used_subdomains.contains(&candidate) {
                    tracing::warn!(
                        "Subdomain collision detected for service '{}': subdomain '{}' already allocated. Disambiguated as '{}'",
                        service.id,
                        service.subdomain,
                        candidate
                    );
                    service.subdomain = candidate;
                    break;
                }
                suffix_idx += 1;
            }
        }

        used_subdomains.insert(service.subdomain.clone());
        resolved_services.push(service);
    }

    // Filter services if specified
    if let Some(filter) = filter_services {
        if !filter.is_empty() {
            let filter_set: HashSet<String> = filter.iter().map(|s| s.to_lowercase()).collect();
            resolved_services.retain(|s| {
                filter_set.contains(&s.id.to_lowercase())
                    || filter_set.contains(&s.name.to_lowercase())
                    || filter_set.contains(&s.subdomain.to_lowercase())
            });
        }
    }

    Ok(resolved_services)
}
