use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use regex::Regex;

use crate::ringbuf::RingBuffer;
use crate::supervisor::ProcessStatus;

/// Focus target in the split-screen view
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveFocus {
    ServiceCockpit,
    LogStream,
}

/// Active popup modal dialog state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModalState {
    None,
    ProxyInfo,
    HelpCheatsheet,
    SearchFilter { query: String, cursor_pos: usize },
}

/// Log pane auto-scrolling state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoScrollState {
    /// Viewport locks to the bottom and follows new incoming lines
    Follow,
    /// Viewport is paused at a specific scroll offset
    Paused { scroll_offset: usize },
}

/// View-model for an auto-discovered supervised service
#[derive(Clone)]
pub struct ServiceViewData {
    pub id: String,
    pub name: String,
    pub command: String,
    pub status: ProcessStatus,
    pub pid: Option<u32>,
    pub port: Option<u16>,
    pub subdomain: String,
    pub restart_count: u32,
    pub uptime: Option<Duration>,
    pub memory_bytes: u64,
    pub cpu_percentage: f32,
    pub buffer: Arc<RingBuffer>,
}

impl std::fmt::Debug for ServiceViewData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceViewData")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("status", &self.status)
            .field("pid", &self.pid)
            .field("port", &self.port)
            .field("subdomain", &self.subdomain)
            .field("restart_count", &self.restart_count)
            .finish()
    }
}

/// Commands triggered by user interactions in the TUI
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiCommand {
    Quit,
    RestartService(String),
    StopService(String),
    StartService(String),
    StartAll,
    StopAll,
    ClearLogs(String),
}

/// Root application state machine driving the TUI cockpit
pub struct AppState {
    pub running: bool,
    pub is_dirty: bool,
    pub focus: ActiveFocus,
    pub modal: ModalState,
    pub autoscroll: AutoScrollState,

    pub services: Vec<ServiceViewData>,
    pub selected_index: usize,

    pub start_time: Instant,
    pub last_render_latency: Duration,
    pub total_memory_bytes: u64,
    pub total_cpu_percentage: f32,
    pub proxy_port: u16,

    pub search_query: String,
    pub search_regex_valid: bool,
    pub search_match_count: usize,

    pub status_banner: Option<(String, Instant)>,
}

impl AppState {
    pub fn new(services: Vec<ServiceViewData>, proxy_port: u16) -> Self {
        Self {
            running: true,
            is_dirty: true,
            focus: ActiveFocus::ServiceCockpit,
            modal: ModalState::None,
            autoscroll: AutoScrollState::Follow,
            services,
            selected_index: 0,
            start_time: Instant::now(),
            last_render_latency: Duration::from_millis(1),
            total_memory_bytes: 0,
            total_cpu_percentage: 0.0,
            proxy_port,
            search_query: String::new(),
            search_regex_valid: true,
            search_match_count: 0,
            status_banner: None,
        }
    }

    pub fn select_next(&mut self) {
        if self.services.is_empty() {
            self.selected_index = 0;
            return;
        }
        self.selected_index = (self.selected_index + 1) % self.services.len();
        self.is_dirty = true;
    }

    pub fn select_prev(&mut self) {
        if self.services.is_empty() {
            self.selected_index = 0;
            return;
        }
        if self.selected_index == 0 {
            self.selected_index = self.services.len().saturating_sub(1);
        } else {
            self.selected_index -= 1;
        }
        self.is_dirty = true;
    }

    pub fn select_index(&mut self, idx: usize) {
        if self.services.is_empty() {
            self.selected_index = 0;
        } else {
            self.selected_index = idx.min(self.services.len() - 1);
        }
        self.is_dirty = true;
    }

    pub fn selected_service(&self) -> Option<&ServiceViewData> {
        self.services.get(self.selected_index)
    }

    pub fn selected_service_mut(&mut self) -> Option<&mut ServiceViewData> {
        self.services.get_mut(self.selected_index)
    }

    pub fn selected_service_id(&self) -> Option<&str> {
        self.selected_service().map(|s| s.id.as_str())
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            ActiveFocus::ServiceCockpit => ActiveFocus::LogStream,
            ActiveFocus::LogStream => ActiveFocus::ServiceCockpit,
        };
        self.is_dirty = true;
    }

    pub fn toggle_autoscroll(&mut self) {
        self.autoscroll = match self.autoscroll {
            AutoScrollState::Follow => AutoScrollState::Paused { scroll_offset: 0 },
            AutoScrollState::Paused { .. } => AutoScrollState::Follow,
        };
        self.is_dirty = true;
    }

    pub fn scroll_up(&mut self, delta: usize, total_lines: usize, viewport_height: usize) {
        let max_offset = total_lines.saturating_sub(viewport_height);
        let current_offset = match self.autoscroll {
            AutoScrollState::Follow => max_offset,
            AutoScrollState::Paused { scroll_offset } => scroll_offset,
        };

        let new_offset = current_offset.saturating_sub(delta);
        self.autoscroll = AutoScrollState::Paused {
            scroll_offset: new_offset,
        };
        self.is_dirty = true;
    }

    pub fn scroll_down(&mut self, delta: usize, total_lines: usize, viewport_height: usize) {
        let max_offset = total_lines.saturating_sub(viewport_height);
        let current_offset = match self.autoscroll {
            AutoScrollState::Follow => max_offset,
            AutoScrollState::Paused { scroll_offset } => scroll_offset,
        };

        let new_offset = current_offset + delta;
        if new_offset >= max_offset {
            self.autoscroll = AutoScrollState::Follow;
        } else {
            self.autoscroll = AutoScrollState::Paused {
                scroll_offset: new_offset,
            };
        }
        self.is_dirty = true;
    }

    pub fn clear_current_logs(&mut self) {
        if let Some(svc) = self.selected_service() {
            svc.buffer.clear();
            self.set_status_banner(format!("Cleared logs for '{}'", svc.name));
            self.is_dirty = true;
        }
    }

    pub fn open_search_modal(&mut self) {
        let current = self.search_query.clone();
        let len = current.len();
        self.modal = ModalState::SearchFilter {
            query: current,
            cursor_pos: len,
        };
        self.is_dirty = true;
    }

    pub fn close_modal(&mut self) {
        self.modal = ModalState::None;
        self.is_dirty = true;
    }

    pub fn toggle_proxy_modal(&mut self) {
        self.modal = match self.modal {
            ModalState::ProxyInfo => ModalState::None,
            _ => ModalState::ProxyInfo,
        };
        self.is_dirty = true;
    }

    pub fn toggle_help_modal(&mut self) {
        self.modal = match self.modal {
            ModalState::HelpCheatsheet => ModalState::None,
            _ => ModalState::HelpCheatsheet,
        };
        self.is_dirty = true;
    }

    pub fn set_search_query(&mut self, query: String) {
        self.search_regex_valid = Regex::new(&query).is_ok();
        self.search_query = query;
        self.is_dirty = true;
    }

    pub fn set_status_banner(&mut self, msg: impl Into<String>) {
        self.status_banner = Some((msg.into(), Instant::now()));
        self.is_dirty = true;
    }

    pub fn active_services_count(&self) -> usize {
        self.services
            .iter()
            .filter(|s| s.status.is_running())
            .count()
    }

    pub fn total_services_count(&self) -> usize {
        self.services.len()
    }

    pub fn uptime(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub fn update_service_status(
        &mut self,
        service_id: &str,
        status: ProcessStatus,
        pid: Option<u32>,
        restart_count: Option<u32>,
    ) {
        for svc in &mut self.services {
            if svc.id == service_id || svc.name == service_id {
                if svc.status != status {
                    svc.status = status.clone();
                    self.is_dirty = true;
                }
                if pid.is_some() && svc.pid != pid {
                    svc.pid = pid;
                    self.is_dirty = true;
                }
                if let Some(rc) = restart_count {
                    if svc.restart_count != rc {
                        svc.restart_count = rc;
                        self.is_dirty = true;
                    }
                }
            }
        }
    }

    pub fn update_telemetry(&mut self) {
        // Clear transient status message after 3 seconds
        if let Some((_, timestamp)) = self.status_banner {
            if timestamp.elapsed() > Duration::from_secs(3) {
                self.status_banner = None;
                self.is_dirty = true;
            }
        }
    }

    /// Handles a keyboard event and returns an optional command to execute
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Option<TuiCommand> {
        // Global Ctrl+C handler
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(TuiCommand::Quit);
        }

        // Handle modal interactions if a modal dialog is currently open
        if let ModalState::SearchFilter {
            ref mut query,
            ref mut cursor_pos,
        } = self.modal
        {
            let mut close = false;
            let mut update_query = None;

            match key.code {
                KeyCode::Esc => {
                    close = true;
                }
                KeyCode::Enter => {
                    update_query = Some(query.clone());
                    close = true;
                }
                KeyCode::Backspace => {
                    if *cursor_pos > 0 && !query.is_empty() {
                        *cursor_pos -= 1;
                        if *cursor_pos < query.len() {
                            query.remove(*cursor_pos);
                        }
                        update_query = Some(query.clone());
                    }
                }
                KeyCode::Left => {
                    if *cursor_pos > 0 {
                        *cursor_pos -= 1;
                        self.is_dirty = true;
                    }
                }
                KeyCode::Right => {
                    if *cursor_pos < query.len() {
                        *cursor_pos += 1;
                        self.is_dirty = true;
                    }
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.clear();
                    *cursor_pos = 0;
                    update_query = Some(String::new());
                }
                KeyCode::Char(c) if *cursor_pos <= query.len() => {
                    query.insert(*cursor_pos, c);
                    *cursor_pos += 1;
                    update_query = Some(query.clone());
                }
                _ => {}
            }

            if let Some(q) = update_query {
                self.set_search_query(q);
            }
            if close {
                self.close_modal();
            }
            return None;
        }

        if matches!(
            self.modal,
            ModalState::ProxyInfo | ModalState::HelpCheatsheet
        ) {
            match key.code {
                KeyCode::Esc
                | KeyCode::Enter
                | KeyCode::Char('p')
                | KeyCode::Char('?')
                | KeyCode::Char('h')
                | KeyCode::Char('q') => {
                    self.close_modal();
                }
                _ => {}
            }
            return None;
        }

        // Root keybindings when no modal is open
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(TuiCommand::Quit),

            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                None
            }

            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                None
            }

            KeyCode::Tab => {
                self.toggle_focus();
                None
            }

            KeyCode::Char(' ') => {
                self.toggle_autoscroll();
                None
            }

            KeyCode::PageUp => {
                self.scroll_up(15, 10_000, 30);
                None
            }

            KeyCode::PageDown => {
                self.scroll_down(15, 10_000, 30);
                None
            }

            KeyCode::Char('/') | KeyCode::Char('f') => {
                self.open_search_modal();
                None
            }

            KeyCode::Char('p') => {
                self.toggle_proxy_modal();
                None
            }

            KeyCode::Char('?') | KeyCode::Char('h') => {
                self.toggle_help_modal();
                None
            }

            KeyCode::Char('c') => {
                self.clear_current_logs();
                None
            }

            KeyCode::Char('r') => {
                if let Some(svc) = self.selected_service() {
                    let id = svc.id.clone();
                    self.set_status_banner(format!("Restarting service '{}'...", svc.name));
                    Some(TuiCommand::RestartService(id))
                } else {
                    None
                }
            }

            KeyCode::Char('s') => {
                if let Some(svc) = self.selected_service() {
                    let id = svc.id.clone();
                    self.set_status_banner(format!("Stopping service '{}'...", svc.name));
                    Some(TuiCommand::StopService(id))
                } else {
                    None
                }
            }

            KeyCode::Char('a') => {
                self.set_status_banner("Starting all services...");
                Some(TuiCommand::StartAll)
            }

            KeyCode::Char('x') => {
                self.set_status_banner("Stopping all services...");
                Some(TuiCommand::StopAll)
            }

            _ => None,
        }
    }
}
