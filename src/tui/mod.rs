pub mod app;
pub mod event;
pub mod terminal;
pub mod theme;
pub mod ui;

pub use app::{ActiveFocus, AppState, AutoScrollState, ModalState, ServiceViewData, TuiCommand};
pub use event::run_tui_loop;
pub use terminal::TerminalGuard;
pub use theme::{ColorCapability, SpinmuxPalette, Theme};
pub use ui::render;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::proxy::RunningProxy;
use crate::supervisor::Supervisor;

/// Launches the interactive Split-Screen Terminal Cockpit with fault-tolerant raw mode
pub async fn run_tui(
    supervisor: Arc<Supervisor>,
    proxy_server: RunningProxy,
    proxy_port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Snapshot registered services to initialize ServiceViewData list
    let svcs = supervisor.all_services().await;
    let mut service_views = Vec::with_capacity(svcs.len());

    for svc in &svcs {
        let status = svc.status();
        let pid = svc.current_pid.load(Ordering::Relaxed);
        let pid_opt = if pid > 0 { Some(pid) } else { None };
        let restart_count = svc.crash_count.load(Ordering::Relaxed);

        service_views.push(ServiceViewData {
            id: svc.definition.id.clone(),
            name: svc.definition.name.clone(),
            command: svc.definition.command.clone(),
            status,
            pid: pid_opt,
            port: Some(svc.assigned_port),
            subdomain: svc.definition.subdomain.clone(),
            restart_count,
            uptime: None,
            memory_bytes: 0,
            cpu_percentage: 0.0,
            buffer: svc.buffer.clone(),
        });
    }

    // 2. Initialize fault-tolerant TerminalGuard and Ratatui Terminal
    let (mut terminal, _guard) = TerminalGuard::init()?;

    // 3. Construct initial AppState
    let app = AppState::new(service_views, proxy_port);

    // 4. Run async event loop until exit signal
    let loop_result = run_tui_loop(&mut terminal, app, supervisor.clone()).await;

    // 5. Explicitly terminate background processes and proxy
    let _ = supervisor.stop_all().await;
    let _ = proxy_server.shutdown().await;

    loop_result
}
