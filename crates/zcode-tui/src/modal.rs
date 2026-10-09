//! Modals estilo opencode: paleta de comandos (`/`), archivos (`@`) y
//! skills (`$`), todos con búsqueda difusa (subsequence con scoring).

use std::path::{Path, PathBuf};

/// Una skill descubierta en disco.
#[derive(Clone)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// Scoring fuzzy: None si `needle` no es subsecuencia de `hay`;
/// si lo es, puntaje mayor = mejor match (consecutividad y comienzo de palabra).
pub fn fuzzy_score(needle: &str, hay: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let n: Vec<char> = needle.to_lowercase().chars().collect();
    let h: Vec<char> = hay.to_lowercase().chars().collect();
    let mut score = 0;
    let mut h_idx = 0usize;
    let mut last_match: Option<usize> = None;
    for &c in &n {
        let found = h[h_idx..].iter().position(|&hc| hc == c)? + h_idx;
        // bonus por consecutividad
        if last_match == Some(found.wrapping_sub(1)) {
            score += 5;
        }
        // bonus por comienzo de palabra
        if found == 0 || !h[found - 1].is_alphanumeric() {
            score += 3;
        }
        score -= (found - h_idx).min(10) as i32;
        h_idx = found + 1;
        last_match = Some(found);
    }
    Some(score)
}

/// Filtra `items` por `query` y devuelve los índices ordenados por puntaje.
pub fn filter_indices(query: &str, items: &[String]) -> Vec<usize> {
    let mut scored: Vec<(i32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| fuzzy_score(query, it).map(|s| (s, i)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Descubre skills: SKILL.md con frontmatter YAML (name/description) en
/// ~/.zcode/skills, ~/.agents/skills y sus equivalentes del workspace.
pub fn discover_skills(workspace: &Path) -> Vec<SkillInfo> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(&home).join(".zcode").join("skills"));
        roots.push(PathBuf::from(&home).join(".agents").join("skills"));
    }
    roots.push(workspace.join(".zcode").join("skills"));
    roots.push(workspace.join(".agents").join("skills"));

    let mut out: Vec<SkillInfo> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let (name, description) = parse_frontmatter(&skill_md)
                .unwrap_or((dir_name.clone(), String::new()));
            if seen.insert(name.clone()) {
                out.push(SkillInfo {
                    name,
                    description,
                    path: skill_md,
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Extrae `name` y `description` del frontmatter YAML de un SKILL.md.
fn parse_frontmatter(path: &Path) -> Option<(String, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let rest = content.strip_prefix("---")?;
    let mut name = None;
    let mut description = None;
    for line in rest.lines() {
        if line.trim() == "---" {
            break;
        }
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().trim_matches('"').trim_matches('\'').to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            description =
                Some(v.trim().trim_matches('"').trim_matches('\'').to_string());
        }
    }
    Some((name?, description.unwrap_or_default()))
}

/// Lista de archivos relativos del workspace (para el modal `@`).
/// Salta directorios de build/venv comunes y topa la cantidad.
pub fn discover_files(workspace: &Path) -> Vec<String> {
    const SKIP_DIRS: &[&str] = &[
        ".git", "node_modules", "target", "dist", "build", ".venv", "venv",
        "__pycache__", ".next", ".cache", "coverage",
    ];
    let mut out = Vec::new();
    walk(workspace, workspace, SKIP_DIRS, 0, 8, 3000, &mut out);
    out.sort();
    out
}

fn walk(
    root: &Path,
    dir: &Path,
    skip: &[&str],
    depth: usize,
    max_depth: usize,
    cap: usize,
    out: &mut Vec<String>,
) {
    if depth > max_depth || out.len() >= cap {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= cap {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if skip.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            walk(root, &path, skip, depth + 1, max_depth, cap, out);
        } else if !name.starts_with('.') {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_subsequence() {
        assert!(fuzzy_score("mdl", "modelo").is_some());
        assert!(fuzzy_score("xyz", "modelo").is_none());
        assert_eq!(fuzzy_score("", "cualquiera"), Some(0));
    }

    #[test]
    fn fuzzy_prefiere_consecutivo() {
        assert!(fuzzy_score("mod", "modelo").unwrap() > fuzzy_score("mod", "maod").unwrap());
    }

    #[test]
    fn filter_ordena_por_puntaje() {
        let items = vec![
            "tool calls".to_string(),
            "modelo".to_string(),
            "modo oscuro".to_string(),
        ];
        let idx = filter_indices("mod", &items);
        assert_eq!(idx[0], 1); // "modelo" gana por consecutividad
    }

    #[test]
    fn discover_files_salta_node_modules() {
        let tmp = std::env::temp_dir().join("zcode-tui-files-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::create_dir_all(tmp.join("node_modules/x")).unwrap();
        std::fs::write(tmp.join("src/main.rs"), "").unwrap();
        std::fs::write(tmp.join("node_modules/x/y.js"), "").unwrap();
        let files = discover_files(&tmp);
        assert_eq!(files, vec!["src/main.rs".to_string()]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
