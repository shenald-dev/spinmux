use clap::Parser;
use spinmux::cli::Cli;
use spinmux::discovery::{ServiceDefinition, ServiceSource};
use spinmux::format_dry_run_services;
use std::collections::HashMap;
use std::path::PathBuf;

#[test]
fn test_cli_flags_parsing() {
    let args = [
        "spinmux",
        "--dir",
        "./fixtures/node_project",
        "--port",
        "9000",
        "--headless",
        "--dry-run",
    ];

    let cli = Cli::try_parse_from(args).expect("parse cli flags");
    assert_eq!(cli.dir, PathBuf::from("./fixtures/node_project"));
    assert_eq!(cli.port, 9000);
    assert!(cli.headless);
    assert!(cli.dry_run);
    assert!(cli.services.is_empty());
    assert!(cli.config.is_none());
}

#[test]
fn test_cli_default_flags() {
    let args = ["spinmux"];

    let cli = Cli::try_parse_from(args).expect("parse default cli");
    assert_eq!(cli.dir, PathBuf::from("."));
    assert_eq!(cli.port, 8080);
    assert!(!cli.headless);
    assert!(!cli.dry_run);
    assert!(cli.services.is_empty());
    assert!(cli.config.is_none());
}

#[test]
fn test_cli_services_filter_comma_separated() {
    let args = ["spinmux", "--services", "web,api,worker"];

    let cli = Cli::try_parse_from(args).expect("parse comma-separated services");
    assert_eq!(cli.services, vec!["web", "api", "worker"]);
}

#[test]
fn test_cli_services_filter_multi_args() {
    let args = ["spinmux", "--services", "web", "api", "worker"];

    let cli = Cli::try_parse_from(args).expect("parse multi-arg services");
    assert_eq!(cli.services, vec!["web", "api", "worker"]);
}

#[test]
fn test_cli_explicit_config_flag() {
    let args = ["spinmux", "--config", "./custom/Procfile.dev"];

    let cli = Cli::try_parse_from(args).expect("parse explicit config flag");
    assert_eq!(cli.config, Some(PathBuf::from("./custom/Procfile.dev")));
}

#[test]
fn test_dry_run_formatting() {
    let services = vec![
        ServiceDefinition {
            id: "web".to_string(),
            name: "web".to_string(),
            command: "npm.cmd".to_string(),
            args: vec!["run".to_string(), "dev".to_string()],
            working_dir: PathBuf::from("./app"),
            env: HashMap::new(),
            source: ServiceSource::Procfile {
                path: PathBuf::from("Procfile"),
                variant: "standard".to_string(),
            },
            target_port: None,
            is_http: true,
            subdomain: "web".to_string(),
        },
        ServiceDefinition {
            id: "api".to_string(),
            name: "api".to_string(),
            command: "python".to_string(),
            args: vec![
                "-m".to_string(),
                "http.server".to_string(),
                "8000".to_string(),
            ],
            working_dir: PathBuf::from("./api"),
            env: HashMap::new(),
            source: ServiceSource::PackageJson {
                path: PathBuf::from("package.json"),
                script: "api".to_string(),
            },
            target_port: Some(8000),
            is_http: true,
            subdomain: "api".to_string(),
        },
    ];

    let dry_run_str = format_dry_run_services(&services, 8080);
    assert!(dry_run_str.contains("SPINMUX SERVICE DISCOVERY"));
    assert!(dry_run_str.contains("Discovered 2 service(s)"));
    assert!(dry_run_str.contains("http://web.localhost:8080"));
    assert!(dry_run_str.contains("http://api.localhost:8080"));
    assert!(dry_run_str.contains("Static Port: 8000"));
    assert!(dry_run_str.contains("Dynamic (ephemeral allocation)"));
}
