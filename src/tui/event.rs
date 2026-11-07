use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use super::app::{AppState, TuiCommand};
use super::ui;
use crate::supervisor::Supervisor;

/// Event types handled by the async TUI event loop
#[derive(Debug)]
pub enum AppEvent {
    Crossterm(Event),
    StatusTick,
    TelemetryTick,
    RenderTick,
}

/// Spawns a background thread reading Crossterm events and forwarding them via an unbounded Tokio mpsc channel
pub fn spawn_input_reader(running: Arc<AtomicBool>) -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();

    std::thread::Builder::new()
        .name("spinmux-input-reader".to_string())
        .spawn(move || {
            while running.load(Ordering::Relaxed) {
                // Poll for 20ms to allow responsive shutdown when running flag turns false
                match event::poll(Duration::from_millis(20)) {
                    Ok(true) => {
                        if let Ok(evt) = event::read() {
                            if tx.send(evt).is_err() {
                                break;
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        })
        .expect("Failed to spawn input reader thread");

    rx
}

/// Runs the non-blocking async event loop interleaving keyboard input, status ticks, and 60 FPS renders
pub async fn run_tui_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: AppState,
    supervisor: Arc<Supervisor>,
) -> Result<(), Box<dyn std::error::Error>> {
    let running_flag = Arc::new(AtomicBool::new(true));
    let mut input_rx = spawn_input_reader(running_flag.clone());

    let mut tick_16ms = tokio::time::interval(Duration::from_millis(16));
    tick_16ms.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut status_100ms = tokio::time::interval(Duration::from_millis(100));
    status_100ms.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut telemetry_1s = tokio::time::interval(Duration::from_millis(1000));
    telemetry_1s.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut last_selected_seq: u64 = 0;

    // Initial draw
    terminal.draw(|frame| {
        ui::render(frame, &mut app);
    })?;
    app.is_dirty = false;

    while app.running {
        tokio::select! {
            // 1. Crossterm Keyboard & Mouse Input Events
            Some(evt) = input_rx.recv() => {
                match evt {
                    Event::Key(key_event) => {
                        // Filter out key release events
                        if key_event.kind == KeyEventKind::Press {
                            if let Some(cmd) = app.handle_key_event(key_event) {
                                match cmd {
                                    TuiCommand::Quit => {
                                        app.running = false;
                                        break;
                                    }
                                    TuiCommand::RestartService(id) => {
                                        let sup = supervisor.clone();
                                        tokio::spawn(async move {
                                            let _ = sup.restart_service(&id).await;
                                        });
                                    }
                                    TuiCommand::StopService(id) => {
                                        let sup = supervisor.clone();
                                        tokio::spawn(async move {
                                            let _ = sup.stop_service(&id).await;
                                        });
                                    }
                                    TuiCommand::StartService(id) => {
                                        let sup = supervisor.clone();
                                        tokio::spawn(async move {
                                            let _ = sup.start_service(&id).await;
                                        });
                                    }
                                    TuiCommand::StartAll => {
                                        let sup = supervisor.clone();
                                        tokio::spawn(async move {
                                            let _ = sup.start_all().await;
                                        });
                                    }
                                    TuiCommand::StopAll => {
                                        let sup = supervisor.clone();
                                        tokio::spawn(async move {
                                            let _ = sup.stop_all().await;
                                        });
                                    }
                                    TuiCommand::ClearLogs(id) => {
                                        let sup = supervisor.clone();
                                        tokio::spawn(async move {
                                            if let Some(buf) = sup.get_buffer(&id).await {
                                                buf.clear();
                                            }
                                        });
                                    }
                                }
                            }
                            app.is_dirty = true;
                        }
                    }
                    Event::Resize(..) => {
                        app.is_dirty = true;
                    }
                    _ => {}
                }
            }

            // 2. Process Supervisor Status Synchronization (10 Hz)
            _ = status_100ms.tick() => {
                let svcs = supervisor.all_services().await;
                for svc in svcs {
                    let status = svc.status();
                    let pid = svc.current_pid.load(Ordering::Relaxed);
                    let pid_opt = if pid > 0 { Some(pid) } else { None };
                    let crash_count = svc.crash_count.load(Ordering::Relaxed);
                    app.update_service_status(&svc.definition.id, status, pid_opt, Some(crash_count));
                }

                // Check for new logs in selected service ring buffer
                if let Some(svc) = app.selected_service() {
                    let current_seq = svc.buffer.head_sequence();
                    if current_seq != last_selected_seq {
                        last_selected_seq = current_seq;
                        app.is_dirty = true;
                    }
                }
            }

            // 3. System Telemetry & Banner Expiration Tick (1 Hz)
            _ = telemetry_1s.tick() => {
                app.update_telemetry();
                app.is_dirty = true;
            }

            // 4. Coalesced 60 FPS Render Boundary (16ms)
            _ = tick_16ms.tick() => {
                if !app.running {
                    break;
                }

                // Sub-15ms delta rendering: draw only when state is dirty
                if app.is_dirty {
                    let start = Instant::now();
                    terminal.draw(|frame| {
                        ui::render(frame, &mut app);
                    })?;
                    app.last_render_latency = start.elapsed();
                    app.is_dirty = false;
                }
            }

            // 5. Ctrl+C Terminal Signal Handling
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl+C signal received, initiating graceful TUI shutdown.");
                app.running = false;
                break;
            }
        }
    }

    // Terminate input reader thread
    running_flag.store(false, Ordering::Relaxed);

    Ok(())
}
