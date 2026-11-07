use serde_yaml::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::procfile::tokenize_command;
use super::{sanitize_subdomain, DiscoveryError, ServiceDefinition, ServiceSource};

/// Extracts the host-published port from a compose port mapping entry
pub fn parse_compose_port_entry(port_val: &Value) -> Option<u16> {
    match port_val {
        Value::String(s) => parse_port_string(s),
        Value::Number(num) => num.as_u64().and_then(|p| u16::try_from(p).ok()),
        Value::Mapping(map) => {
            // Check for published or target key
            let published_key = Value::String("published".to_string());
            let target_key = Value::String("target".to_string());

            if let Some(pub_val) = map.get(&published_key) {
                if let Some(port_num) = pub_val.as_u64() {
                    if let Ok(p) = u16::try_from(port_num) {
                        return Some(p);
                    }
                } else if let Some(port_str) = pub_val.as_str() {
                    return parse_port_string(port_str);
                }
            }

            if let Some(tgt_val) = map.get(&target_key) {
                if let Some(port_num) = tgt_val.as_u64() {
                    if let Ok(p) = u16::try_from(port_num) {
                        return Some(p);
                    }
                } else if let Some(port_str) = tgt_val.as_str() {
                    return parse_port_string(port_str);
                }
            }

            None
        }
        _ => None,
    }
}

/// Parses port string specifications like "3000:3000", "127.0.0.1:8080:80", "5000", "8080:80/tcp"
fn parse_port_string(s: &str) -> Option<u16> {
    // Strip protocol suffix e.g. /tcp, /udp
    let clean = s.split('/').next()?.trim();

    let parts: Vec<&str> = clean.split(':').collect();
    match parts.len() {
        1 => parts[0].trim().parse::<u16>().ok(),
        2 => parts[0].trim().parse::<u16>().ok(),
        3 => parts[1].trim().parse::<u16>().ok(), // e.g. "127.0.0.1:8080:80" -> 8080
        _ => None,
    }
}

/// Parses a docker-compose.yml or compose.yaml file
pub fn parse_compose_file(path: &Path) -> Result<Vec<ServiceDefinition>, DiscoveryError> {
    if !path.exists() {
        return Err(DiscoveryError::NotFound(path.to_path_buf()));
    }

    let content = fs::read_to_string(path)?;
    parse_compose_content(&content, path)
}

/// Parses compose YAML content directly
pub fn parse_compose_content(
    content: &str,
    path: &Path,
) -> Result<Vec<ServiceDefinition>, DiscoveryError> {
    // Strip UTF-8 BOM if present
    let sanitized_content = content.strip_prefix('\u{FEFF}').unwrap_or(content);

    let root: Value =
        serde_yaml::from_str(sanitized_content).map_err(|e| DiscoveryError::Yaml {
            path: path.to_path_buf(),
            source: e,
        })?;

    let working_dir = path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let services_val = match root.get("services") {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };

    let services_map = match services_val.as_mapping() {
        Some(m) => m,
        None => {
            return Err(DiscoveryError::MalformedConfig {
                path: path.to_path_buf(),
                message: "'services' section must be a YAML mapping".to_string(),
            });
        }
    };

    let mut services = Vec::new();

    for (svc_key, svc_spec) in services_map {
        let service_name = match svc_key.as_str() {
            Some(name) => name.to_string(),
            None => continue,
        };

        // Extract command or fallback
        let (command, args) = if let Some(cmd_val) = svc_spec.get("command") {
            match cmd_val {
                Value::String(s) => tokenize_command(s),
                Value::Sequence(seq) => {
                    let mut tokens: Vec<String> = seq
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                    if tokens.is_empty() {
                        (
                            "docker".to_string(),
                            vec![
                                "compose".to_string(),
                                "up".to_string(),
                                service_name.clone(),
                            ],
                        )
                    } else {
                        let prog = tokens.remove(0);
                        (prog, tokens)
                    }
                }
                _ => (
                    "docker".to_string(),
                    vec![
                        "compose".to_string(),
                        "up".to_string(),
                        service_name.clone(),
                    ],
                ),
            }
        } else {
            (
                "docker".to_string(),
                vec![
                    "compose".to_string(),
                    "up".to_string(),
                    service_name.clone(),
                ],
            )
        };

        // Extract ports
        let mut target_port = None;
        if let Some(ports_val) = svc_spec.get("ports") {
            if let Some(seq) = ports_val.as_sequence() {
                for item in seq {
                    if let Some(p) = parse_compose_port_entry(item) {
                        target_port = Some(p);
                        break;
                    }
                }
            }
        }

        // Extract environment variables
        let mut env = HashMap::new();
        if let Some(env_val) = svc_spec.get("environment") {
            if let Some(map) = env_val.as_mapping() {
                for (k, v) in map {
                    if let Some(k_str) = k.as_str() {
                        let v_str = match v {
                            Value::String(s) => s.clone(),
                            Value::Number(n) => n.to_string(),
                            Value::Bool(b) => b.to_string(),
                            _ => "".to_string(),
                        };
                        env.insert(k_str.to_string(), v_str);
                    }
                }
            } else if let Some(seq) = env_val.as_sequence() {
                for item in seq {
                    if let Some(s) = item.as_str() {
                        if let Some((k, v)) = s.split_once('=') {
                            env.insert(k.trim().to_string(), v.trim().to_string());
                        }
                    }
                }
            }
        }

        // Custom working dir if specified
        let svc_working_dir =
            if let Some(wd_val) = svc_spec.get("working_dir").and_then(|v| v.as_str()) {
                working_dir.join(wd_val)
            } else {
                working_dir.clone()
            };

        let lower_name = service_name.to_lowercase();
        let is_http = target_port.is_some()
            || lower_name.contains("web")
            || lower_name.contains("api")
            || lower_name.contains("server");

        let subdomain = sanitize_subdomain(&service_name);

        services.push(ServiceDefinition {
            id: service_name.clone(),
            name: service_name.clone(),
            command,
            args,
            working_dir: svc_working_dir,
            env,
            source: ServiceSource::DockerCompose {
                path: path.to_path_buf(),
                service_name: service_name.clone(),
            },
            target_port,
            is_http,
            subdomain,
        });
    }

    Ok(services)
}
