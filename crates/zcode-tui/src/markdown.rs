//! Render de markdown ligero a líneas estilo `Line<'static>` de ratatui.
//! Soporta: encabezados, negrita, cursiva, código inline, bloques de código.

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

    for raw in src.lines() {
        if raw.trim_start().starts_with("```") {
            if in_code {
                out.extend(render_code_block(&code, &code_lang, theme));
                code.clear();
                in_code = false;
            } else {
                in_code = true;
                code_lang = raw.trim_start_matches('`').trim().to_string();
            }
            continue;
        }
        if in_code {
            code.push(raw.to_string());
            continue;
        }
        if let Some(h) = raw.strip_prefix("### ") {
            out.push(header(h, 2, theme));
        } else if let Some(h) = raw.strip_prefix("## ") {
            out.push(header(h, 1, theme));
        } else if let Some(h) = raw.strip_prefix("# ") {
            out.push(header(h, 0, theme));
        } else if raw.trim() == "---" {
            out.push(Line::from(Span::styled(
                "─────────────────────────────",
                Style::new().fg(theme.muted),
            )));
        } else if raw.starts_with("- ") || raw.starts_with("* ") {
            let mut spans = vec![Span::styled("  • ", Style::new().fg(theme.accent))];
            spans.extend(inline(&raw[2..], theme));
            out.push(Line::from(spans));
        } else {
            out.push(Line::from(inline(raw, theme)));
        }
    }
    if in_code && !code.is_empty() {
        out.extend(render_code_block(&code, &code_lang, theme));
    }
    out
}

fn header(text: &str, level: usize, theme: &Theme) -> Line<'static> {
    let size = match level {
        0 => Modifier::BOLD.union(Modifier::UNDERLINED),
        1 => Modifier::BOLD,
        _ => Modifier::BOLD,
    };
    let prefix = match level {
        0 => "◤ ",
        1 => "◢ ",
        _ => "",
    };
    let mut spans = vec![Span::styled(
        format!("{prefix}{text}"),
        Style::new().fg(theme.accent).add_modifier(size),
    )];
    if level == 0 {
        spans.push(Span::default());
    }
    Line::from(spans)
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

/// Inline: `código`, **negrita**, *cursiva*.
pub fn inline(src: &str, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = src.to_string();
    while !rest.is_empty() {
        if let Some(pos) = rest.find('`') {
            if let Some(end_rel) = rest[pos + 1..].find('`') {
                if pos > 0 {
                    spans.push(Span::styled(
                        rest[..pos].to_string(),
                        Style::new().fg(theme.fg),
                    ));
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
                    spans.push(Span::styled(
                        rest[..pos].to_string(),
                        Style::new().fg(theme.fg),
                    ));
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
        spans.push(Span::styled(rest.clone(), Style::new().fg(theme.fg)));
        break;
    }
    if spans.is_empty() {
        spans.push(Span::default());
    }
    spans
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
}
