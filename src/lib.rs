pub mod cli;
pub mod discovery;
pub mod proxy;
pub mod ringbuf;
pub mod supervisor;
pub mod tui;

pub use cli::Cli;
pub use discovery::{
    discover_services, DiscoveredService, DiscoveryError, ServiceDefinition, ServiceSource,
};
pub use proxy::{
    empty_body, extract_subdomain, extract_subdomain_from_headers, full_body, handle_request,
    MemoryRouteRegistry, ProxyBody, ProxyError, ProxyServer, RouteRegistry, RunningProxy,
    ServiceRoute,
};
pub use ringbuf::{
    strip_ansi, CircularLogBuffer, LogBuffer, LogEntry, LogStream, RingBuffer, StreamType,
};
pub use supervisor::{
    allocate_ephemeral_port, JobObject, ProcessStatus, SupervisedService, Supervisor,
    SupervisorError, WinJob,
};
pub use tui::{
    run_tui, ActiveFocus, AppState, AutoScrollState, ColorCapability, ModalState, ServiceViewData,
    SpinmuxPalette, TerminalGuard, Theme, TuiCommand,
};

/// Formats a list of discovered services for `--dry-run` display
pub fn format_dry_run_services(services: &[ServiceDefinition], proxy_port: u16) -> String {
    let mut out = String::new();
    out.push_str("====================================================================\n");
    out.push_str("                        SPINMUX SERVICE DISCOVERY                   \n");
    out.push_str("====================================================================\n");

    if services.is_empty() {
        out.push_str("No runnable services discovered.\n");
        return out;
    }

    out.push_str(&format!(
        "Discovered {} service(s) [Reverse Proxy: http://localhost:{}]:\n\n",
        services.len(),
        proxy_port
    ));

    for (idx, svc) in services.iter().enumerate() {
        out.push_str(&format!("{}. [{}] {}\n", idx + 1, svc.id, svc.name));
        out.push_str(&format!("   Source:      {}\n", svc.source));
        let full_cmd = if svc.args.is_empty() {
            svc.command.clone()
        } else {
            format!("{} {}", svc.command, svc.args.join(" "))
        };
        out.push_str(&format!("   Command:     {}\n", full_cmd));
        out.push_str(&format!("   Working Dir: {}\n", svc.working_dir.display()));
        out.push_str(&format!(
            "   Route:       http://{}.localhost:{}\n",
            svc.subdomain, proxy_port
        ));
        if let Some(port) = svc.target_port {
            out.push_str(&format!("   Static Port: {}\n", port));
        } else {
            out.push_str("   Port:        Dynamic (ephemeral allocation)\n");
        }
        out.push_str(&format!("   HTTP Service: {}\n", svc.is_http));
        out.push('\n');
    }

    out.push_str("====================================================================\n");
    out
}
