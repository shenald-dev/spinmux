use std::sync::Arc;

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Table,
};
use ratatui::Frame;
use regex::Regex;

use super::app::{ActiveFocus, AppState, AutoScrollState, ModalState};
use super::theme::{SpinmuxPalette, Theme};
use crate::ringbuf::{LogEntry, LogStream};
use crate::supervisor::ProcessStatus;

/// Primary render function called on every dirty frame tick
pub fn render(frame: &mut Frame, app: &mut AppState) {
    let theme = Theme::default();
    let area = frame.area();

    // Responsive guard: if terminal viewport is too small, render warning
    if area.width < 60 || area.height < 15 {
        render_too_small(frame, area, &theme);
        return;
    }

    // Split root vertically: Header (3), Main Body (Min 10), Footer (3)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(area);

    let header_area = chunks[0];
    let body_area = chunks[1];
    let footer_area = chunks[2];

    // Split body horizontally: Left Cockpit (35%), Right Log Stream (65%)
    let cockpit_width = if area.width >= 110 {
        Constraint::Length(52)
    } else {
        Constraint::Percentage(42)
    };

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([cockpit_width, Constraint::Min(40)])
        .split(body_area);

    let cockpit_area = body_chunks[0];
    let log_area = body_chunks[1];

    // Render primary components
    render_header(frame, header_area, app, &theme);
    render_service_cockpit(frame, cockpit_area, app, &theme);
    render_log_stream(frame, log_area, app, &theme);
    render_footer(frame, footer_area, app, &theme);

    // Render active modal overlays
    match &app.modal {
        ModalState::ProxyInfo => render_proxy_modal(frame, area, app, &theme),
        ModalState::SearchFilter { query, cursor_pos } => {
            render_search_modal(frame, area, query, *cursor_pos, app, &theme)
        }
        ModalState::HelpCheatsheet => render_help_modal(frame, area, &theme),
        ModalState::None => {}
    }
}

/// Fallback display when terminal dimensions are below minimums
fn render_too_small(frame: &mut Frame, area: Rect, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.status_stopped_color()))
        .title(" Terminal Dimension Warning ");

    let text = vec![
        Line::from(vec![
            Span::styled("Viewport: ", Style::default().fg(theme.text_muted())),
            Span::styled(
                format!("{}x{}", area.width, area.height),
                Style::default().fg(theme.status_starting_color()).bold(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Minimum required: ",
                Style::default().fg(theme.text_muted()),
            ),
            Span::styled(
                "60x15 cols/rows",
                Style::default().fg(theme.electric_cyan()).bold(),
            ),
        ]),
        Line::from("Please enlarge terminal window to continue..."),
    ];

    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(Alignment::Center);

    frame.render_widget(paragraph, area);
}

/// Top telemetry banner displaying logo, reverse proxy, active process count, uptime, and latency
fn render_header(frame: &mut Frame, area: Rect, app: &AppState, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme.border_type())
        .border_style(Style::default().fg(theme.border_normal()));

    let uptime = app.uptime();
    let hours = uptime.as_secs() / 3600;
    let mins = (uptime.as_secs() % 3600) / 60;
    let secs = uptime.as_secs() % 60;
    let uptime_str = format!("{:02}:{:02}:{:02}", hours, mins, secs);

    let active_count = app.active_services_count();
    let total_count = app.total_services_count();

    let service_ratio_color = if total_count == 0 {
        theme.text_dim()
    } else if active_count == total_count {
        theme.status_running_color()
    } else if active_count > 0 {
        theme.status_starting_color()
    } else {
        theme.status_stopped_color()
    };

    let latency_ms = app.last_render_latency.as_micros() as f64 / 1000.0;

    let header_line = Line::from(vec![
        Span::styled("⚡ spinmux", theme.header_logo_style()),
        Span::styled(" v0.1.0  ", Style::default().fg(theme.text_dim())),
        Span::styled("│ ", Style::default().fg(theme.border_normal())),
        Span::styled("Proxy: ", Style::default().fg(theme.text_muted())),
        Span::styled(
            format!("http://localhost:{}", app.proxy_port),
            theme.header_proxy_style(),
        ),
        Span::styled("  │ ", Style::default().fg(theme.border_normal())),
        Span::styled("Services: ", Style::default().fg(theme.text_muted())),
        Span::styled(
            format!("{}/{} active", active_count, total_count),
            Style::default().fg(service_ratio_color).bold(),
        ),
        Span::styled("  │ ", Style::default().fg(theme.border_normal())),
        Span::styled("Uptime: ", Style::default().fg(theme.text_muted())),
        Span::styled(uptime_str, Style::default().fg(theme.text_primary())),
        Span::styled("  │ ", Style::default().fg(theme.border_normal())),
        Span::styled("Render: ", Style::default().fg(theme.text_muted())),
        Span::styled(
            format!("{:.1}ms", latency_ms),
            Style::default().fg(theme.electric_cyan()),
        ),
    ]);

    let paragraph = Paragraph::new(header_line).block(block);
    frame.render_widget(paragraph, area);
}

/// Left pane: service cockpit table with status glyphs, ports, virtual subdomains, and PIDs
fn render_service_cockpit(frame: &mut Frame, area: Rect, app: &AppState, theme: &Theme) {
    let is_focused = app.focus == ActiveFocus::ServiceCockpit;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme.border_type())
        .border_style(theme.block_style(is_focused))
        .title(Span::styled(
            " Service Cockpit ",
            if is_focused {
                Style::default().fg(theme.electric_cyan()).bold()
            } else {
                Style::default().fg(theme.text_muted())
            },
        ));

    if app.services.is_empty() {
        let empty_msg = vec![
            Line::from(""),
            Line::from(Span::styled(
                "No runnable services discovered.",
                Style::default().fg(theme.text_muted()),
            )),
            Line::from(Span::styled(
                "Add package.json, Procfile, or compose.yml",
                Style::default().fg(theme.text_dim()),
            )),
        ];
        let p = Paragraph::new(empty_msg)
            .block(block)
            .alignment(Alignment::Center);
        frame.render_widget(p, area);
        return;
    }

    let header_row = Row::new(vec![
        Span::styled("  ", Style::default().bold()),
        Span::styled("SERVICE", Style::default().fg(theme.text_dim()).bold()),
        Span::styled("PORT", Style::default().fg(theme.text_dim()).bold()),
        Span::styled("SUBDOMAIN", Style::default().fg(theme.text_dim()).bold()),
        Span::styled("TELEMETRY", Style::default().fg(theme.text_dim()).bold()),
    ])
    .style(Style::default().bg(theme.surface_slate()))
    .bottom_margin(1);

    let rows: Vec<Row> = app
        .services
        .iter()
        .enumerate()
        .map(|(idx, svc)| {
            let is_selected = idx == app.selected_index;
            let status_glyph = theme.status_symbol(&svc.status);
            let status_col = theme.status_color(&svc.status);

            let prefix = if is_selected { "▶ " } else { "  " };

            let name_display = if svc.name.len() > 14 {
                format!("{}...", &svc.name[..11])
            } else {
                svc.name.clone()
            };

            let port_display = if let Some(p) = svc.port {
                format!(":{}", p)
            } else {
                "----".to_string()
            };

            let subdomain_display = format!("{}.localhost:{}", svc.subdomain, app.proxy_port);

            let pid_display = match &svc.status {
                ProcessStatus::Running(pid) => {
                    format!("PID:{} R:{}", pid, svc.restart_count)
                }
                ProcessStatus::Starting => "STARTING".to_string(),
                ProcessStatus::Stopped(code) => {
                    if let Some(c) = code {
                        format!("EXIT:{} R:{}", c, svc.restart_count)
                    } else {
                        format!("STOPPED R:{}", svc.restart_count)
                    }
                }
                ProcessStatus::Crashing(_) => format!("CRASH R:{}", svc.restart_count),
                ProcessStatus::CrashLoopBackOff => "BACKOFF".to_string(),
            };

            let row_cells = vec![
                Span::styled(
                    format!("{}{}", prefix, status_glyph),
                    Style::default().fg(status_col).bold(),
                ),
                Span::styled(
                    name_display,
                    if is_selected {
                        Style::default().fg(theme.electric_cyan()).bold()
                    } else {
                        Style::default().fg(theme.text_primary())
                    },
                ),
                Span::styled(port_display, Style::default().fg(theme.electric_cyan())),
                Span::styled(subdomain_display, Style::default().fg(theme.text_muted())),
                Span::styled(pid_display, Style::default().fg(theme.text_dim())),
            ];

            let row = Row::new(row_cells);
            if is_selected {
                row.style(theme.selected_row_style())
            } else {
                row
            }
        })
        .collect();

    let widths = [
        Constraint::Length(4),
        Constraint::Length(10),
        Constraint::Length(7),
        Constraint::Length(20),
        Constraint::Min(8),
    ];

    let table = Table::new(rows, widths).header(header_row).block(block);
    frame.render_widget(table, area);
}

/// Right pane: real-time streaming log output from selected service ring-buffer
fn render_log_stream(frame: &mut Frame, area: Rect, app: &mut AppState, theme: &Theme) {
    let is_focused = app.focus == ActiveFocus::LogStream;

    let selected_svc = app.selected_service();
    let service_name = selected_svc
        .map(|s| s.name.as_str())
        .unwrap_or("No Service");

    let autoscroll_badge = match app.autoscroll {
        AutoScrollState::Follow => Span::styled(" [AUTO-SCROLL: ON] ", theme.autoscroll_on_style()),
        AutoScrollState::Paused { .. } => {
            Span::styled(" [SCROLL-LOCKED] ", theme.autoscroll_paused_style())
        }
    };

    let title_spans = vec![
        Span::styled(
            format!(" Log Stream: {} ", service_name),
            if is_focused {
                Style::default().fg(theme.electric_cyan()).bold()
            } else {
                Style::default().fg(theme.text_muted())
            },
        ),
        autoscroll_badge,
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme.border_type())
        .border_style(theme.block_style(is_focused))
        .title(Line::from(title_spans));

    let inner_area = block.inner(area);
    let visible_height = inner_area.height as usize;

    if selected_svc.is_none() {
        let p = Paragraph::new("No service selected").block(block);
        frame.render_widget(p, area);
        return;
    }

    let buffer = selected_svc.unwrap().buffer.clone();
    let raw_entries = buffer.snapshot(10_000);

    // Apply search filter if active
    let filter_active = !app.search_query.is_empty();
    let regex_opt = if filter_active && app.search_regex_valid {
        Regex::new(&app.search_query).ok()
    } else {
        None
    };

    let filtered_entries: Vec<Arc<LogEntry>> = if filter_active {
        raw_entries
            .into_iter()
            .filter(|entry| {
                if let Some(ref re) = regex_opt {
                    re.is_match(&entry.message)
                } else {
                    entry
                        .message
                        .to_lowercase()
                        .contains(&app.search_query.to_lowercase())
                }
            })
            .map(Arc::new)
            .collect()
    } else {
        raw_entries.into_iter().map(Arc::new).collect()
    };

    app.search_match_count = filtered_entries.len();
    let total_lines = filtered_entries.len();

    // Viewport calculation
    let max_offset = total_lines.saturating_sub(visible_height);
    let current_offset = match app.autoscroll {
        AutoScrollState::Follow => max_offset,
        AutoScrollState::Paused { scroll_offset } => scroll_offset.min(max_offset),
    };

    let slice_end = (current_offset + visible_height).min(total_lines);
    let slice_start = current_offset.min(slice_end);

    let mut lines_to_render = Vec::with_capacity(visible_height);

    if total_lines == 0 {
        if filter_active {
            lines_to_render.push(Line::from(vec![
                Span::styled(
                    "No log entries match filter '",
                    Style::default().fg(theme.text_dim()),
                ),
                Span::styled(
                    &app.search_query,
                    Style::default().fg(theme.electric_cyan()).bold(),
                ),
                Span::styled("'", Style::default().fg(theme.text_dim())),
            ]));
        } else {
            lines_to_render.push(Line::from(Span::styled(
                "[No log entries recorded yet]",
                Style::default().fg(theme.text_dim()),
            )));
        }
    } else {
        for entry in &filtered_entries[slice_start..slice_end] {
            let timestamp_str = format!("[{}] ", entry.timestamp.format("%H:%M:%S%.3f"));
            let timestamp_span = Span::styled(timestamp_str, Style::default().fg(theme.text_dim()));

            let stream_span = match entry.stream {
                LogStream::Stdout => {
                    Span::styled("[stdout] ", Style::default().fg(theme.electric_cyan()))
                }
                LogStream::Stderr => Span::styled(
                    "[stderr] ",
                    Style::default().fg(theme.status_stopped_color()),
                ),
                LogStream::System => {
                    Span::styled("[system] ", Style::default().fg(theme.neon_violet()))
                }
            };

            let mut message_spans = parse_ansi_to_spans(&entry.message, theme);

            // Highlight matches if search active
            if filter_active {
                message_spans = highlight_search_matches(message_spans, &app.search_query, theme);
            }

            let mut full_spans = Vec::with_capacity(message_spans.len() + 2);
            full_spans.push(timestamp_span);
            full_spans.push(stream_span);
            full_spans.extend(message_spans);

            lines_to_render.push(Line::from(full_spans));
        }
    }

    let paragraph = Paragraph::new(lines_to_render).block(block);
    frame.render_widget(paragraph, area);

    // Render interactive scrollbar on the right edge of log area
    if total_lines > visible_height {
        let mut scrollbar_state = ScrollbarState::new(max_offset).position(current_offset);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"))
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .style(Style::default().fg(theme.border_normal()))
            .thumb_style(Style::default().fg(theme.electric_cyan()));

        frame.render_stateful_widget(scrollbar, inner_area, &mut scrollbar_state);
    }
}

/// Bottom footer command bar with ergonomic hotkey legends and status banner
fn render_footer(frame: &mut Frame, area: Rect, app: &AppState, theme: &Theme) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme.border_type())
        .border_style(Style::default().fg(theme.border_normal()));

    let make_key = |key: &'static str, desc: &'static str| -> Vec<Span<'static>> {
        vec![
            Span::styled("[", theme.key_bracket_style()),
            Span::styled(key, theme.key_char_style()),
            Span::styled("] ", theme.key_bracket_style()),
            Span::styled(format!("{}  ", desc), theme.key_desc_style()),
        ]
    };

    let mut spans = Vec::new();
    spans.extend(make_key("r", "Restart"));
    spans.extend(make_key("s", "Stop"));
    spans.extend(make_key("a", "Start All"));
    spans.extend(make_key("x", "Stop All"));
    spans.extend(make_key("Tab", "Focus"));
    spans.extend(make_key("Space", "Pause"));
    spans.extend(make_key("/", "Filter"));
    spans.extend(make_key("p", "Proxy"));
    spans.extend(make_key("c", "Clear"));
    spans.extend(make_key("?", "Help"));
    spans.extend(make_key("q", "Quit"));

    if let Some((ref msg, _)) = app.status_banner {
        spans.push(Span::styled(
            " │ ",
            Style::default().fg(theme.border_normal()),
        ));
        spans.push(Span::styled(
            msg,
            Style::default().fg(theme.status_starting_color()).bold(),
        ));
    }

    let p = Paragraph::new(Line::from(spans)).block(block);
    frame.render_widget(p, area);
}

/// Centered modal dialog showing all active virtual localhost routes and curl recipes
fn render_proxy_modal(frame: &mut Frame, area: Rect, app: &AppState, theme: &Theme) {
    let popup_area = centered_rect(75, 65, area);
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme.border_type())
        .border_style(Style::default().fg(theme.electric_cyan()))
        .title(Span::styled(
            " 🌐 Proxy Virtual Subdomain Routes ",
            Style::default().fg(theme.electric_cyan()).bold(),
        ));

    let mut text_lines = Vec::new();
    text_lines.push(Line::from(vec![
        Span::styled(
            "Reverse Proxy Base: ",
            Style::default().fg(theme.text_muted()),
        ),
        Span::styled(
            format!("http://localhost:{}", app.proxy_port),
            Style::default().fg(theme.electric_cyan()).bold(),
        ),
    ]));
    text_lines.push(Line::from(""));

    let header_fmt = format!(
        "{:<28} {:<16} {:<12} {:<12}",
        "SUBDOMAIN ROUTE", "SERVICE", "LOCAL PORT", "STATUS"
    );
    text_lines.push(Line::from(Span::styled(
        header_fmt,
        Style::default().fg(theme.text_dim()).bold(),
    )));
    text_lines.push(Line::from(Span::styled(
        "─".repeat(70),
        Style::default().fg(theme.border_normal()),
    )));

    for svc in &app.services {
        let route = format!("http://{}.localhost:{}", svc.subdomain, app.proxy_port);
        let port_str = svc
            .port
            .map(|p| format!(":{}", p))
            .unwrap_or_else(|| "----".to_string());
        let (status_label, status_col) = match &svc.status {
            ProcessStatus::Running(_) => ("Active", theme.status_running_color()),
            ProcessStatus::Starting => ("Starting", theme.status_starting_color()),
            ProcessStatus::Stopped(_) => ("Stopped", theme.status_stopped_color()),
            ProcessStatus::Crashing(_) => ("Crashing", theme.status_starting_color()),
            ProcessStatus::CrashLoopBackOff => ("Failed", theme.status_stopped_color()),
        };

        let row_line = Line::from(vec![
            Span::styled(
                format!("{:<28} ", route),
                Style::default().fg(theme.electric_cyan()),
            ),
            Span::styled(
                format!("{:<16} ", svc.name),
                Style::default().fg(theme.text_primary()),
            ),
            Span::styled(
                format!("{:<12} ", port_str),
                Style::default().fg(theme.text_muted()),
            ),
            Span::styled(
                format!("{:<12}", status_label),
                Style::default().fg(status_col).bold(),
            ),
        ]);
        text_lines.push(row_line);
    }

    text_lines.push(Line::from(""));
    text_lines.push(Line::from(Span::styled(
        "Test route via curl:",
        Style::default().fg(theme.text_muted()).bold(),
    )));

    if let Some(first) = app.services.first() {
        let curl_cmd = format!(
            "curl -H \"Host: {}.localhost\" http://localhost:{}",
            first.subdomain, app.proxy_port
        );
        text_lines.push(Line::from(Span::styled(
            format!("  $ {}", curl_cmd),
            Style::default().fg(theme.status_running_color()),
        )));
    }

    text_lines.push(Line::from(""));
    text_lines.push(Line::from(Span::styled(
        "[Esc / p / Enter] Close Dialog",
        Style::default().fg(theme.neon_violet()).bold(),
    )));

    let p = Paragraph::new(text_lines).block(block);
    frame.render_widget(p, popup_area);
}

/// Live search filter prompt with instant regex validation and match counting
fn render_search_modal(
    frame: &mut Frame,
    area: Rect,
    query: &str,
    cursor_pos: usize,
    app: &AppState,
    theme: &Theme,
) {
    let popup_area = centered_rect(65, 20, area);
    frame.render_widget(Clear, popup_area);

    let border_color = if app.search_regex_valid {
        theme.electric_cyan()
    } else {
        theme.status_stopped_color()
    };

    let title = if app.search_regex_valid {
        " 🔍 Filter Logs [Regex / Substring] "
    } else {
        " ⚠️ Filter Logs [Invalid Regex Syntax] "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme.border_type())
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title,
            Style::default().fg(border_color).bold(),
        ));

    let mut query_with_cursor = String::new();
    let char_indices: Vec<(usize, char)> = query.char_indices().collect();
    let mut rendered_cursor = false;

    for (idx, (byte_idx, c)) in char_indices.iter().enumerate() {
        if idx == cursor_pos {
            query_with_cursor.push('▎');
            rendered_cursor = true;
        }
        query_with_cursor.push(*c);
        let _ = byte_idx;
    }
    if !rendered_cursor {
        query_with_cursor.push('▎');
    }

    let match_badge = if app.search_regex_valid {
        Span::styled(
            format!(" [{} matches]", app.search_match_count),
            Style::default().fg(theme.status_running_color()).bold(),
        )
    } else {
        Span::styled(
            " [Syntax Error]",
            Style::default().fg(theme.status_stopped_color()).bold(),
        )
    };

    let text = vec![
        Line::from(vec![
            Span::styled("Pattern: ", Style::default().fg(theme.text_muted())),
            Span::styled(
                query_with_cursor,
                Style::default().fg(theme.text_primary()).bold(),
            ),
            match_badge,
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("[Enter]", theme.key_char_style()),
            Span::styled(" Apply  ", theme.key_desc_style()),
            Span::styled("[Esc]", theme.key_char_style()),
            Span::styled(" Close  ", theme.key_desc_style()),
            Span::styled("[Ctrl+U]", theme.key_char_style()),
            Span::styled(" Clear Pattern", theme.key_desc_style()),
        ]),
    ];

    let p = Paragraph::new(text).block(block);
    frame.render_widget(p, popup_area);
}

/// Help cheatsheet popup modal showing full keyboard controls
fn render_help_modal(frame: &mut Frame, area: Rect, theme: &Theme) {
    let popup_area = centered_rect(70, 75, area);
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme.border_type())
        .border_style(Style::default().fg(theme.neon_violet()))
        .title(Span::styled(
            " ⌨️ Spinmux Cockpit Navigation Cheatsheet ",
            Style::default().fg(theme.neon_violet()).bold(),
        ));

    let mut lines = Vec::new();
    let make_row = |key: &'static str, desc: &'static str| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("  {:<16}", key), theme.key_char_style()),
            Span::styled(desc, theme.key_desc_style()),
        ])
    };

    lines.push(Line::from(Span::styled(
        "PROCESS SUPERVISION CONTROLS:",
        Style::default().fg(theme.electric_cyan()).bold(),
    )));
    lines.push(make_row("[r]", "Restart currently selected service"));
    lines.push(make_row(
        "[s]",
        "Gracefully stop currently selected service",
    ));
    lines.push(make_row("[a]", "Start / spawn all discovered services"));
    lines.push(make_row("[x]", "Stop all running services simultaneously"));
    lines.push(make_row(
        "[c]",
        "Clear in-memory ring-buffer logs for selected service",
    ));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        "NAVIGATION & VIEWPORT:",
        Style::default().fg(theme.electric_cyan()).bold(),
    )));
    lines.push(make_row("[j] / [Down]", "Select next service in table"));
    lines.push(make_row("[k] / [Up]", "Select previous service in table"));
    lines.push(make_row(
        "[Tab]",
        "Toggle focus between Cockpit and Log Stream",
    ));
    lines.push(make_row(
        "[Space]",
        "Toggle Auto-Scroll Follow vs Scroll-Locked",
    ));
    lines.push(make_row("[PgUp] / [PgDn]", "Scroll log history up/down"));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        "MODALS & SEARCH:",
        Style::default().fg(theme.electric_cyan()).bold(),
    )));
    lines.push(make_row(
        "[/] or [f]",
        "Open live regex / substring log filter dialog",
    ));
    lines.push(make_row(
        "[p]",
        "Toggle Virtual Subdomain Proxy routing table",
    ));
    lines.push(make_row("[?] or [h]", "Toggle this help cheatsheet"));
    lines.push(make_row(
        "[q] or [Esc]",
        "Quit Spinmux cleanly (zero orphaned processes)",
    ));
    lines.push(Line::from(""));

    lines.push(Line::from(Span::styled(
        "[Esc / ? / Enter] Close Dialog",
        Style::default().fg(theme.neon_violet()).bold(),
    )));

    let p = Paragraph::new(lines).block(block);
    frame.render_widget(p, popup_area);
}

/// Computes centered Rect coordinates with percentage sizing
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Converts raw log lines with ANSI escape sequences into styled Ratatui Spans
pub fn parse_ansi_to_spans<'a>(input: &'a str, _theme: &Theme) -> Vec<Span<'a>> {
    if !input.contains('\x1b') {
        return vec![Span::raw(input)];
    }

    let mut spans = Vec::new();
    let mut current_style = Style::default();
    let mut i = 0;
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut segment_start = 0;

    while i < len {
        if bytes[i] == 0x1b && i + 1 < len && bytes[i + 1] == b'[' {
            // Push previous plain text segment
            if i > segment_start {
                spans.push(Span::styled(&input[segment_start..i], current_style));
            }

            // Find ending code in @..=~
            let mut j = i + 2;
            while j < len && !((b'@'..=b'~').contains(&bytes[j])) {
                j += 1;
            }

            if j < len {
                let code_byte = bytes[j];
                let params = &input[i + 2..j];

                if code_byte == b'm' {
                    // SGR command: update current style
                    apply_sgr_params(params, &mut current_style);
                }
                i = j + 1;
                segment_start = i;
            } else {
                i += 2;
                segment_start = i;
            }
        } else {
            i += 1;
        }
    }

    if segment_start < len {
        spans.push(Span::styled(&input[segment_start..], current_style));
    }

    if spans.is_empty() {
        vec![Span::raw(input)]
    } else {
        spans
    }
}

/// Parses semicolon-separated ANSI SGR parameters and updates the Style
fn apply_sgr_params(params_str: &str, style: &mut Style) {
    if params_str.is_empty() || params_str == "0" {
        *style = Style::default();
        return;
    }

    let parts: Vec<u32> = params_str
        .split(';')
        .filter_map(|s| s.parse().ok())
        .collect();

    let mut idx = 0;
    while idx < parts.len() {
        match parts[idx] {
            0 => *style = Style::default(),
            1 => *style = style.add_modifier(Modifier::BOLD),
            2 => *style = style.add_modifier(Modifier::DIM),
            3 => *style = style.add_modifier(Modifier::ITALIC),
            4 => *style = style.add_modifier(Modifier::UNDERLINED),
            22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *style = style.remove_modifier(Modifier::ITALIC),
            24 => *style = style.remove_modifier(Modifier::UNDERLINED),

            // Standard Foreground
            30 => *style = style.fg(Color::Black),
            31 => *style = style.fg(Color::Red),
            32 => *style = style.fg(Color::Green),
            33 => *style = style.fg(Color::Yellow),
            34 => *style = style.fg(Color::Blue),
            35 => *style = style.fg(Color::Magenta),
            36 => *style = style.fg(Color::Cyan),
            37 => *style = style.fg(Color::White),
            39 => *style = style.fg(Color::Reset),

            // Standard Background
            40 => *style = style.bg(Color::Black),
            41 => *style = style.bg(Color::Red),
            42 => *style = style.bg(Color::Green),
            43 => *style = style.bg(Color::Yellow),
            44 => *style = style.bg(Color::Blue),
            45 => *style = style.bg(Color::Magenta),
            46 => *style = style.bg(Color::Cyan),
            47 => *style = style.bg(Color::White),
            49 => *style = style.bg(Color::Reset),

            // High-intensity Foreground
            90 => *style = style.fg(Color::DarkGray),
            91 => *style = style.fg(Color::LightRed),
            92 => *style = style.fg(Color::LightGreen),
            93 => *style = style.fg(Color::LightYellow),
            94 => *style = style.fg(Color::LightBlue),
            95 => *style = style.fg(Color::LightMagenta),
            96 => *style = style.fg(Color::LightCyan),
            97 => *style = style.fg(Color::White),

            // High-intensity Background
            100 => *style = style.bg(Color::DarkGray),
            101 => *style = style.bg(Color::LightRed),
            102 => *style = style.bg(Color::LightGreen),
            103 => *style = style.bg(Color::LightYellow),
            104 => *style = style.bg(Color::LightBlue),
            105 => *style = style.bg(Color::LightMagenta),
            106 => *style = style.bg(Color::LightCyan),
            107 => *style = style.bg(Color::White),

            // 256 Color & 24-bit TrueColor Foreground
            38 => {
                if idx + 2 < parts.len() && parts[idx + 1] == 5 {
                    let col_idx = parts[idx + 2] as u8;
                    *style = style.fg(Color::Indexed(col_idx));
                    idx += 2;
                } else if idx + 4 < parts.len() && parts[idx + 1] == 2 {
                    let r = parts[idx + 2] as u8;
                    let g = parts[idx + 3] as u8;
                    let b = parts[idx + 4] as u8;
                    *style = style.fg(Color::Rgb(r, g, b));
                    idx += 4;
                }
            }

            // 256 Color & 24-bit TrueColor Background
            48 => {
                if idx + 2 < parts.len() && parts[idx + 1] == 5 {
                    let col_idx = parts[idx + 2] as u8;
                    *style = style.bg(Color::Indexed(col_idx));
                    idx += 2;
                } else if idx + 4 < parts.len() && parts[idx + 1] == 2 {
                    let r = parts[idx + 2] as u8;
                    let g = parts[idx + 3] as u8;
                    let b = parts[idx + 4] as u8;
                    *style = style.bg(Color::Rgb(r, g, b));
                    idx += 4;
                }
            }

            _ => {}
        }
        idx += 1;
    }
}

/// Highlights substrings matching the search query with an Electric Yellow background
fn highlight_search_matches<'a>(
    spans: Vec<Span<'a>>,
    query: &str,
    _theme: &Theme,
) -> Vec<Span<'a>> {
    // If query is empty, return spans unchanged
    if query.is_empty() {
        return spans;
    }

    let highlight_style = Style::default()
        .bg(SpinmuxPalette::ACCENT_YELLOW)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);

    let query_lower = query.to_lowercase();
    let mut out_spans = Vec::new();

    for span in spans {
        let content = span.content.as_ref();
        let content_lower = content.to_lowercase();

        if !content_lower.contains(&query_lower) {
            out_spans.push(span);
            continue;
        }

        let mut last_idx = 0;
        let original_style = span.style;

        for (match_idx, _) in content_lower.match_indices(&query_lower) {
            if match_idx > last_idx {
                out_spans.push(Span::styled(
                    content[last_idx..match_idx].to_string(),
                    original_style,
                ));
            }
            out_spans.push(Span::styled(
                content[match_idx..match_idx + query.len()].to_string(),
                highlight_style,
            ));
            last_idx = match_idx + query.len();
        }

        if last_idx < content.len() {
            out_spans.push(Span::styled(
                content[last_idx..].to_string(),
                original_style,
            ));
        }
    }

    out_spans
}
