use spinmux::discovery::{
    compose::{parse_compose_file, parse_compose_port_entry},
    discover_services,
    package_json::{parse_package_json, parse_package_json_content},
    procfile::{parse_procfile, parse_procfile_content, tokenize_command},
    sanitize_subdomain, DiscoveryError, ServiceSource,
};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

#[test]
fn test_parse_valid_package_json() {
    let fixture_path = Path::new("tests/fixtures/package_json/package.json");
    assert!(fixture_path.exists(), "Fixture must exist");

    let services = parse_package_json(fixture_path).expect("parse package.json");

    // Must discover dev, server, worker, dev:api
    // Must exclude test, build, lint
    let names: Vec<String> = services.iter().map(|s| s.name.clone()).collect();
    assert!(names.contains(&"dev".to_string()));
    assert!(names.contains(&"server".to_string()));
    assert!(names.contains(&"worker".to_string()));
    assert!(names.contains(&"dev:api".to_string()));
    assert!(!names.contains(&"test".to_string()));
    assert!(!names.contains(&"build".to_string()));
    assert!(!names.contains(&"lint".to_string()));

    // Verify subdomains
    let dev_api = services.iter().find(|s| s.name == "dev:api").unwrap();
    assert_eq!(dev_api.subdomain, "dev-api");
    assert_eq!(dev_api.target_port, Some(4000));

    let dev_svc = services.iter().find(|s| s.name == "dev").unwrap();
    assert_eq!(dev_svc.subdomain, "dev");
    assert_eq!(dev_svc.target_port, Some(3000));

    let server_svc = services.iter().find(|s| s.name == "server").unwrap();
    assert_eq!(server_svc.target_port, Some(5000));
    assert!(server_svc.is_http);

    // Verify Windows / Unix command
    if cfg!(windows) {
        assert_eq!(dev_svc.command, "npm.cmd");
    } else {
        assert_eq!(dev_svc.command, "npm");
    }
    assert_eq!(dev_svc.args, vec!["run", "dev"]);
}

#[test]
fn test_parse_valid_procfile() {
    let fixture_path = Path::new("tests/fixtures/procfile/Procfile");
    assert!(fixture_path.exists(), "Fixture must exist");

    let services = parse_procfile(fixture_path, "standard").expect("parse Procfile");

    assert_eq!(services.len(), 3);

    let web = services.iter().find(|s| s.name == "web").unwrap();
    assert_eq!(web.subdomain, "web");
    assert!(web.is_http);

    let worker = services.iter().find(|s| s.name == "worker").unwrap();
    assert_eq!(worker.command, "python");
    assert_eq!(worker.args, vec!["worker.py"]);
    assert_eq!(worker.subdomain, "worker");

    let api = services.iter().find(|s| s.name == "api").unwrap();
    assert_eq!(api.command, "go");
    assert_eq!(api.args, vec!["run", "main.go", "-p", "8000"]);
    assert_eq!(api.target_port, Some(8000));
}

#[test]
fn test_parse_valid_docker_compose() {
    let fixture_path = Path::new("tests/fixtures/compose/docker-compose.yml");
    assert!(fixture_path.exists(), "Fixture must exist");

    let services = parse_compose_file(fixture_path).expect("parse docker-compose");

    assert_eq!(services.len(), 4);

    let web = services.iter().find(|s| s.name == "web").unwrap();
    assert_eq!(web.command, "npm");
    assert_eq!(web.args, vec!["run", "dev"]);
    assert_eq!(web.target_port, Some(3000));
    assert_eq!(web.env.get("NODE_ENV"), Some(&"development".to_string()));

    let api = services.iter().find(|s| s.name == "api").unwrap();
    assert_eq!(api.command, "python");
    assert_eq!(api.args, vec!["-m", "http.server", "5000"]);
    assert_eq!(api.target_port, Some(5000));
    assert_eq!(api.env.get("PORT"), Some(&"5000".to_string()));

    let db = services.iter().find(|s| s.name == "db").unwrap();
    assert_eq!(db.command, "docker");
    assert_eq!(db.args, vec!["compose", "up", "db"]);
    assert_eq!(db.target_port, Some(5432));

    let redis = services.iter().find(|s| s.name == "redis").unwrap();
    assert_eq!(redis.target_port, Some(6379));
}

#[test]
fn test_precedence_and_priority_order() {
    let temp_dir = tempdir().expect("create temp dir");
    let root = temp_dir.path();

    // 1. Write Procfile with 'web' service
    let procfile_content = "web: python -m http.server $PORT\nworker: python worker.py\n";
    fs::write(root.join("Procfile"), procfile_content).expect("write Procfile");

    // 2. Write package.json with 'web' (collision) and 'dev:frontend'
    let pkg_content = r#"{
      "scripts": {
        "web": "vite --port 3000",
        "dev:frontend": "vite --port 3001"
      }
    }"#;
    fs::write(root.join("package.json"), pkg_content).expect("write package.json");

    // 3. Write compose.yaml with 'web' (collision) and 'redis'
    let compose_content = r#"
version: '3.8'
services:
  web:
    image: node:alpine
    ports:
      - "4000:4000"
  redis:
    image: redis:alpine
    ports:
      - "6379:6379"
"#;
    fs::write(root.join("compose.yaml"), compose_content).expect("write compose.yaml");

    // Discover all
    let discovered = discover_services(root, None, None).expect("discover services");

    // Precedence: Procfile 'web' must hold primary 'web' ID
    let primary_web = discovered
        .iter()
        .find(|s| s.id == "web")
        .expect("primary web");
    assert!(
        matches!(primary_web.source, ServiceSource::Procfile { .. }),
        "Primary 'web' must come from Procfile due to higher precedence"
    );

    // Colliding 'web' from package.json must be namespaced to 'npm:web'
    let npm_web = discovered
        .iter()
        .find(|s| s.id == "npm:web")
        .expect("namespaced npm web");
    assert!(matches!(npm_web.source, ServiceSource::PackageJson { .. }));
    assert_eq!(npm_web.subdomain, "npm-web");

    // Colliding 'web' from compose.yaml must be namespaced to 'compose:web'
    let compose_web = discovered
        .iter()
        .find(|s| s.id == "compose:web")
        .expect("namespaced compose web");
    assert!(matches!(
        compose_web.source,
        ServiceSource::DockerCompose { .. }
    ));
    assert_eq!(compose_web.subdomain, "compose-web");

    // Non-colliding services must be present
    assert!(discovered.iter().any(|s| s.id == "worker"));
    assert!(discovered.iter().any(|s| s.id == "dev:frontend"));
    assert!(discovered.iter().any(|s| s.id == "redis"));
}

#[test]
fn test_cli_config_override() {
    let temp_dir = tempdir().expect("create temp dir");
    let custom_file = temp_dir.path().join("custom.procfile");
    fs::write(&custom_file, "custom_svc: node app.js --port 9090\n").expect("write custom config");

    let services = discover_services(temp_dir.path(), Some(&custom_file), None)
        .expect("discover with override");

    assert_eq!(services.len(), 1);
    assert_eq!(services[0].id, "custom_svc");
    assert_eq!(services[0].target_port, Some(9090));
    assert!(matches!(
        services[0].source,
        ServiceSource::CliOverride { .. }
    ));
}

#[test]
fn test_service_filtering() {
    let fixture_dir = Path::new("tests/fixtures/procfile");
    let filter = vec!["api".to_string(), "worker".to_string()];

    let services = discover_services(fixture_dir, None, Some(&filter)).expect("filtered discovery");

    assert_eq!(services.len(), 2);
    let ids: Vec<String> = services.iter().map(|s| s.id.clone()).collect();
    assert!(ids.contains(&"api".to_string()));
    assert!(ids.contains(&"worker".to_string()));
    assert!(!ids.contains(&"web".to_string()));
}

#[test]
fn test_malformed_json_resilience() {
    let fixture_path = Path::new("tests/fixtures/malformed/corrupted.json");
    let res = parse_package_json(fixture_path);
    assert!(res.is_err(), "Must return error on corrupted JSON");
    match res.unwrap_err() {
        DiscoveryError::Json { .. } => {}
        other => panic!("Expected DiscoveryError::Json, got: {:?}", other),
    }
}

#[test]
fn test_malformed_yaml_resilience() {
    let fixture_path = Path::new("tests/fixtures/malformed/invalid.yml");
    let res = parse_compose_file(fixture_path);
    assert!(res.is_err(), "Must return error on corrupted YAML");
    match res.unwrap_err() {
        DiscoveryError::Yaml { .. } => {}
        other => panic!("Expected DiscoveryError::Yaml, got: {:?}", other),
    }
}

#[test]
fn test_procfile_missing_colon() {
    let temp_dir = tempdir().expect("create temp dir");
    let broken_file = temp_dir.path().join("Procfile");
    fs::write(&broken_file, "this line has no colon separator\n").expect("write broken Procfile");

    let res = parse_procfile(&broken_file, "test");
    assert!(res.is_err(), "Must return error when colon is missing");
    match res.unwrap_err() {
        DiscoveryError::MalformedConfig { message, .. } => {
            assert!(message.contains("missing colon delimiter"));
        }
        other => panic!("Expected DiscoveryError::MalformedConfig, got: {:?}", other),
    }
}

#[test]
fn test_subdomain_sanitization() {
    assert_eq!(sanitize_subdomain("dev:api"), "dev-api");
    assert_eq!(sanitize_subdomain("MY_COOL_SERVICE"), "my-cool-service");
    assert_eq!(
        sanitize_subdomain("---special...chars///---"),
        "special-chars"
    );
    assert_eq!(sanitize_subdomain("123-frontend"), "123-frontend");
    assert_eq!(sanitize_subdomain(""), "service");
    assert_eq!(sanitize_subdomain("!@#$%^&*()"), "service");
}

#[test]
fn test_tokenize_command_quoting_and_escapes() {
    let (prog, args) = tokenize_command(r#"node -e "console.log('hello world')""#);
    assert_eq!(prog, "node");
    assert_eq!(args, vec!["-e", "console.log('hello world')"]);

    let (prog2, args2) = tokenize_command("python -m http.server 8080");
    assert_eq!(prog2, "python");
    assert_eq!(args2, vec!["-m", "http.server", "8080"]);

    let (prog3, args3) = tokenize_command(r#"echo "nested \"quotes\" here""#);
    assert_eq!(prog3, "echo");
    assert_eq!(args3, vec![r#"nested "quotes" here"#]);
}

#[test]
fn test_procfile_windows_backslash_paths() {
    let (prog1, args1) = tokenize_command(r#".\bin\server.exe --flag"#);
    assert_eq!(prog1, r#".\bin\server.exe"#);
    assert_eq!(args1, vec!["--flag"]);

    let (prog2, args2) = tokenize_command(r#"C:\dir\file.exe --port 3000"#);
    assert_eq!(prog2, r#"C:\dir\file.exe"#);
    assert_eq!(args2, vec!["--port", "3000"]);

    let (prog3, args3) = tokenize_command(r#"bin\worker.exe"#);
    assert_eq!(prog3, r#"bin\worker.exe"#);
    assert!(args3.is_empty());

    let content = "web: .\\bin\\server.exe --flag\napi: C:\\tools\\server.exe -p 8080\nworker: bin\\worker.exe\n";
    let dummy_path = Path::new("Procfile");
    let services = parse_procfile_content(content, dummy_path, "standard")
        .expect("parse procfile with windows paths");

    assert_eq!(services.len(), 3);

    let web = services.iter().find(|s| s.name == "web").unwrap();
    assert_eq!(web.command, r#".\bin\server.exe"#);
    assert_eq!(web.args, vec!["--flag"]);

    let api = services.iter().find(|s| s.name == "api").unwrap();
    assert_eq!(api.command, r#"C:\tools\server.exe"#);
    assert_eq!(api.args, vec!["-p", "8080"]);
    assert_eq!(api.target_port, Some(8080));

    let worker = services.iter().find(|s| s.name == "worker").unwrap();
    assert_eq!(worker.command, r#"bin\worker.exe"#);
    assert!(worker.args.is_empty());
}

#[test]
fn test_package_json_utf8_bom() {
    let bom_content =
        "\u{FEFF}{\"scripts\": {\"dev\": \"vite --port 3000\", \"server\": \"node server.js\"}}";
    let dummy_path = Path::new("package.json");
    let services = parse_package_json_content(bom_content, dummy_path)
        .expect("parse package.json with UTF-8 BOM");

    assert_eq!(services.len(), 2);
    let dev = services.iter().find(|s| s.name == "dev").unwrap();
    assert_eq!(dev.target_port, Some(3000));
    assert_eq!(dev.subdomain, "dev");

    // Also test file on disk written with raw UTF-8 BOM bytes
    let temp = tempdir().expect("create temp dir");
    let pkg_path = temp.path().join("package.json");
    let mut raw_bytes = vec![0xEF, 0xBB, 0xBF];
    raw_bytes.extend_from_slice(br#"{"scripts": {"dev": "vite --port 4000"}}"#);
    fs::write(&pkg_path, raw_bytes).expect("write package.json with raw BOM");

    let disk_services =
        parse_package_json(&pkg_path).expect("parse package.json with BOM from disk");
    assert_eq!(disk_services.len(), 1);
    assert_eq!(disk_services[0].name, "dev");
    assert_eq!(disk_services[0].target_port, Some(4000));

    let discovered =
        discover_services(temp.path(), None, None).expect("discover services with BOM");
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].name, "dev");
}

#[test]
fn test_subdomain_truncation_rfc1123() {
    // 1. Long name (> 63 characters)
    let long_name =
        "my-super-awesome-front-end-microservice-development-server-running-locally-for-testing";
    let sanitized = sanitize_subdomain(long_name);
    assert!(
        sanitized.len() <= 63,
        "Subdomain must not exceed 63 characters per RFC 1123"
    );
    assert!(
        !sanitized.ends_with('-'),
        "Subdomain must not end with a hyphen"
    );
    assert!(
        !sanitized.starts_with('-'),
        "Subdomain must not start with a hyphen"
    );

    // 2. Exact 63 characters
    let exact_63 = "a".repeat(63);
    assert_eq!(sanitize_subdomain(&exact_63).len(), 63);

    // 3. 64 characters with a hyphen at the 63rd boundary position
    let boundary_hyphen = format!("{}-bbbb", "a".repeat(62));
    let sanitized_boundary = sanitize_subdomain(&boundary_hyphen);
    assert!(sanitized_boundary.len() <= 63);
    assert_eq!(sanitized_boundary.len(), 62);
    assert!(!sanitized_boundary.ends_with('-'));
}

#[test]
fn test_subdomain_collision_disambiguation() {
    let temp = tempdir().expect("create temp dir");
    let pkg_path = temp.path().join("package.json");

    // "dev:app", "dev-app", and "dev_app" all sanitize to base subdomain "dev-app"
    let pkg_content = r#"{
      "scripts": {
        "dev:app": "node server1.js",
        "dev-app": "node server2.js",
        "dev_app": "node server3.js"
      }
    }"#;
    fs::write(&pkg_path, pkg_content).expect("write package.json with colliding scripts");

    let services = discover_services(temp.path(), None, None).expect("discover services");
    assert_eq!(services.len(), 3);

    let subdomains: Vec<String> = services.iter().map(|s| s.subdomain.clone()).collect();
    assert_eq!(subdomains[0], "dev-app");
    assert_eq!(subdomains[1], "dev-app-2");
    assert_eq!(subdomains[2], "dev-app-3");

    // Verify all subdomains are globally distinct
    let unique: std::collections::HashSet<_> = subdomains.iter().collect();
    assert_eq!(unique.len(), 3, "All assigned subdomains must be unique");
}

#[test]
fn test_empty_procfile_fixture_handling() {
    let fixture_path = Path::new("tests/fixtures/malformed/empty_procfile");
    assert!(fixture_path.exists(), "empty_procfile fixture must exist");

    let services = parse_procfile(fixture_path, "standard").expect("parse empty procfile");
    assert!(
        services.is_empty(),
        "Empty Procfile with comments only must discover 0 services"
    );
}

#[test]
fn test_compose_port_checked_cast_overflow() {
    // Port exceeding u16::MAX (65535) must be safely rejected (return None)
    let overflow_val = serde_yaml::Value::Number(serde_yaml::Number::from(70000u64));
    assert_eq!(parse_compose_port_entry(&overflow_val), None);

    let valid_max = serde_yaml::Value::Number(serde_yaml::Number::from(65535u64));
    assert_eq!(parse_compose_port_entry(&valid_max), Some(65535));

    let standard_port = serde_yaml::Value::Number(serde_yaml::Number::from(8080u64));
    assert_eq!(parse_compose_port_entry(&standard_port), Some(8080));
}
