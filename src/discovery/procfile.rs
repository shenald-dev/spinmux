use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::{sanitize_subdomain, DiscoveryError, ServiceDefinition, ServiceSource};

/// Tokenizes a shell command string into the program executable and its arguments,
/// properly respecting double and single quotes and escape sequences.
pub fn tokenize_command(cmd_str: &str) -> (String, Vec<String>) {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escape = false;

    let chars: Vec<char> = cmd_str.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        if escape {
            current.push(c);
            escape = false;
            i += 1;
            continue;
        }

        match c {
            '\\' if !in_single_quote => {
                // If inside double quotes, escape quotes or keep backslash
                if in_double_quote {
                    if i + 1 < chars.len() && (chars[i + 1] == '"' || chars[i + 1] == '\\') {
                        escape = true;
                    } else {
                        current.push('\\');
                    }
                } else if i + 1 < chars.len() && (chars[i + 1] == '"' || chars[i + 1] == '\'') {
                    // Only treat backslash as an escape character when explicitly escaping quotes
                    escape = true;
                } else if i + 1 < chars.len() || !current.is_empty() {
                    // Preserve unquoted backslashes as literal characters for Windows paths
                    current.push('\\');
                }
            }
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    tokens.push(current);
                    current = String::new();
                }
            }
            _ => {
                current.push(c);
            }
        }
        i += 1;
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    if tokens.is_empty() {
        ("".to_string(), Vec::new())
    } else {
        let program = tokens.remove(0);
        (program, tokens)
    }
}

/// Detects if a command uses a port or binds to $PORT
pub fn detect_port_in_command(cmd: &str) -> Option<u16> {
    let lower = cmd.to_lowercase();
    let patterns = ["--port ", "--port=", "-p ", "port="];
    for pat in &patterns {
        if let Some(pos) = lower.find(pat) {
            let after = &cmd[pos + pat.len()..];
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

/// Parses a Procfile (e.g. Procfile, Procfile.dev, Procfile.local) from disk
pub fn parse_procfile(
    path: &Path,
    variant: &str,
) -> Result<Vec<ServiceDefinition>, DiscoveryError> {
    if !path.exists() {
        return Err(DiscoveryError::NotFound(path.to_path_buf()));
    }

    let content = fs::read_to_string(path)?;
    parse_procfile_content(&content, path, variant)
}

/// Parses Procfile content directly
pub fn parse_procfile_content(
    content: &str,
    path: &Path,
    variant: &str,
) -> Result<Vec<ServiceDefinition>, DiscoveryError> {
    // 1. Strip UTF-8 BOM if present
    let sanitized_content = content.strip_prefix('\u{FEFF}').unwrap_or(content);

    let working_dir = path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    let mut services = Vec::new();

    for (line_num, raw_line) in sanitized_content.lines().enumerate() {
        let line = raw_line.trim();

        // Discard empty lines and comment lines
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Split on FIRST colon
        let colon_pos = match line.find(':') {
            Some(pos) => pos,
            None => {
                return Err(DiscoveryError::MalformedConfig {
                    path: path.to_path_buf(),
                    message: format!(
                        "Procfile line {} is missing colon delimiter (expected 'name: command'): '{}'",
                        line_num + 1,
                        line
                    ),
                });
            }
        };

        let name = line[..colon_pos].trim();
        let cmd_part = line[colon_pos + 1..].trim();

        if name.is_empty() {
            return Err(DiscoveryError::MalformedConfig {
                path: path.to_path_buf(),
                message: format!(
                    "Procfile line {} has empty process name: '{}'",
                    line_num + 1,
                    line
                ),
            });
        }

        if cmd_part.is_empty() {
            return Err(DiscoveryError::MalformedConfig {
                path: path.to_path_buf(),
                message: format!(
                    "Procfile line {} has empty command for service '{}'",
                    line_num + 1,
                    name
                ),
            });
        }

        let (mut command, args) = tokenize_command(cmd_part);

        // Normalize command on Windows if calling npm / yarn / pnpm directly
        if cfg!(windows) {
            if command == "npm" {
                command = "npm.cmd".to_string();
            } else if command == "yarn" {
                command = "yarn.cmd".to_string();
            } else if command == "pnpm" {
                command = "pnpm.cmd".to_string();
            } else if command == "npx" {
                command = "npx.cmd".to_string();
            }
        }

        let detected_port = detect_port_in_command(cmd_part);
        let lower_name = name.to_lowercase();
        let lower_cmd = cmd_part.to_lowercase();

        let is_http = detected_port.is_some()
            || lower_name == "web"
            || lower_name == "api"
            || lower_name.contains("server")
            || lower_cmd.contains("$port")
            || lower_cmd.contains("${port}")
            || lower_cmd.contains("http")
            || lower_cmd.contains("serve");

        let subdomain = sanitize_subdomain(name);

        let mut env = HashMap::new();
        env.insert("SPINMUX_PROCFILE_CMD".to_string(), cmd_part.to_string());

        services.push(ServiceDefinition {
            id: name.to_string(),
            name: name.to_string(),
            command,
            args,
            working_dir: working_dir.clone(),
            env,
            source: ServiceSource::Procfile {
                path: path.to_path_buf(),
                variant: variant.to_string(),
            },
            target_port: detected_port,
            is_http,
            subdomain,
        });
    }

    Ok(services)
}
