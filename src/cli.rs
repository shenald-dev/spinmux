use clap::Parser;
use std::path::PathBuf;

/// Autonomous multi-service development terminal cockpit
#[derive(Parser, Debug, Clone)]
#[command(
    name = "spinmux",
    author = "shenald-dev",
    version = "0.1.0",
    about = "Autonomous multi-service development cockpit with zero-orphan process supervision, virtual localhost reverse proxy, and electric cyan/violet TUI",
    long_about = None
)]
pub struct Cli {
    /// Target working directory to discover services from (defaults to current dir)
    #[arg(short, long, default_value = ".")]
    pub dir: PathBuf,

    /// Explicit path to a service configuration file (Procfile, package.json, or compose.yaml)
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Filter specific services to run (can be repeated or comma-separated)
    #[arg(short, long, value_delimiter = ',', num_args = 1..)]
    pub services: Vec<String>,

    /// Port for the virtual localhost reverse proxy
    #[arg(short, long, default_value_t = 8080)]
    pub port: u16,

    /// Run in headless mode without interactive TUI (ideal for CI, scripting, or background supervision)
    #[arg(long, default_value_t = false)]
    pub headless: bool,

    /// Discover and print all resolved services and configuration, then exit immediately
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}
