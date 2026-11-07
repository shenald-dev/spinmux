use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use ratatui::Terminal;

use spinmux::ringbuf::{LogEntry, LogStream, RingBuffer};
use spinmux::supervisor::ProcessStatus;
use spinmux::tui::app::{
    ActiveFocus, AppState, AutoScrollState, ModalState, ServiceViewData, TuiCommand,
};
use spinmux::tui::terminal::TerminalGuard;
use spinmux::tui::theme::{resolve_color, ColorCapability, SpinmuxPalette, Theme};
use spinmux::tui::ui::{parse_ansi_to_spans, render};

fn make_key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    }
}

fn create_sample_services() -> Vec<ServiceViewData> {
    let buf1 = Arc::new(RingBuffer::new(100, 1024 * 1024));
    buf1.push(LogEntry::new(
        "web",
        LogStream::Stdout,
        "ready on http://localhost:5173",
    ));
    buf1.push(LogEntry::new("web", LogStream::Stdout, "compiled in 150ms"));

    let buf2 = Arc::new(RingBuffer::new(100, 1024 * 1024));
    buf2.push(LogEntry::new(
        "api",
        LogStream::Stdout,
        "API server listening on :8000",
    ));
    buf2.push(LogEntry::new(
        "api",
        LogStream::Stderr,
        "WARN: Cache miss on /health",
    ));

    vec![
        ServiceViewData {
            id: "web".to_string(),
            name: "web".to_string(),
            command: "npm run dev".to_string(),
            status: ProcessStatus::Running(1234),
            pid: Some(1234),
            port: Some(5173),
            subdomain: "web".to_string(),
            restart_count: 0,
            uptime: Some(Duration::from_secs(42)),
            memory_bytes: 1024 * 1024 * 50,
            cpu_percentage: 1.2,
            buffer: buf1,
        },
        ServiceViewData {
            id: "api".to_string(),
            name: "api".to_string(),
            command: "python app.py".to_string(),
            status: ProcessStatus::Running(5678),
            pid: Some(5678),
            port: Some(8000),
            subdomain: "api".to_string(),
            restart_count: 1,
            uptime: Some(Duration::from_secs(30)),
            memory_bytes: 1024 * 1024 * 80,
            cpu_percentage: 2.5,
            buffer: buf2,
        },
        ServiceViewData {
            id: "worker".to_string(),
            name: "worker".to_string(),
            command: "cargo run".to_string(),
            status: ProcessStatus::Stopped(Some(0)),
            pid: None,
            port: None,
            subdomain: "worker".to_string(),
            restart_count: 3,
            uptime: None,
            memory_bytes: 0,
            cpu_percentage: 0.0,
            buffer: Arc::new(RingBuffer::new(100, 1024 * 1024)),
        },
    ]
}

#[test]
fn test_theme_palette_and_fallback_mapping() {
    let true_theme = Theme::new(ColorCapability::TrueColor);
    assert_eq!(true_theme.electric_cyan(), SpinmuxPalette::ELECTRIC_CYAN);
    assert_eq!(true_theme.neon_violet(), SpinmuxPalette::NEON_VIOLET);
    assert_eq!(
        true_theme.status_running_color(),
        SpinmuxPalette::STATUS_RUNNING
    );

    let ansi256_theme = Theme::new(ColorCapability::Ansi256);
    assert_eq!(ansi256_theme.electric_cyan(), Color::Indexed(51));
    assert_eq!(ansi256_theme.neon_violet(), Color::Indexed(129));
    assert_eq!(ansi256_theme.status_running_color(), Color::Indexed(48));

    let ansi16_theme = Theme::new(ColorCapability::Ansi16);
    assert_eq!(ansi16_theme.electric_cyan(), Color::Cyan);
    assert_eq!(ansi16_theme.neon_violet(), Color::Magenta);
    assert_eq!(ansi16_theme.status_running_color(), Color::Green);

    // Test resolve_color directly
    let resolved = resolve_color(
        Color::Rgb(10, 20, 30),
        42,
        Color::Blue,
        ColorCapability::Ansi256,
    );
    assert_eq!(resolved, Color::Indexed(42));
}

#[test]
fn test_theme_status_glyphs_and_colors() {
    let theme = Theme::default();
    assert_eq!(theme.status_symbol(&ProcessStatus::Running(100)), "●");
    assert_eq!(theme.status_symbol(&ProcessStatus::Starting), "◌");
    assert_eq!(theme.status_symbol(&ProcessStatus::Stopped(Some(1))), "✕");
    assert_eq!(theme.status_symbol(&ProcessStatus::CrashLoopBackOff), "✕");
}

#[test]
fn test_app_state_navigation_and_wrapping() {
    let services = create_sample_services();
    let mut app = AppState::new(services, 8080);

    assert_eq!(app.selected_index, 0);
    assert_eq!(app.selected_service_id(), Some("web"));

    // Navigate down
    app.select_next();
    assert_eq!(app.selected_index, 1);
    assert_eq!(app.selected_service_id(), Some("api"));

    app.select_next();
    assert_eq!(app.selected_index, 2);
    assert_eq!(app.selected_service_id(), Some("worker"));

    // Wrap around to top
    app.select_next();
    assert_eq!(app.selected_index, 0);
    assert_eq!(app.selected_service_id(), Some("web"));

    // Wrap around backwards to bottom
    app.select_prev();
    assert_eq!(app.selected_index, 2);
    assert_eq!(app.selected_service_id(), Some("worker"));
}

#[test]
fn test_app_state_focus_and_autoscroll_toggles() {
    let services = create_sample_services();
    let mut app = AppState::new(services, 8080);

    assert_eq!(app.focus, ActiveFocus::ServiceCockpit);
    app.toggle_focus();
    assert_eq!(app.focus, ActiveFocus::LogStream);
    app.toggle_focus();
    assert_eq!(app.focus, ActiveFocus::ServiceCockpit);

    assert_eq!(app.autoscroll, AutoScrollState::Follow);
    app.toggle_autoscroll();
    assert_eq!(app.autoscroll, AutoScrollState::Paused { scroll_offset: 0 });
    app.toggle_autoscroll();
    assert_eq!(app.autoscroll, AutoScrollState::Follow);
}

#[test]
fn test_app_state_scroll_offset_state_machine() {
    let services = create_sample_services();
    let mut app = AppState::new(services, 8080);

    // Initial state is Follow
    assert_eq!(app.autoscroll, AutoScrollState::Follow);

    // Scrolling up pauses autoscroll
    app.scroll_up(5, 50, 20); // total 50, view 20, max_offset = 30
    assert_eq!(
        app.autoscroll,
        AutoScrollState::Paused { scroll_offset: 25 }
    );

    app.scroll_up(10, 50, 20);
    assert_eq!(
        app.autoscroll,
        AutoScrollState::Paused { scroll_offset: 15 }
    );

    // Scrolling down towards bottom
    app.scroll_down(10, 50, 20);
    assert_eq!(
        app.autoscroll,
        AutoScrollState::Paused { scroll_offset: 25 }
    );

    // Reaching or exceeding max_offset re-engages Follow
    app.scroll_down(10, 50, 20);
    assert_eq!(app.autoscroll, AutoScrollState::Follow);
}

#[test]
fn test_app_state_search_filter_regex_validation() {
    let services = create_sample_services();
    let mut app = AppState::new(services, 8080);

    app.set_search_query("health.*200".to_string());
    assert!(app.search_regex_valid);
    assert_eq!(app.search_query, "health.*200");

    // Invalid regex syntax
    app.set_search_query("[invalid(regex".to_string());
    assert!(!app.search_regex_valid);
}

#[test]
fn test_app_state_modal_toggles() {
    let services = create_sample_services();
    let mut app = AppState::new(services, 8080);

    assert_eq!(app.modal, ModalState::None);

    app.toggle_proxy_modal();
    assert_eq!(app.modal, ModalState::ProxyInfo);

    app.toggle_proxy_modal();
    assert_eq!(app.modal, ModalState::None);

    app.toggle_help_modal();
    assert_eq!(app.modal, ModalState::HelpCheatsheet);

    app.close_modal();
    assert_eq!(app.modal, ModalState::None);

    app.open_search_modal();
    assert!(matches!(app.modal, ModalState::SearchFilter { .. }));

    app.close_modal();
    assert_eq!(app.modal, ModalState::None);
}

#[test]
fn test_app_state_key_event_commands() {
    let services = create_sample_services();
    let mut app = AppState::new(services, 8080);

    // Restart key [r]
    let cmd = app.handle_key_event(make_key_event(KeyCode::Char('r'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(TuiCommand::RestartService("web".to_string())));

    // Stop key [s]
    let cmd = app.handle_key_event(make_key_event(KeyCode::Char('s'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(TuiCommand::StopService("web".to_string())));

    // Start all [a]
    let cmd = app.handle_key_event(make_key_event(KeyCode::Char('a'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(TuiCommand::StartAll));

    // Stop all [x]
    let cmd = app.handle_key_event(make_key_event(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(TuiCommand::StopAll));

    // Quit key [q]
    let cmd = app.handle_key_event(make_key_event(KeyCode::Char('q'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(TuiCommand::Quit));

    // Esc key
    let cmd = app.handle_key_event(make_key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(cmd, Some(TuiCommand::Quit));

    // Ctrl+C
    let cmd = app.handle_key_event(make_key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(cmd, Some(TuiCommand::Quit));

    // Clear logs [c]
    assert_eq!(app.selected_service().unwrap().buffer.len(), 2);
    let cmd = app.handle_key_event(make_key_event(KeyCode::Char('c'), KeyModifiers::NONE));
    assert_eq!(cmd, None);
    assert_eq!(app.selected_service().unwrap().buffer.len(), 0);
}

#[test]
fn test_search_modal_keystroke_flow() {
    let services = create_sample_services();
    let mut app = AppState::new(services, 8080);

    // Open search modal via [/]
    app.handle_key_event(make_key_event(KeyCode::Char('/'), KeyModifiers::NONE));
    assert!(matches!(app.modal, ModalState::SearchFilter { .. }));

    // Type "api"
    app.handle_key_event(make_key_event(KeyCode::Char('a'), KeyModifiers::NONE));
    app.handle_key_event(make_key_event(KeyCode::Char('p'), KeyModifiers::NONE));
    app.handle_key_event(make_key_event(KeyCode::Char('i'), KeyModifiers::NONE));

    if let ModalState::SearchFilter { ref query, .. } = app.modal {
        assert_eq!(query, "api");
    } else {
        panic!("Modal should be SearchFilter");
    }

    // Press Backspace -> "ap"
    app.handle_key_event(make_key_event(KeyCode::Backspace, KeyModifiers::NONE));
    if let ModalState::SearchFilter { ref query, .. } = app.modal {
        assert_eq!(query, "ap");
    }

    // Press Enter to apply and close
    app.handle_key_event(make_key_event(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.modal, ModalState::None);
    assert_eq!(app.search_query, "ap");
}

#[test]
fn test_ansi_parsing_and_styling() {
    let theme = Theme::default();

    // Plain text without escapes
    let spans = parse_ansi_to_spans("plain text message", &theme);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content, "plain text message");

    // SGR Color escape: red text
    let ansi_input = "\x1b[31mRed Alert\x1b[0m";
    let spans = parse_ansi_to_spans(ansi_input, &theme);
    assert!(!spans.is_empty());

    // 24-bit TrueColor escape
    let truecolor_input = "\x1b[38;2;0;229;255mElectric Cyan\x1b[0m";
    let spans = parse_ansi_to_spans(truecolor_input, &theme);
    assert!(!spans.is_empty());
}

#[test]
fn test_split_screen_ui_rendering_headless_buffer() {
    let services = create_sample_services();
    let mut app = AppState::new(services, 8080);

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("create test terminal");

    // Render primary split-screen cockpit
    terminal
        .draw(|frame| {
            render(frame, &mut app);
        })
        .expect("draw main UI");

    let buffer = terminal.backend().buffer();
    let buffer_str = format!("{:?}", buffer);

    assert!(buffer_str.contains("spinmux"));
    assert!(buffer_str.contains("Service Cockpit"));
    assert!(buffer_str.contains("Log Stream: web"));
    assert!(buffer_str.contains("web.localhost:8080"));
    assert!(buffer_str.contains("api.localhost:8080"));

    // Render with Search Filter modal open
    app.open_search_modal();
    terminal
        .draw(|frame| {
            render(frame, &mut app);
        })
        .expect("draw search modal");

    // Render with Proxy Modal open
    app.toggle_proxy_modal();
    terminal
        .draw(|frame| {
            render(frame, &mut app);
        })
        .expect("draw proxy modal");

    // Render with Help Modal open
    app.toggle_help_modal();
    terminal
        .draw(|frame| {
            render(frame, &mut app);
        })
        .expect("draw help modal");
}

#[test]
fn test_terminal_too_small_rendering() {
    let services = create_sample_services();
    let mut app = AppState::new(services, 8080);

    // Small terminal: 40 cols x 10 rows
    let backend = TestBackend::new(40, 10);
    let mut terminal = Terminal::new(backend).expect("create small test terminal");

    terminal
        .draw(|frame| {
            render(frame, &mut app);
        })
        .expect("draw too small guard");

    let buffer = terminal.backend().buffer();
    let buffer_str = format!("{:?}", buffer);
    assert!(buffer_str.contains("Terminal Dimension Warning") || buffer_str.contains("Viewport"));
}

#[test]
fn test_terminal_guard_idempotent_restoration() {
    // Calling restore_terminal multiple times must never panic
    assert!(TerminalGuard::restore_terminal().is_ok());
    assert!(TerminalGuard::restore_terminal().is_ok());
}
