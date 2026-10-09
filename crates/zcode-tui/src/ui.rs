//! Layout de la TUI estilo agente de código: mensajes fluyendo sin cajas,
//! input multilinea abajo con altura dinámica, status bar con spinner.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use crate::{app::App, markdown, theme::Theme};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(f: &mut Frame, app: &mut App) {
    let theme = app.theme;
    let area = f.area();

    // altura del input: 3 líneas mín., crece con el contenido hasta 40% de pantalla
    let input_h = (app.input.len() as u16 + 2)
        .max(3)
        .min(area.height.saturating_mul(4) / 10)
        .max(3);

    let main = if app.sidebar.is_some() {
        let [sidebar, rest] = Layout::horizontal([
            Constraint::Percentage(32),
            Constraint::Min(20),
        ])
        .areas(area);
        draw_sidebar(f, app, theme, sidebar);
        rest
    } else {
        area
    };

    let [transcript, input, status] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(input_h),
        Constraint::Length(1),
    ])
    .areas(main);
    draw_transcript(f, app, theme, transcript);
    draw_input(f, app, theme, input);
    draw_status(f, app, theme, status);
    draw_model_picker(f, app, theme, area);
}

fn draw_sidebar(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .map(|si| {
            let is_current = app.current_session.as_deref() == Some(si.session_id.as_str());
            let marker = if is_current { "● " } else { "  " };
            let title: String = si.title.chars().take(34).collect();
            ListItem::new(ratatui::text::Line::from(vec![
                Span::styled(marker, Style::new().fg(theme.accent)),
                Span::styled(
                    title,
                    Style::new().fg(if is_current { theme.accent } else { theme.fg }),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::new().fg(theme.border_active))
                .title(Span::styled(
                    " sesiones — enter: reanudar · esc: cerrar ",
                    Style::new().fg(theme.accent),
                )),
        )
        .highlight_style(
            Style::new()
                .fg(theme.bg)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
    let mut state = ListState::default()
        .with_selected(Some(app.sidebar.unwrap_or(0)));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_model_picker(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let Some(picker) = &app.picker else { return };

    let (title, items, selected) = match picker {
        crate::app::Picker::Models(selected) => {
            let items: Vec<ListItem> = app
                .models
                .iter()
                .map(|m| {
                    ListItem::new(ratatui::text::Line::from(vec![
                        Span::styled(
                            format!(" {} ", m.label),
                            Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(" — {} ", m.provider_label),
                            Style::new().fg(theme.muted),
                        ),
                        Span::styled(
                            match &m.default_level {
                                Some(l) => format!("(default {l})"),
                                None => String::new(),
                            },
                            Style::new().fg(theme.tool),
                        ),
                    ]))
                })
                .collect();
            (
                " modelos — enter: elegir · esc: cerrar ".to_string(),
                items,
                *selected,
            )
        }
        crate::app::Picker::Levels(model, selected) => {
            let items: Vec<ListItem> = app
                .reasoning_levels
                .iter()
                .map(|l| {
                    let marker = if Some(l) == model.default_level.as_ref() {
                        " ← default"
                    } else {
                        ""
                    };
                    ListItem::new(Span::styled(
                        format!(" {l}{marker} "),
                        Style::new().fg(theme.fg),
                    ))
                })
                .collect();
            (
                format!(
                    " nivel para {} — enter: aplicar · esc: volver ",
                    model.label
                ),
                items,
                *selected,
            )
        }
    };

    let rows = match picker {
        crate::app::Picker::Models(_) => app.models.len() as u16,
        crate::app::Picker::Levels(_, _) => app.reasoning_levels.len() as u16,
    };
    let width = (area.width.saturating_sub(8)).min(72).max(30);
    let height = (rows + 4).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup = Rect { x, y, width, height };

    f.render_widget(Clear, popup);
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::new().fg(theme.border_active))
                .title(Span::styled(title.as_str(), Style::new().fg(theme.accent))),
        )
        .highlight_style(
            Style::new()
                .fg(theme.bg)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
    let mut state = ListState::default().with_selected(Some(selected));
    f.render_stateful_widget(list, popup, &mut state);
}

/// Pantalla de bienvenida: solo se ve cuando el transcript está vacío.
fn welcome_lines(theme: Theme, width: u16) -> Vec<Line<'static>> {
    let pad = (width as usize / 2).saturating_sub(26);
    let blank = Line::default();
    let mut out = vec![blank.clone(); 2.min(usize::from(width > 10))];
    for line in [
        "   ╭───────────────────────────────────────────╮",
        "   │                                           │",
        &format!("   │   {:^39}   │", "zcode-tui"),
        &format!("   │   {:^39}   │", "agente de código en tu terminal"),
        "   │                                           │",
        &format!("   │   {:^39}   │", "enter: enviar · ctrl+p: modelo"),
        &format!("   │   {:^39}   │", "ctrl+s: sesiones · ctrl+t: tema"),
        &format!("   │   {:^39}   │", "o: ver/ocultar herramientas"),
        "   │                                           │",
        "   ╰───────────────────────────────────────────╯",
        "",
        &format!("   {:>pad$}{}", "", "escribe un prompt y presiona enter", pad = pad + 3),
    ] {
        out.push(Line::from(Span::styled(line.to_string(), Style::new().fg(theme.border))));
    }
    out
}

fn draw_transcript(f: &mut Frame, app: &mut App, theme: Theme, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    if app.messages.is_empty() {
        lines.extend(welcome_lines(theme, area.width));
    } else {
        for msg in &app.messages {
            match msg.role {
                crate::app::Role::User => {
                    lines.push(Line::from(Span::styled(
                        "❯".to_string(),
                        Style::new()
                            .fg(theme.user)
                            .add_modifier(Modifier::BOLD),
                    )));
                    let mut spans = vec![Span::raw(" ")];
                    spans.extend(markdown::inline(&msg.content, &theme));
                    lines.push(Line::from(spans));
                    lines.push(Line::default());
                }
                crate::app::Role::Assistant => {
                    if msg.kind == crate::app::MsgKind::Reasoning {
                        // thinking: sin caja, indent + muted italic
                        lines.push(Line::from(Span::styled(
                            "  ✻ thinking",
                            Style::new()
                                .fg(theme.muted)
                                .add_modifier(Modifier::ITALIC | Modifier::BOLD),
                        )));
                        for raw in msg.content.lines() {
                            lines.push(Line::from(Span::styled(
                                format!("  {raw}"),
                                Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC),
                            )));
                        }
                        lines.push(Line::default());
                    } else {
                        // marcador del asistente pegado a la primera línea del cuerpo
                        let mut md = markdown::render_markdown(&msg.content, &theme);
                        if let Some(first) = md.first_mut() {
                            let mut spans = vec![Span::styled(
                                "✻ ".to_string(),
                                Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
                            )];
                            spans.append(&mut first.spans);
                            *first = Line::from(spans);
                        }
                        lines.extend(md);
                        lines.push(Line::default());
                    }
                }
                crate::app::Role::Tool => {
                    if msg.expanded {
                        for raw in msg.content.lines() {
                            lines.push(Line::from(Span::styled(
                                format!("  {raw}"),
                                Style::new().fg(theme.tool),
                            )));
                        }
                    } else {
                        // colapsado: primera línea compacta `▸ …`
                        let first = msg.content.lines().next().unwrap_or("").trim();
                        let first = first.strip_prefix("│ ").unwrap_or(first);
                        lines.push(Line::from(Span::styled(
                            format!("  ▸ {first}"),
                            Style::new().fg(theme.muted),
                        )));
                    }
                    lines.push(Line::default());
                }
                crate::app::Role::Diff => {
                    for (i, raw) in msg.content.lines().enumerate() {
                        if i == 0 {
                            let label = raw.trim_start_matches('─').trim();
                            if !msg.expanded {
                                lines.push(Line::from(Span::styled(
                                    format!("  ▸ diff · {label}"),
                                    Style::new().fg(theme.muted),
                                )));
                                break;
                            }
                            lines.push(Line::from(Span::styled(
                                format!("  ─ {label}"),
                                Style::new().fg(theme.tool).add_modifier(Modifier::BOLD),
                            )));
                        } else {
                            let (color, bg) = match raw.chars().next() {
                                Some('+') => (theme.diff_add, Some(theme.code_bg)),
                                Some('-') => (theme.diff_del, Some(theme.code_bg)),
                                _ => (theme.diff_ctx, Some(theme.code_bg)),
                            };
                            let mut style = Style::new().fg(color).bg(bg.unwrap_or(theme.bg));
                            if raw.starts_with('+') || raw.starts_with('-') {
                                style = style.add_modifier(Modifier::BOLD);
                            }
                            lines.push(Line::from(Span::styled(
                                format!("  {raw}"),
                                style,
                            )));
                        }
                    }
                    lines.push(Line::default());
                }
                crate::app::Role::System => {
                    let (color, icon) = if msg.content.starts_with("error") {
                        (theme.error, "✗")
                    } else if msg.content.starts_with("⚠") {
                        (theme.warning, "")
                    } else {
                        (theme.muted, "·")
                    };
                    lines.push(Line::from(Span::styled(
                        format!("  {icon}{}{content}", if icon.is_empty() { "" } else { " " }, content = msg.content),
                        Style::new().fg(color),
                    )));
                    lines.push(Line::default());
                }
            }
        }
    }

    let total = lines.len() as u16;
    let visible = area.height as usize;
    // auto-scroll salvo que el usuario haya subido manualmente
    let max_scroll = total.saturating_sub(visible as u16);
    let scroll = if app.auto_scroll { max_scroll } else { app.scroll.min(max_scroll) };

    let para = Paragraph::new(lines)
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, area);

    if max_scroll > 0 {
        let mut sb = ScrollbarState::new(max_scroll as usize).position(scroll as usize);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut sb,
        );
    }
}

fn draw_input(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let focused = app.focus_input;
    let streaming_border = if app.streaming { theme.accent } else { theme.border };
    let border = if focused { theme.border_active } else { streaming_border };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(Span::styled(
            if focused { " prompt — enter: enviar · alt+enter: nueva línea " } else { " prompt (tab) " },
            Style::new().fg(if focused { theme.accent } else { theme.muted }),
        ));

    let mut lines: Vec<Line> = Vec::new();
    for (i, l) in app.input.iter().enumerate() {
        let mut spans = Vec::new();
        if i == 0 {
            spans.push(Span::styled(
                "❯ ",
                Style::new()
                    .fg(if focused { theme.accent } else { theme.muted })
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(l.clone(), Style::new().fg(theme.fg)));
        lines.push(Line::from(spans));
    }
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, area);

    if focused {
        let inner_h = area.height.saturating_sub(2) as usize;
        let row = app.input_cursor.row.min(inner_h.saturating_sub(1));
        let cursor_x = area.x + 2 + app.input_cursor.col as u16;
        f.set_cursor_position((cursor_x.min(area.right() - 1), area.y + 1 + row as u16));
    }
}

fn draw_status(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let mode = match &app.connector {
        crate::agent::Connector::Demo => "demo",
        crate::agent::Connector::Stdio { .. } => "zcode",
        crate::agent::Connector::Zcode { .. } => "zcode",
    };
    let mut spans = vec![
        Span::styled(format!(" {mode} "), Style::new().fg(theme.bg).bg(theme.accent)),
        Span::raw(" "),
    ];
    if app.streaming {
        let secs = app
            .streaming_since
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let frame = SPINNER[app.tick % SPINNER.len()];
        spans.push(Span::styled(
            format!("{frame} trabajando · {secs}s"),
            Style::new().fg(theme.accent),
        ));
    } else {
        spans.push(Span::styled("○ listo", Style::new().fg(theme.muted)));
    }
    spans.push(Span::styled(
        format!("  · tema: {}", theme.name),
        Style::new().fg(theme.muted),
    ));
    spans.push(Span::styled(
        "  · ctrl+p:modelo ctrl+s:sesiones o:tools q:salir",
        Style::new().fg(theme.muted),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
