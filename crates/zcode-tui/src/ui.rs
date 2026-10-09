//! Layout de la TUI: transcript central, input abajo, barra de estado.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
    Frame,
};

use crate::{app::App, markdown, theme::Theme};

pub fn draw(f: &mut Frame, app: &mut App) {
    let theme = app.theme;
    let area = f.area();

    // sidebar de sesiones a la izquierda cuando está abierta
    if app.sidebar.is_some() {
        let [sidebar, rest] = Layout::horizontal([
            Constraint::Percentage(32),
            Constraint::Min(20),
        ])
        .areas(area);
        draw_sidebar(f, app, theme, sidebar);
        let [transcript, input, status] = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(rest);
        draw_transcript(f, app, theme, transcript);
        draw_input(f, app, theme, input);
        draw_status(f, app, theme, status);
    } else {
        let [transcript, input, status] = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(area);
        draw_transcript(f, app, theme, transcript);
        draw_input(f, app, theme, input);
        draw_status(f, app, theme, status);
    }
    draw_model_picker(f, app, theme, area);
}

fn draw_sidebar(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    use ratatui::widgets::{List, ListItem, ListState};
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
    use ratatui::widgets::{Clear, List, ListItem, ListState};

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

fn draw_transcript(f: &mut Frame, app: &mut App, theme: Theme, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for msg in &app.messages {
        match msg.role {
            crate::app::Role::User => {
                lines.push(Line::from(Span::styled(
                    format!("{} ▷", app.user_label),
                    Style::new().fg(theme.user).add_modifier(Modifier::BOLD),
                )));
                lines.extend(wrap_spans(markdown::inline(&msg.content, &theme), 4));
                lines.push(Line::default());
            }
            crate::app::Role::Assistant => {
                if msg.kind == crate::app::MsgKind::Reasoning {
                    // bloque de thinking: marco propio atenuado, inconfundible
                    lines.push(Line::from(Span::styled(
                        "╭─ ✻ thinking ",
                        Style::new()
                            .fg(theme.muted)
                            .add_modifier(Modifier::ITALIC | Modifier::BOLD),
                    )));
                    for raw in msg.content.lines() {
                        lines.push(Line::from(Span::styled(
                            format!("│ {raw}"),
                            Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC),
                        )));
                    }
                    lines.push(Line::from(Span::styled(
                        "╰─",
                        Style::new().fg(theme.muted),
                    )));
                    lines.push(Line::default());
                } else {
                    lines.push(Line::from(Span::styled(
                        "zcode ◂",
                        Style::new().fg(theme.assistant).add_modifier(Modifier::BOLD),
                    )));
                    lines.extend(markdown::render_markdown(&msg.content, &theme));
                    lines.push(Line::default());
                }
            }
            crate::app::Role::Tool => {
                for raw in msg.content.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {raw}"),
                        Style::new().fg(theme.tool),
                    )));
                }
                lines.push(Line::default());
            }
            crate::app::Role::Diff => {
                for (i, raw) in msg.content.lines().enumerate() {
                    let line = if i == 0 {
                        // cabecera: ─ path +N −M
                        Line::from(Span::styled(
                            format!("  {raw}"),
                            Style::new().fg(theme.tool).add_modifier(Modifier::BOLD),
                        ))
                    } else {
                        let (style, text) = match raw.chars().next() {
                            Some('+') => (
                                Style::new().fg(theme.diff_add).bg(theme.code_bg),
                                format!("  {raw}"),
                            ),
                            Some('-') => (
                                Style::new().fg(theme.diff_del).bg(theme.code_bg),
                                format!("  {raw}"),
                            ),
                            _ => (
                                Style::new().fg(theme.muted).bg(theme.code_bg),
                                format!("  {raw}"),
                            ),
                        };
                        Line::from(Span::styled(text, style))
                    };
                    lines.push(line);
                }
                lines.push(Line::default());
            }
            crate::app::Role::System => {
                lines.push(Line::from(Span::styled(
                    format!("· {}", msg.content),
                    Style::new().fg(theme.muted),
                )));
                lines.push(Line::default());
            }
        }
    }

    let total = lines.len() as u16;
    let visible = area.height.saturating_sub(2) as usize;
    // auto-scroll salvo que el usuario haya subido manualmente
    let max_scroll = total.saturating_sub(visible as u16);
    let scroll = if app.auto_scroll { max_scroll } else { app.scroll.min(max_scroll) };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::new().fg(theme.border))
        .title(Span::styled(
            " zcode-tui ",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            format!(" {}/{} ", scroll + (visible.min(total as usize) as u16), total),
            Style::new().fg(theme.muted),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let para = Paragraph::new(lines)
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(para, inner);

    if max_scroll > 0 {
        let mut sb = ScrollbarState::new(max_scroll as usize).position(scroll as usize);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut sb,
        );
    }
}

fn wrap_spans(spans: Vec<Span<'static>>, indent: usize) -> Vec<Line<'static>> {
    // wrap simple por ancho fijo se maneja en Paragraph; aquí solo indentamos
    let pad = " ".repeat(indent);
    vec![Line::from(vec![Span::raw(pad)].into_iter().chain(spans).collect::<Vec<_>>())]
}

fn draw_input(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let focused = app.focus_input;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::new().fg(if focused { theme.border_active } else { theme.border }))
        .title(Span::styled(
            if focused { " prompt " } else { " prompt (tab) " },
            Style::new().fg(if focused { theme.accent } else { theme.muted }),
        ));
    let para = Paragraph::new(app.input.clone())
        .block(block)
        .style(Style::new().fg(theme.fg));
    f.render_widget(para, area);
    if focused {
        let cursor_x = area.x + 1 + app.input.chars().count() as u16;
        f.set_cursor_position((cursor_x.min(area.right() - 1), area.y + 1));
    }
}

fn draw_status(f: &mut Frame, app: &App, theme: Theme, area: Rect) {
    let mode = match &app.connector {
        crate::agent::Connector::Demo => "demo",
        crate::agent::Connector::Stdio { .. } => "zcode",
        crate::agent::Connector::Zcode { .. } => "zcode",
    };
    let state = if app.streaming {
        let secs = app
            .streaming_since
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        format!("● streaming ({secs}s)")
    } else {
        "○ idle".to_string()
    };
    let line = Line::from(vec![
        Span::styled(format!(" {mode} "), Style::new().fg(theme.bg).bg(theme.accent)),
        Span::raw(" "),
        Span::styled(
            state,
            Style::new().fg(if app.streaming { theme.error } else { theme.muted }),
        ),
        Span::styled("  enter:enviar · pgup/pgdn:scroll · tab+m:rueda · q:salir", Style::new().fg(theme.muted)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}
