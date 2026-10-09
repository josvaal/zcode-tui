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
    draw_modal(f, app, theme, area);
    draw_user_input(f, app, theme, area);
}

/// Modal de AskUserQuestion: pregunta(s) con opciones numeradas.
fn draw_user_input(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    use ratatui::widgets::Clear;
    let Some(ui) = &app.pending_user_input else { return };
    let Some(q) = ui.questions.get(ui.question_idx) else { return };

    let mut lines: Vec<Line> = Vec::new();
    if ui.questions.len() > 1 {
        lines.push(Line::from(Span::styled(
            format!(" pregunta {}/{}", ui.question_idx + 1, ui.questions.len()),
            Style::new().fg(theme.muted),
        )));
    }
    lines.push(Line::from(vec![
        Span::styled("❓ ", Style::new().fg(theme.warning).add_modifier(Modifier::BOLD)),
        Span::styled(q.question.clone(), Style::new().fg(theme.fg).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::default());
    for (i, (_value, label)) in q.options.iter().enumerate() {
        let selected = i == ui.option_idx;
        let style = if selected {
            Style::new().fg(theme.bg).bg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.fg)
        };
        lines.push(Line::from(Span::styled(
            format!(" {} {} ", i + 1, label),
            style,
        )));
    }
    lines.push(Line::default());
    let hint = if ui.questions.len() > 1 {
        "1-9: elegir · ↑↓: pregunta · esc: cancelar "
    } else {
        "1-9: elegir · esc: cancelar "
    };
    lines.push(Line::from(Span::styled(hint, Style::new().fg(theme.muted))));

    let width = (area.width.saturating_sub(8)).min(70).max(34);
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)).max(1) / 2;
    let popup = Rect { x, y, width, height };
    f.render_widget(Clear, popup);
    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::new().fg(theme.warning))
            .title(Span::styled(" input requerido ", Style::new().fg(theme.warning))),
    );
    f.render_widget(para, popup);
}

/// Modal genérico con búsqueda difusa (comandos `/`, archivos `@`, skills `$`).
fn draw_modal(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    use ratatui::widgets::Clear;
    let Some(modal) = &app.modal else { return };

    let (title, query, sel) = match modal {
        crate::app::Modal::Commands { sel, query } => {
            (" comandos — enter: ejecutar · esc: cerrar ", query, *sel)
        }
        crate::app::Modal::Files { sel, query } => {
            (" archivos — enter: insertar ruta · esc: cerrar ", query, *sel)
        }
        crate::app::Modal::Skills { sel, query } => {
            (" skills — enter: insertar en el prompt · esc: cerrar ", query, *sel)
        }
    };

    let filtered = app.modal_filtered();
    let items = app.modal_items();

    let width = (area.width.saturating_sub(8)).min(76).max(34);
    let height = (filtered.len() as u16 + 5).min(area.height.saturating_sub(4)).max(6);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)).max(1) / 2;
    let popup = Rect { x, y, width, height };

    let inner_h = height.saturating_sub(4) as usize;
    let mut visible: Vec<Line> = filtered
        .iter()
        .enumerate()
        .map(|(vis_i, &idx)| {
            let style = if vis_i == sel {
                Style::new().fg(theme.bg).bg(theme.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme.fg)
            };
            Line::from(Span::styled(format!(" {}", items[idx]), style))
        })
        .collect();
    let skip = sel.saturating_sub(inner_h.saturating_sub(1));
    visible = visible.into_iter().skip(skip).take(inner_h).collect();

    f.render_widget(Clear, popup);

    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled("› ", Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)),
        Span::styled(query.clone(), Style::new().fg(theme.fg)),
        Span::styled("▏", Style::new().fg(theme.accent)),
    ])];
    if visible.is_empty() {
        lines.push(Line::from(Span::styled(
            "  sin resultados",
            Style::new().fg(theme.muted),
        )));
    } else {
        lines.extend(visible);
    }
    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::new().fg(theme.border_active))
            .title(Span::styled(title, Style::new().fg(theme.accent))),
    );
    f.render_widget(para, popup);
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
                    " sesiones — enter: reanudar · r: renombrar · d×2: borrar · esc: cerrar ",
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
        crate::app::Picker::Mode(selected) => {
            const MODES: [(&str, &str); 4] = [
                ("build", "pregunta antes de cambios"),
                ("edit", "edita archivos automáticamente"),
                ("plan", "solo planifica, no ejecuta"),
                ("yolo", "acceso total, sin permisos"),
            ];
            let items: Vec<ListItem> = MODES
                .iter()
                .map(|(name, hint)| {
                    let is_current = *name == app.mode;
                    let marker = if is_current { "● " } else { "  " };
                    ListItem::new(ratatui::text::Line::from(vec![
                        Span::styled(
                            format!(" {marker}{name} "),
                            Style::new()
                                .fg(theme.fg)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("— {hint} "), Style::new().fg(theme.muted)),
                    ]))
                })
                .collect();
            (
                " modo — enter: aplicar · esc: cerrar ".to_string(),
                items,
                *selected,
            )
        }
    };

    let rows = match picker {
        crate::app::Picker::Models(_) => app.models.len() as u16,
        crate::app::Picker::Levels(_, _) => app.reasoning_levels.len() as u16,
        crate::app::Picker::Mode(_) => 4,
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
fn welcome_lines(theme: Theme) -> Vec<Line<'static>> {
    fn center(text: &str, width: usize) -> String {
        let len = text.chars().count();
        let left = (width - len) / 2;
        format!("{}{}{}", " ".repeat(left), text, " ".repeat(width - len - left))
    }
    const W: usize = 37;
    let body = [
        center("zcode-tui", W),
        center("agente de código en tu terminal", W),
        String::new(),
        center("enter: enviar · ctrl+p: modelo", W),
        center("ctrl+s: sesiones · ctrl+t: tema", W),
        center("/: comandos · @: archivos · $: skills", W),
        center("o: ver/ocultar herramientas", W),
    ];
    let mut out = vec![Line::default(), Line::default()];
    out.push(Line::from(Span::styled(
        format!("   ╭─{}─╮", "─".repeat(W)),
        Style::new().fg(theme.border),
    )));
    out.push(Line::from(Span::styled(
        format!("   │ {} │", " ".repeat(W)),
        Style::new().fg(theme.border),
    )));
    for (i, row) in body.iter().enumerate() {
        let style = if i == 0 {
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(if row.is_empty() { theme.border } else { theme.muted })
        };
        out.push(Line::from(Span::styled(
            format!("   │ {} │", row),
            style,
        )));
    }
    out.push(Line::from(Span::styled(
        format!("   │ {} │", " ".repeat(W)),
        Style::new().fg(theme.border),
    )));
    out.push(Line::from(Span::styled(
        format!("   ╰─{}─╯", "─".repeat(W)),
        Style::new().fg(theme.border),
    )));
    out
}

fn draw_transcript(f: &mut Frame, app: &mut App, theme: Theme, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    if app.messages.is_empty() {
        lines.extend(welcome_lines(theme));
    } else {
        for msg in &app.messages {
            match msg.role {
                crate::app::Role::User => {
                    // marcador y texto en la misma línea
                    let mut spans = vec![Span::styled(
                        "❯ ".to_string(),
                        Style::new()
                            .fg(theme.user)
                            .add_modifier(Modifier::BOLD),
                    )];
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
        // chip del modo de colaboración
        Span::styled(
            format!(" {} ", app.mode),
            Style::new().fg(theme.bg).bg(theme.tool),
        ),
        Span::raw(" "),
    ];
    if app.streaming {
        let secs = app
            .streaming_since
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let frame = SPINNER[app.tick % SPINNER.len()];
        spans.push(Span::styled(
            format!("{frame} trabajando · {secs}s · esc detiene"),
            Style::new().fg(theme.accent),
        ));
    } else {
        spans.push(Span::styled("○ listo", Style::new().fg(theme.muted)));
    }
    if !app.queue.is_empty() {
        spans.push(Span::styled(
            format!(" · en cola: {}", app.queue.len()),
            Style::new().fg(theme.warning),
        ));
    }
    if let Some(usage) = &app.usage_text {
        spans.push(Span::styled(
            format!(" · {usage}"),
            Style::new().fg(theme.muted),
        ));
    }
    spans.push(Span::styled(
        format!("  · tema: {}", theme.name),
        Style::new().fg(theme.muted),
    ));
    spans.push(Span::styled(
        "  · /:comandos @:archivos $:skills ctrl+o:modo",
        Style::new().fg(theme.muted),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
