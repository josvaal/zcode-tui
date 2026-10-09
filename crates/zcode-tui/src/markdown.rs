//! Render de markdown ligero a líneas estilo `Line<'static>` de ratatui.
//! Soporta: encabezados, negrita, cursiva, código inline, bloques de código,
//! listas (con anidación), listas numeradas, tablas simples, citas, enlaces
//! y separadores.

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::theme::Theme;

pub fn render_markdown(src: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code: Vec<String> = Vec::new();
    // tabla en curso: filas de celdas
    let mut table: Vec<Vec<String>> = Vec::new();

    let flush_table = |out: &mut Vec<Line<'static>>, rows: &mut Vec<Vec<String>>| {
        if rows.is_empty() {
            return;
        }
        out.extend(render_table(rows, theme));
        rows.clear();
    };

    for raw in src.lines() {
        if raw.trim_start().starts_with("```") {
            if in_code {
                out.extend(render_code_block(&code, &code_lang, theme));
                code.clear();
                in_code = false;
            } else {
                flush_table(&mut out, &mut table);
                in_code = true;
                code_lang = raw.trim_start_matches('`').trim().to_string();
            }
            continue;
        }
        if in_code {
            code.push(raw.to_string());
            continue;
        }
        // líneas de tabla: | a | b | (el separador |---|---| se descarta)
        let trimmed = raw.trim();
        if trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() > 1 {
            let cells: Vec<String> = trimmed
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim().to_string())
                .collect();
            if cells.iter().all(|c| c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' ')) {
                continue; // separador
            }
            table.push(cells);
            continue;
        }
        flush_table(&mut out, &mut table);

        if let Some(h) = raw.strip_prefix("### ") {
            out.push(header(h, 2, theme));
        } else if let Some(h) = raw.strip_prefix("## ") {
            out.push(header(h, 1, theme));
        } else if let Some(h) = raw.strip_prefix("# ") {
            out.push(header(h, 0, theme));
        } else if raw.trim() == "---" || raw.trim() == "***" {
            out.push(Line::from(Span::styled(
                "─────────────────────────────",
                Style::new().fg(theme.border),
            )));
        } else if let Some(q) = raw.strip_prefix("> ").or_else(|| {
            if raw.trim() == ">" { Some("") } else { None }
        }) {
            let mut spans = vec![Span::styled("▌ ", Style::new().fg(theme.accent))];
            spans.extend(inline(q, theme));
            out.push(Line::from(spans));
        } else if let Some((indent, marker, rest)) = parse_list_item(raw) {
            let bullet = Span::styled(
                format!("{}{marker} ", "  ".repeat(indent)),
                Style::new().fg(theme.accent),
            );
            let mut spans = vec![bullet];
            spans.extend(inline(rest, theme));
            out.push(Line::from(spans));
        } else if raw.trim().is_empty() {
            out.push(Line::default());
        } else {
            out.push(Line::from(inline(raw, theme)));
        }
    }
    if in_code && !code.is_empty() {
        out.extend(render_code_block(&code, &code_lang, theme));
    }
    flush_table(&mut out, &mut table);
    out
}

fn header(text: &str, level: usize, theme: &Theme) -> Line<'static> {
    let (style, spacing) = match level {
        0 => (
            Style::new()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            true,
        ),
        1 => (Style::new().fg(theme.accent).add_modifier(Modifier::BOLD), false),
        _ => (Style::new().fg(theme.fg).add_modifier(Modifier::BOLD), false),
    };
    let mut spans = vec![Span::styled(format!("{text}"), style)];
    if spacing {
        spans.push(Span::default());
    }
    Line::from(spans)
}

/// Detecta `- item` / `* item` / `1. item` con anidación por indentación
/// (cada 2 espacios). Devuelve (nivel, viñeta, resto).
fn parse_list_item(raw: &str) -> Option<(usize, String, &str)> {
    let indent = raw.len() - raw.trim_start().len();
    let t = raw.trim_start();
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        Some((indent / 2, "•".to_string(), rest))
    } else {
        // lista numerada: dígitos + '.' o ')'
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 {
            let after = &t[digits..];
            if let Some(rest) = after.strip_prefix(". ").or_else(|| after.strip_prefix(") ")) {
                let num = &t[..digits];
                return Some((indent / 2, format!("{num}."), rest));
            }
        }
        None
    }
}

fn render_table(rows: &[Vec<String>], theme: &Theme) -> Vec<Line<'static>> {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    let mut widths = vec![0usize; cols];
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(cols) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let mut out = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        let mut spans = Vec::new();
        if ri == 0 {
            // cabecera con subrayado
            for (i, cell) in row.iter().enumerate().take(cols) {
                spans.push(Span::styled(
                    format!(" {:<width$} ", cell, width = widths[i]),
                    Style::new()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ));
                if i + 1 < cols {
                    spans.push(Span::styled("│", Style::new().fg(theme.border)));
                }
            }
        } else {
            for (i, cell) in row.iter().enumerate().take(cols) {
                spans.push(Span::styled(
                    format!(" {:<width$} ", cell, width = widths[i]),
                    Style::new().fg(theme.fg),
                ));
                if i + 1 < cols {
                    spans.push(Span::styled("│", Style::new().fg(theme.border)));
                }
            }
        }
        out.push(Line::from(spans));
    }
    out
}

fn render_code_block(code: &[String], lang: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let label = if lang.is_empty() { "code" } else { lang };
    out.push(Line::from(Span::styled(
        format!("╭─ {label} "),
        Style::new().fg(theme.tool),
    )));
    for line in code {
        out.push(Line::from(vec![
            Span::styled("│ ", Style::new().fg(theme.border)),
            Span::styled(line.clone(), Style::new().fg(theme.fg).bg(theme.code_bg)),
        ]));
    }
    out.push(Line::from(Span::styled("╰─", Style::new().fg(theme.tool))));
    out
}

/// Inline: `código`, **negrita**, *cursiva*, [texto](url).
pub fn inline(src: &str, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let rest = src.to_string();
    inline_into(&mut spans, &rest, theme);
    if spans.is_empty() {
        spans.push(Span::default());
    }
    spans
}

fn push_plain(spans: &mut Vec<Span<'static>>, text: &str, theme: &Theme) {
    if text.is_empty() {
        return;
    }
    spans.push(Span::styled(text.to_string(), Style::new().fg(theme.fg)));
}

fn inline_into(spans: &mut Vec<Span<'static>>, rest: &str, theme: &Theme) {
    let mut rest = rest.to_string();
    while !rest.is_empty() {
        // `[texto](url)` en cualquier posición
        if let Some(pos) = rest.find('[') {
            let after = &rest[pos + 1..];
            if let Some(close_bracket) = after.find(']') {
                if after[close_bracket + 1..].starts_with('(') {
                    if let Some(close_paren) = after[close_bracket + 1..].find(')') {
                        let text = after[..close_bracket].to_string();
                        let url = after[close_bracket + 2..close_bracket + 1 + close_paren]
                            .to_string();
                        if pos > 0 {
                            push_plain(spans, &rest[..pos], theme);
                        }
                        if !text.is_empty() {
                            spans.push(Span::styled(
                                text,
                                Style::new()
                                    .fg(theme.link)
                                    .add_modifier(Modifier::UNDERLINED),
                            ));
                        }
                        spans.push(Span::styled(
                            format!(" ({url})"),
                            Style::new().fg(theme.muted),
                        ));
                        rest = rest[pos + 1 + close_bracket + 1 + close_paren + 1..].to_string();
                        continue;
                    }
                }
            }
            // '[' sin formato de link: consumirlo como texto normal
            push_plain(spans, &rest[..pos + 1], theme);
            rest = rest[pos + 1..].to_string();
            continue;
        }
        if let Some(pos) = rest.find('`') {
            if let Some(end_rel) = rest[pos + 1..].find('`') {
                if pos > 0 {
                    push_plain(spans, &rest[..pos], theme);
                }
                let code = rest[pos + 1..pos + 1 + end_rel].to_string();
                spans.push(Span::styled(
                    code,
                    Style::new()
                        .fg(theme.accent)
                        .bg(theme.code_bg)
                        .add_modifier(Modifier::BOLD),
                ));
                rest = rest[pos + end_rel + 2..].to_string();
                continue;
            }
        }
        if let Some(pos) = rest.find("**") {
            if let Some(end_rel) = rest[pos + 2..].find("**") {
                if pos > 0 {
                    push_plain(spans, &rest[..pos], theme);
                }
                let bold = rest[pos + 2..pos + 2 + end_rel].to_string();
                spans.push(Span::styled(
                    bold,
                    Style::new().fg(theme.fg).add_modifier(Modifier::BOLD),
                ));
                rest = rest[pos + end_rel + 4..].to_string();
                continue;
            }
        }
        // cursiva *texto* — solo si no es parte de ** y hay texto pegado a ambos *
        if let Some(pos) = rest.find('*') {
            // evitar confundir con ** (ya tratado arriba, pero un ** sin par cae aquí)
            if !rest[pos..].starts_with("**") {
                if let Some(end_rel) = rest[pos + 1..].find('*') {
                    let inner = &rest[pos + 1..pos + 1 + end_rel];
                    if !inner.is_empty() && !inner.contains('*') {
                        if pos > 0 {
                            push_plain(spans, &rest[..pos], theme);
                        }
                        spans.push(Span::styled(
                            inner.to_string(),
                            Style::new().fg(theme.fg).add_modifier(Modifier::ITALIC),
                        ));
                        rest = rest[pos + end_rel + 2..].to_string();
                        continue;
                    }
                }
            }
        }
        push_plain(spans, &rest, theme);
        break;
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_inline_code() {
        let theme = crate::theme::DARK;
        let lines = render_markdown("usa `cargo test` **ahora**", &theme);
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert!(spans.iter().any(|s| s.content.contains("cargo test")));
        assert!(spans.iter().any(|s| s.content.contains("ahora")));
    }

    #[test]
    fn renders_code_block() {
        let theme = crate::theme::DARK;
        let lines = render_markdown("```rust\nfn main() {}\n```", &theme);
        assert!(lines.len() >= 3);
        assert!(lines[0].spans[0].content.contains("rust"));
    }

    #[test]
    fn renders_italic() {
        let theme = crate::theme::DARK;
        let lines = render_markdown("esto es *importante* vaya", &theme);
        let spans = &lines[0].spans;
        assert!(spans.iter().any(|s| s.content == "importante"));
        let it = spans.iter().find(|s| s.content == "importante").unwrap();
        assert!(it.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn renders_numbered_list() {
        let theme = crate::theme::DARK;
        let lines = render_markdown("1. uno\n2. dos", &theme);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].spans[0].content.trim() == "1.");
        assert!(lines[1].spans[0].content.trim() == "2.");
    }

    #[test]
    fn renders_nested_list() {
        let theme = crate::theme::DARK;
        let lines = render_markdown("- a\n  - b", &theme);
        assert_eq!(lines.len(), 2);
        // el anidado tiene más indent que el padre
        assert!(lines[0].spans[0].content.starts_with("•"));
        assert!(lines[1].spans[0].content.starts_with("  •"));
    }

    #[test]
    fn renders_table() {
        let theme = crate::theme::DARK;
        let md = "| a | bb |\n|---|----|\n| 1 | 2  |";
        let lines = render_markdown(md, &theme);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].spans.iter().any(|s| s.content.contains("bb")));
        assert!(lines[1].spans.iter().any(|s| s.content.contains(" 2  ")));
    }

    #[test]
    fn renders_link() {
        let theme = crate::theme::DARK;
        let lines = render_markdown("mira [esto](https://x.com) ya", &theme);
        let spans = &lines[0].spans;
        assert!(spans.iter().any(|s| s.content == "esto"));
        assert!(spans.iter().any(|s| s.content.contains("https://x.com")));
    }

    #[test]
    fn renders_quote() {
        let theme = crate::theme::DARK;
        let lines = render_markdown("> cita", &theme);
        assert!(lines[0].spans[0].content.contains("▌"));
    }
}
