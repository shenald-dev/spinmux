use std::process;
use std::sync::Arc;

use clap::Parser;
use spinmux::cli::Cli;
use spinmux::discovery::discover_services;
use spinmux::format_dry_run_services;
use spinmux::proxy::ProxyServer;
use spinmux::supervisor::Supervisor;
use spinmux::tui::run_tui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.headless {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .init();
    }

    let filter = if cli.services.is_empty() {
        None
    } else {
        Some(cli.services.as_slice())
    };

    let discovered = match discover_services(&cli.dir, cli.config.as_deref(), filter) {
        Ok(services) => services,
        Err(err) => {
            eprintln!("Error discovering services: {err}");
            process::exit(1);
        }
    };

    if cli.dry_run {
        print!("{}", format_dry_run_services(&discovered, cli.port));
        return Ok(());
    }

    if discovered.is_empty() {
        eprintln!(
            "No runnable services discovered in directory '{}'.\nCheck that a package.json, Procfile, or docker-compose.yml exists.",
            cli.dir.display()
        );
        process::exit(1);
    }

    // Initialize supervisor with Windows Job Object encapsulation
    let supervisor = Arc::new(Supervisor::new()?);

    // Register all auto-discovered services
    for svc in &discovered {
        supervisor.add_service(svc.clone(), None).await?;
    }

    // Start all services
    supervisor.start_all().await?;

    // Spawn virtual localhost reverse proxy
    let proxy = ProxyServer::spawn(cli.port, supervisor.clone()).await?;

    if cli.headless {
        println!(
            "[spinmux] Running in headless mode with {} service(s) on reverse proxy port {}.",
            discovered.len(),
            cli.port
        );
        for svc in &discovered {
            println!(
                "  -> Service '{}' ({}) routing to http://{}.localhost:{}",
                svc.name, svc.id, svc.subdomain, cli.port
            );
        }
        println!("[spinmux] All services active. Press Ctrl+C to terminate.");
        tokio::signal::ctrl_c().await?;
        println!("[spinmux] Terminating all supervised services...");
        supervisor.stop_all().await?;
        proxy.shutdown().await?;
        println!("[spinmux] Clean shutdown complete.");
    } else {
        // Launch interactive Split-Screen Ratatui TUI Cockpit
        if let Err(err) = run_tui(supervisor, proxy, cli.port).await {
            eprintln!("TUI cockpit error: {err}");
            process::exit(1);
        }
    }

    Ok(())
}
