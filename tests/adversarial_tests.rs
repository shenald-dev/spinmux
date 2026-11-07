use spinmux::discovery::{
    compose::parse_compose_file,
    discover_services,
    package_json::{detect_port_in_script, parse_package_json},
    procfile::{detect_port_in_command, parse_procfile, tokenize_command},
    sanitize_subdomain, DiscoveryError,
};
use std::fs::{self, File};
use tempfile::tempdir;

// =========================================================================
// 1. Unicode & UTF-8 Stress Testing (Procfile & Port Detection)
// =========================================================================

#[test]
fn test_procfile_utf8_emojis_and_non_ascii() {
    let temp = tempdir().expect("tempdir");
    let proc_path = temp.path().join("Procfile");

    let content = "\u{FEFF}🚀-service: node app.js --port 3000\r\n\
                   тест_сервис: python server.py -p 5000\r\n\
                   café_api: ruby app.rb --port=8080\r\n\
                   # This is a comment with emojis 🔥🎉\r\n\
                   \r\n\
                   web: npm.cmd run dev --port 4000\r\n";

    fs::write(&proc_path, content.as_bytes()).expect("write procfile");

    let services = parse_procfile(&proc_path, "standard").expect("parse procfile");
    assert_eq!(services.len(), 4);

    let s0 = &services[0];
    assert_eq!(s0.name, "🚀-service");
    assert_eq!(s0.subdomain, "service");
    assert_eq!(s0.target_port, Some(3000));

    let s1 = &services[1];
    assert_eq!(s1.name, "тест_сервис");
    assert_eq!(s1.subdomain, "service");
    assert_eq!(s1.target_port, Some(5000));

    let s2 = &services[2];
    assert_eq!(s2.name, "café_api");
    assert_eq!(s2.subdomain, "caf-api");
    assert_eq!(s2.target_port, Some(8080));

    let s3 = &services[3];
    assert_eq!(s3.name, "web");
    assert_eq!(s3.subdomain, "web");
    assert_eq!(s3.target_port, Some(4000));
}

#[test]
fn test_procfile_crlf_and_whitespace_torture() {
    let temp = tempdir().expect("tempdir");
    let proc_path = temp.path().join("Procfile");

    let content = "   \r\n\
                   \t   \t\r\n\
                   \t  web   :    node   server.js   --port 3000   \r\n\
                   # comment followed by spaces   \r\n\
                     api:go run main.go -p 8080\r\n\
                   \r\n";

    fs::write(&proc_path, content.as_bytes()).expect("write procfile");
    let services = parse_procfile(&proc_path, "standard").expect("parse procfile");
    assert_eq!(services.len(), 2);

    assert_eq!(services[0].name, "web");
    assert_eq!(services[0].command, "node");
    assert_eq!(services[0].args, vec!["server.js", "--port", "3000"]);
    assert_eq!(services[0].target_port, Some(3000));

    assert_eq!(services[1].name, "api");
    assert_eq!(services[1].command, "go");
    assert_eq!(services[1].args, vec!["run", "main.go", "-p", "8080"]);
    assert_eq!(services[1].target_port, Some(8080));
}

#[test]
fn test_reproduce_unicode_index_out_of_bounds_panic() {
    // EMPIRICAL BUG REPRODUCTION:
    // In src/discovery/procfile.rs:78:
    // `let lower = cmd.to_lowercase();`
    // `pos = lower.find(pat)` searches in `lower`.
    // Then `&cmd[pos + pat.len()..]` indexes `cmd`!
    // Turkish uppercase dotted I (\u{0130}) expands from 2 to 3 bytes in lowercase.
    // When multiple expanding characters exist, `pos + pat.len()` exceeds `cmd.len()`,
    // triggering a panic: `start byte index X is out of bounds for string of length Y`.
    let cmd = "İİİİİİİİİİ node server.js --port 8080";
    let result = std::panic::catch_unwind(|| detect_port_in_command(cmd));
    assert!(
        result.is_err(),
        "CRITICAL BUG: detect_port_in_command panics with slice out of bounds on Unicode strings with length expansion"
    );
}

#[test]
fn test_reproduce_unicode_non_char_boundary_panic() {
    // EMPIRICAL BUG REPRODUCTION:
    // When byte index shift causes `pos + pat.len()` to land inside a multi-byte UTF-8 char,
    // Rust panics with: `byte index X is not a char boundary; it is inside '...'`.
    let cmd = "İİİnode --port 3🔥00";
    let result = std::panic::catch_unwind(|| detect_port_in_command(cmd));
    assert!(
        result.is_err(),
        "CRITICAL BUG: detect_port_in_command panics on non-char boundary when byte offsets shift"
    );
}

#[test]
fn test_port_detection_multi_space_limitation() {
    // When multiple spaces occur between `--port` and `3000`,
    // `after.chars().take_while(|c| c.is_ascii_digit())` fails because of leading whitespace.
    let cmd = "node server.js --port   3000";
    let port = detect_port_in_command(cmd);
    // Documenting that multi-space returns None due to missing trim_start()
    assert_eq!(
        port, None,
        "Multi-space returns None in current implementation"
    );

    // Single space works:
    assert_eq!(
        detect_port_in_command("node server.js --port 3000"),
        Some(3000)
    );
}

// =========================================================================
// 2. Command Tokenization & Quoting Stress Testing
// =========================================================================

#[test]
fn test_tokenize_deeply_nested_and_escaped_quotes() {
    // 1. Single quotes inside double quotes
    let (cmd, args) = tokenize_command(r#"bash -c "echo 'nested with spaces' && ls""#);
    assert_eq!(cmd, "bash");
    assert_eq!(args, vec!["-c", "echo 'nested with spaces' && ls"]);

    // 2. Double quotes inside single quotes
    let (cmd, args) = tokenize_command(r#"python -c 'print("hello world")'"#);
    assert_eq!(cmd, "python");
    assert_eq!(args, vec!["-c", r#"print("hello world")"#]);

    // 3. Escaped double quotes inside double quotes
    let (cmd, args) = tokenize_command(r#"sh -c "echo \"escaped quotes\"""#);
    assert_eq!(cmd, "sh");
    assert_eq!(args, vec!["-c", r#"echo "escaped quotes""#]);

    // 4. Consecutive spaces between arguments
    let (cmd, args) = tokenize_command("  node     --inspect    server.js    ");
    assert_eq!(cmd, "node");
    assert_eq!(args, vec!["--inspect", "server.js"]);

    // 5. Trailing backslash
    let (cmd, args) = tokenize_command("node server.js \\");
    assert_eq!(cmd, "node");
    assert_eq!(args, vec!["server.js"]);

    // 6. Unclosed single quote
    let (cmd, args) = tokenize_command("node 'unclosed single quote");
    assert_eq!(cmd, "node");
    assert_eq!(args, vec!["unclosed single quote"]);

    // 7. Unclosed double quote
    let (cmd, args) = tokenize_command(r#"node "unclosed double quote"#);
    assert_eq!(cmd, "node");
    assert_eq!(args, vec!["unclosed double quote"]);

    // 8. Empty input
    let (cmd, args) = tokenize_command("");
    assert_eq!(cmd, "");
    assert!(args.is_empty());

    // 9. Only whitespace
    let (cmd, args) = tokenize_command("    \t\t   ");
    assert_eq!(cmd, "");
    assert!(args.is_empty());
}

// =========================================================================
// 3. Package.json Stress & Adversarial Testing
// =========================================================================

#[test]
fn test_package_json_huge_scripts_block() {
    let mut scripts_json = String::from("{\n  \"name\": \"huge-repo\",\n  \"scripts\": {\n");

    for i in 0..1500 {
        scripts_json.push_str(&format!(
            "    \"dev:service_{}\": \"node server_{}.js --port {}\",\n",
            i,
            i,
            3000 + (i % 50000)
        ));
    }
    scripts_json.push_str("    \"dev:final\": \"vite --port 9999\"\n  }\n}");

    let temp = tempdir().expect("tempdir");
    let pkg_path = temp.path().join("package.json");
    fs::write(&pkg_path, &scripts_json).expect("write huge package.json");

    let services = parse_package_json(&pkg_path).expect("parse huge package.json");
    assert_eq!(services.len(), 1501);

    // Verify last service
    let final_svc = services.iter().find(|s| s.name == "dev:final").unwrap();
    assert_eq!(final_svc.target_port, Some(9999));
    assert_eq!(final_svc.subdomain, "dev-final");
}

#[test]
fn test_package_json_missing_or_corrupt_scripts() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("package.json");

    // 1. Missing "scripts" field
    fs::write(&path, r#"{"name": "no-scripts", "version": "1.0.0"}"#).expect("write");
    let svcs = parse_package_json(&path).expect("parse missing scripts");
    assert!(svcs.is_empty());

    // 2. "scripts" is null
    fs::write(&path, r#"{"name": "null-scripts", "scripts": null}"#).expect("write");
    let svcs = parse_package_json(&path).expect("parse null scripts");
    assert!(svcs.is_empty());

    // 3. "scripts" is an array
    fs::write(
        &path,
        r#"{"name": "array-scripts", "scripts": ["dev", "start"]}"#,
    )
    .expect("write");
    let svcs = parse_package_json(&path).expect("parse array scripts");
    assert!(svcs.is_empty());

    // 4. "scripts" is a string
    fs::write(&path, r#"{"name": "string-scripts", "scripts": "build"}"#).expect("write");
    let svcs = parse_package_json(&path).expect("parse string scripts");
    assert!(svcs.is_empty());

    // 5. "scripts" has non-string values
    let non_string_scripts = r#"{
      "scripts": {
        "dev": 12345,
        "server": true,
        "worker": { "command": "node" },
        "start": "node server.js --port 8000"
      }
    }"#;
    fs::write(&path, non_string_scripts).expect("write");
    let svcs = parse_package_json(&path).expect("parse non-string scripts");
    assert_eq!(svcs.len(), 1);
    assert_eq!(svcs[0].name, "start");
    assert_eq!(svcs[0].target_port, Some(8000));
}

#[test]
fn test_package_json_port_detection_variations() {
    assert_eq!(detect_port_in_script("PORT=3000 node index.js"), Some(3000));
    assert_eq!(detect_port_in_script("port=4000 node index.js"), Some(4000));
    assert_eq!(detect_port_in_script("next start --port 5000"), Some(5000));
    assert_eq!(detect_port_in_script("vite --port=5173"), Some(5173));
    assert_eq!(
        detect_port_in_script("node -e 'app.listen(8080)'"),
        Some(8080)
    );
    assert_eq!(detect_port_in_script("PORT=0 node index.js"), None);
    assert_eq!(detect_port_in_script("PORT=70000 node index.js"), None);
    assert_eq!(detect_port_in_script("no port here"), None);
}

// =========================================================================
// 4. Docker Compose Adversarial Testing
// =========================================================================

#[test]
fn test_compose_yaml_anchors_and_complex_ports() {
    let compose_content = r#"
version: '3.8'

x-common-env: &common-env
  LOG_LEVEL: debug
  ENV: development

services:
  web:
    image: nginx:alpine
    ports:
      - "127.0.0.1:8080:80/tcp"
    environment: *common-env

  api:
    image: python:alpine
    command: ["python", "-m", "http.server", "5000"]
    ports:
      - 5000
    environment:
      - DEBUG=1
      - TRACE=true

  range_svc:
    image: redis:alpine
    ports:
      - "8000-8005:8000-8005"

  missing_cmd_svc:
    image: alpine:latest

  long_syntax_ports:
    image: node:alpine
    ports:
      - target: 3000
        published: 3000
        protocol: tcp
"#;

    let temp = tempdir().expect("tempdir");
    let compose_path = temp.path().join("docker-compose.yml");
    fs::write(&compose_path, compose_content).expect("write compose");

    let services = parse_compose_file(&compose_path).expect("parse complex compose");
    assert_eq!(services.len(), 5);

    // web: 127.0.0.1:8080:80/tcp -> port 8080
    let web = services.iter().find(|s| s.name == "web").unwrap();
    assert_eq!(web.target_port, Some(8080));
    assert_eq!(web.env.get("LOG_LEVEL"), Some(&"debug".to_string()));

    // api: numeric port 5000
    let api = services.iter().find(|s| s.name == "api").unwrap();
    assert_eq!(api.target_port, Some(5000));
    assert_eq!(api.command, "python");
    assert_eq!(api.args, vec!["-m", "http.server", "5000"]);
    assert_eq!(api.env.get("DEBUG"), Some(&"1".to_string()));

    // missing command fallback
    let missing = services
        .iter()
        .find(|s| s.name == "missing_cmd_svc")
        .unwrap();
    assert_eq!(missing.command, "docker");
    assert_eq!(missing.args, vec!["compose", "up", "missing_cmd_svc"]);

    // long syntax port mapping
    let long_port = services
        .iter()
        .find(|s| s.name == "long_syntax_ports")
        .unwrap();
    assert_eq!(long_port.target_port, Some(3000));
}

#[test]
fn test_compose_non_mapping_services_or_root() {
    let temp = tempdir().expect("tempdir");
    let path = temp.path().join("compose.yaml");

    // 1. Services is a sequence instead of mapping
    fs::write(&path, "version: '3'\nservices:\n  - web\n  - api\n").expect("write");
    let res = parse_compose_file(&path);
    assert!(res.is_err());
    match res.unwrap_err() {
        DiscoveryError::MalformedConfig { message, .. } => {
            assert!(message.contains("'services' section must be a YAML mapping"));
        }
        other => panic!("Expected MalformedConfig, got {:?}", other),
    }

    // 2. Compose root is a scalar or list
    fs::write(&path, "- item1\n- item2\n").expect("write");
    let svcs = parse_compose_file(&path).expect("parse list compose");
    assert!(svcs.is_empty());
}

// =========================================================================
// 5. Extreme Inputs & Zero Panic Verification
// =========================================================================

#[test]
fn test_zero_byte_files() {
    let temp = tempdir().expect("tempdir");

    // 0-byte Procfile -> should return empty list without panicking
    let proc_path = temp.path().join("Procfile");
    File::create(&proc_path).expect("create empty procfile");
    let svcs = parse_procfile(&proc_path, "standard").expect("empty procfile parse");
    assert!(svcs.is_empty());

    // 0-byte package.json -> returns DiscoveryError::Json, no panic
    let pkg_path = temp.path().join("package.json");
    File::create(&pkg_path).expect("create empty package.json");
    let res_pkg = parse_package_json(&pkg_path);
    assert!(res_pkg.is_err());
    assert!(matches!(res_pkg.unwrap_err(), DiscoveryError::Json { .. }));

    // 0-byte compose.yaml -> returns empty list without panicking
    let compose_path = temp.path().join("compose.yaml");
    File::create(&compose_path).expect("create empty compose.yaml");
    let svcs_compose = parse_compose_file(&compose_path).expect("empty compose parse");
    assert!(svcs_compose.is_empty());
}

#[test]
fn test_binary_files_as_configs() {
    let temp = tempdir().expect("tempdir");

    // Binary garbage
    let garbage: Vec<u8> = (0..=255).cycle().take(1024).collect();

    // 1. Binary Procfile
    let proc_path = temp.path().join("Procfile");
    fs::write(&proc_path, &garbage).expect("write binary procfile");
    let res = parse_procfile(&proc_path, "standard");
    assert!(res.is_err());
    assert!(matches!(res.unwrap_err(), DiscoveryError::Io(_)));

    // 2. Binary package.json
    let pkg_path = temp.path().join("package.json");
    fs::write(&pkg_path, &garbage).expect("write binary pkg");
    let res_pkg = parse_package_json(&pkg_path);
    assert!(res_pkg.is_err());

    // 3. Binary compose.yaml
    let compose_path = temp.path().join("compose.yaml");
    fs::write(&compose_path, &garbage).expect("write binary compose");
    let res_comp = parse_compose_file(&compose_path);
    assert!(res_comp.is_err());
}

#[test]
fn test_deeply_nested_directory_and_non_existent_paths() {
    let temp = tempdir().expect("tempdir");

    // Build 25 levels of nested directories
    let mut deep_path = temp.path().to_path_buf();
    for i in 0..25 {
        deep_path = deep_path.join(format!("level_{}", i));
    }
    fs::create_dir_all(&deep_path).expect("create deep dirs");

    // Put a valid Procfile at the bottom
    let bottom_procfile = deep_path.join("Procfile");
    fs::write(&bottom_procfile, "deep_web: node index.js --port 3333\n")
        .expect("write deep procfile");

    let svcs = discover_services(&deep_path, None, None).expect("discover in deep path");
    assert_eq!(svcs.len(), 1);
    assert_eq!(svcs[0].name, "deep_web");
    assert_eq!(svcs[0].target_port, Some(3333));

    // Non-existent target directory
    let non_existent = temp.path().join("does_not_exist");
    let svcs_none = discover_services(&non_existent, None, None).expect("non-existent dir");
    assert!(svcs_none.is_empty());

    // Non-existent config override
    let bad_override = temp.path().join("missing.procfile");
    let res_override = discover_services(temp.path(), Some(&bad_override), None);
    assert!(res_override.is_err());
    assert!(matches!(
        res_override.unwrap_err(),
        DiscoveryError::NotFound(_)
    ));
}

#[test]
fn test_subdomain_sanitization_adversarial_matrix() {
    assert_eq!(sanitize_subdomain(""), "service");
    assert_eq!(sanitize_subdomain("---"), "service");
    assert_eq!(sanitize_subdomain("..."), "service");
    assert_eq!(sanitize_subdomain("___"), "service");
    assert_eq!(sanitize_subdomain("!@#$%^&*()_+="), "service");
    assert_eq!(sanitize_subdomain("🔥🚀🎉"), "service");
    assert_eq!(sanitize_subdomain("dev:::api"), "dev-api");
    assert_eq!(sanitize_subdomain("MyService_V2.0"), "myservice-v2-0");
    assert_eq!(
        sanitize_subdomain("---leading-and-trailing---"),
        "leading-and-trailing"
    );
    assert_eq!(sanitize_subdomain("UPPER_CASE"), "upper-case");
}
