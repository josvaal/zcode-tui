//! Lanzador automático del runtime de ZCode.
//!
//! Detecta un runtime capaz de correr `app-server` (ZCode Protocol stdio):
//! 1. binarios de instalación oficial (`~/.local/bin/zcode`, `~/.zcode/runtime/...`)
//! 2. bundle embebido en la app de escritorio (`/opt/ZCode/resources/glm/zcode.cjs`
//!    ejecutado con `node`) — así la TUI funciona "out of the box" con solo
//!    tener ZCode Desktop instalado
//! 3. binario `zcode` del PATH si responde `--version` limpiamente
//!
//! Si no hay nada, explica cómo instalarlo.

use std::path::{Path, PathBuf};

/// Comando listo para spawnear el app-server.
#[derive(Debug, Clone)]
pub struct RuntimeCommand {
    pub program: String,
    pub args: Vec<String>,
    /// Variables de entorno para heredar la config de providers del usuario
    /// (p. ej. la de ZCode Desktop en `~/.zcode/v2`).
    pub env: Vec<(String, String)>,
}

/// Detecta el entorno de config del usuario: si ZCode Desktop ya guardó sus
/// providers (`~/.zcode/v2/provider_config.json`), los reutilizamos.
fn detect_provider_env() -> Vec<(String, String)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let v2 = PathBuf::from(&home).join(".zcode/v2");
    let personal = v2.join("provider_config.json");
    if !personal.is_file() {
        return Vec::new();
    }
    let mut env = vec![(
        "ZCODE_PERSONAL_PROVIDER_CONFIG_FILE".to_string(),
        personal.display().to_string(),
    )];
    // builtin materializado por la versión del desktop:
    // ~/.zcode/v2/runtime/provider/<plataforma>/<versión>/endpoint-*/zcode-builtin.json
    let runtime_dir = v2.join("runtime/provider");
    if let Some(builtin) = find_builtin_config(&runtime_dir, 0) {
        env.push((
            "ZCODE_BUILTIN_PROVIDER_CONFIG_FILE".to_string(),
            builtin.display().to_string(),
        ));
    }
    env
}

/// Búsqueda recursiva (3 niveles) del zcode-builtin.json más reciente.
fn find_builtin_config(dir: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 3 {
        return None;
    }
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = find_builtin_config(&path, depth + 1) {
                    let mtime = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    if newest.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                        newest = Some((mtime, found));
                    }
                }
            } else if path.file_name().map(|f| f == "zcode-builtin.json").unwrap_or(false) {
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                if newest.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                    newest = Some((mtime, path));
                }
            }
        }
    }
    newest.map(|(_, p)| p)
}

/// Detecta el runtime. `explicit` es la ruta de `--zcode-bin`.
pub fn find_runtime(explicit: Option<&str>) -> Result<RuntimeCommand, String> {
    if let Some(bin) = explicit {
        return Ok(RuntimeCommand {
            program: bin.to_string(),
            args: vec!["app-server".into(), "--surface".into(), "desktop".into()],
            env: detect_provider_env(),
        });
    }

    let home = std::env::var("HOME").unwrap_or_default();

    // instalaciones oficiales del CLI
    for candidate in [
        format!("{home}/.local/bin/zcode"),
        format!("{home}/.zcode/runtime/zcode/bin/zcode"),
    ] {
        let p = PathBuf::from(&candidate);
        if p.is_file() {
            return Ok(RuntimeCommand {
                program: candidate,
                args: vec!["app-server".into(), "--surface".into(), "desktop".into()],
                env: detect_provider_env(),
            });
        }
    }

    // bundle embebido de la app de escritorio (Linux)
    for base in ["/opt/ZCode", "/usr/lib/zcode", &format!("{home}/.local/share/ZCode")] {
        let bundle = PathBuf::from(base).join("resources/glm/zcode.cjs");
        if bundle.is_file() {
            let node = which_node();
            return Ok(RuntimeCommand {
                program: node.ok_or("hay bundle de ZCode Desktop pero no `node` en el PATH")?,
                args: vec![bundle.display().to_string(), "app-server".into(), "--surface".into(), "desktop".into()],
                env: detect_provider_env(),
            });
        }
    }

    // binario del PATH, validado con --version (evita lanzar la GUI de Electron)
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("zcode");
            if candidate.is_file() && looks_like_cli(&candidate) {
                return Ok(RuntimeCommand {
                    program: candidate.display().to_string(),
                    args: vec!["app-server".into(), "--surface".into(), "desktop".into()],
                    env: detect_provider_env(),
                });
            }
        }
    }

    Err(
        "runtime de ZCode no encontrado.\n\n\
         Opciones:\n\
         1. Instala ZCode Desktop (trae el runtime embebido) o el CLI oficial.\n\
         2. Pasa la ruta con --zcode-bin /ruta/a/zcode\n\
         3. Conecta a un servidor ya corriendo con --url ws://host:puerto\n\
         4. Modo demostración: --demo"
            .into(),
    )
}

/// Un binario es CLI si `--version` imprime solo una línea de versión
/// (la app de Electron escupe logs de GUI o abre una ventana).
fn looks_like_cli(bin: &std::path::Path) -> bool {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output();
    match out {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let line = stdout.trim();
            !line.is_empty()
                && line.lines().count() == 1
                && line.chars().next().is_some_and(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

fn which_node() -> Option<String> {
    // ZCode requiere Node >= 24; los nvm antiguos (v22) crashean el app-server.
    // Recopilamos candidatos (PATH + node del sistema) y elegimos el más nuevo.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("node");
            if candidate.is_file() {
                candidates.push(candidate);
            }
        }
    }
    for system in ["/usr/sbin/node", "/usr/bin/node"] {
        let p = PathBuf::from(system);
        if p.is_file() {
            candidates.push(p);
        }
    }
    let mut best: Option<(u32, String)> = None;
    for candidate in candidates {
        let Some(version) = node_major_version(&candidate) else { continue };
        if version < 24 {
            continue;
        }
        if best.as_ref().map(|(v, _)| version > *v).unwrap_or(true) {
            best = Some((version, candidate.display().to_string()));
        }
    }
    best.map(|(_, p)| p)
}

fn node_major_version(bin: &std::path::Path) -> Option<u32> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let major = s.trim().trim_start_matches('v').split('.').next()?;
    major.parse().ok()
}
