use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::{sanitize_subdomain, DiscoveryError, ServiceDefinition, ServiceSource};

/// Classifies whether an npm script is a development service runner.
pub fn is_dev_script(name: &str) -> bool {
    let lower = name.to_lowercase();

    // 1. Explicit exclusions: lifecycle, build, test, lint, format
    if lower == "build"
        || lower == "test"
        || lower == "lint"
        || lower == "format"
        || lower == "check"
        || lower == "typecheck"
        || lower == "prepare"
        || lower == "prepublish"
        || lower == "prepublishonly"
        || lower == "postinstall"
        || lower.starts_with("pretest")
        || lower.starts_with("posttest")
        || lower.starts_with("prebuild")
        || lower.starts_with("postbuild")
        || lower.starts_with("lint:")
        || lower.starts_with("test:")
    {
        return false;
    }

    // 2. High-confidence development script patterns
    if lower == "dev"
        || lower == "start"
        || lower == "serve"
        || lower == "server"
        || lower == "api"
        || lower == "web"
        || lower == "worker"
        || lower == "watch"
        || lower.starts_with("dev:")
        || lower.starts_with("dev-")
        || lower.starts_with("dev_")
        || lower.starts_with("start:")
        || lower.starts_with("start-")
        || lower.starts_with("serve:")
        || lower.starts_with("serve-")
        || lower.starts_with("server:")
        || lower.starts_with("server-")
        || lower.ends_with(":dev")
        || lower.ends_with("-dev")
        || lower.ends_with(":start")
        || lower.ends_with("-start")
        || lower.ends_with(":watch")
        || lower.ends_with("-watch")
    {
        return true;
    }

    false
}

/// Heuristically extracts a target port from script content if specified
pub fn detect_port_in_script(script_content: &str) -> Option<u16> {
    // Look for PORT=xxxx or port:xxxx or --port xxxx or listen(xxxx)
    let patterns = ["PORT=", "port=", "--port ", "--port=", "listen("];
    for pat in &patterns {
        if let Some(pos) = script_content.find(pat) {
            let after = &script_content[pos + pat.len()..];
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(port) = digits.parse::<u16>() {
                if port > 0 {
                    return Some(port);
                }
            }
        }
    }
    None
}

/// Parses a package.json file and extracts runnable development services
pub fn parse_package_json(path: &Path) -> Result<Vec<ServiceDefinition>, DiscoveryError> {
    if !path.exists() {
        return Err(DiscoveryError::NotFound(path.to_path_buf()));
    }

    let content = fs::read_to_string(path)?;
    parse_package_json_content(&content, path)
}

/// Parses package.json string content
pub fn parse_package_json_content(
    content: &str,
    path: &Path,
) -> Result<Vec<ServiceDefinition>, DiscoveryError> {
    // Strip UTF-8 BOM if present (standard for Windows editors)
    let sanitized_content = content.strip_prefix('\u{FEFF}').unwrap_or(content);

    let root: Value =
        serde_json::from_str(sanitized_content).map_err(|e| DiscoveryError::Json {
            path: path.to_path_buf(),
            source: e,
        })?;

    let scripts = match root.get("scripts").and_then(|s| s.as_object()) {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };

    let working_dir = path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let mut services = Vec::new();

    for (script_name, script_val) in scripts {
        let script_cmd = match script_val.as_str() {
            Some(cmd) => cmd,
            None => continue,
        };

        if !is_dev_script(script_name) {
            continue;
        }

        let detected_port = detect_port_in_script(script_cmd);
        let lower_cmd = script_cmd.to_lowercase();
        let lower_name = script_name.to_lowercase();

        let is_http = detected_port.is_some()
            || lower_name.contains("web")
            || lower_name.contains("api")
            || lower_name.contains("server")
            || lower_name == "dev"
            || lower_name == "start"
            || lower_cmd.contains("http")
            || lower_cmd.contains("express")
            || lower_cmd.contains("vite")
            || lower_cmd.contains("listen")
            || lower_cmd.contains("port");

        let subdomain = sanitize_subdomain(script_name);

        let (command, args) = if cfg!(windows) {
            (
                "npm.cmd".to_string(),
                vec!["run".to_string(), script_name.clone()],
            )
        } else {
            (
                "npm".to_string(),
                vec!["run".to_string(), script_name.clone()],
            )
        };

        let mut env = HashMap::new();
        env.insert("SPINMUX_SCRIPT_NAME".to_string(), script_name.clone());
        env.insert("SPINMUX_RAW_CMD".to_string(), script_cmd.to_string());

        services.push(ServiceDefinition {
            id: script_name.clone(),
            name: script_name.clone(),
            command,
            args,
            working_dir: working_dir.clone(),
            env,
            source: ServiceSource::PackageJson {
                path: path.to_path_buf(),
                script: script_name.clone(),
            },
            target_port: detected_port,
            is_http,
            subdomain,
        });
    }

    Ok(services)
}
