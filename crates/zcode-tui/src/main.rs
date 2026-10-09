//! zcode-tui: cliente TUI en Rust para el harness de ZCode.
//!
//! Sin argumentos: detecta el runtime (CLI o bundle de ZCode Desktop), lanza
//! `app-server` en modo headless y se conecta automáticamente. Flags:
//!   --url ws://host:puerto   conecta vía WebSocket a un servidor ya corriendo
//!   --token TOKEN            token de autenticación del servidor
//!   --workspace /ruta        workspace del agente (default: cwd)
//!   --demo                   modo demostración sin runtime
//!   --zcode-bin /ruta        ruta explícita al binario zcode (CLI)

mod agent;
mod app;
mod frame_projector;
mod markdown;
mod stdio_agent;
mod theme;
mod ui;
mod zcode_agent;

use agent::{connect_stdio, connect_zcode, Connector};
use app::App;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|p| args.get(p + 1))
            .cloned()
    };
    let has_flag = |name: &str| args.iter().any(|a| a == name);

    let workspace = flag("--workspace").unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string()
    });

    let (connector, frame_rx) = if has_flag("--demo") {
        println!("zcode-tui: modo demo (sin runtime de ZCode)");
        (Connector::Demo, None)
    } else if let Some(url) = flag("--url") {
        let (c, rx) = connect_ws(&url, flag("--token"), &workspace).await?;
        (c, Some(rx))
    } else {
        connect_auto(&workspace, flag("--zcode-bin").as_deref(), flag("--model")).await?
    };

    let mut app = App::new(connector);
    let r = app.run(frame_rx).await;
    eprintln!(
        "[diag] deltas vivos(session/event)={} frames={}",
        agent::LIVE_DELTAS.load(std::sync::atomic::Ordering::Relaxed),
        agent::FRAME_DELTAS.load(std::sync::atomic::Ordering::Relaxed),
    );
    r
}

/// Camino por defecto: runtime detectado → `app-server` stdio → sesión.
async fn connect_auto(
    workspace: &str,
    explicit_bin: Option<&str>,
    model_flag: Option<String>,
) -> anyhow::Result<(Connector, Option<tokio::sync::mpsc::UnboundedReceiver<agent::AgentEvent>>)> {
    println!("zcode-tui: buscando runtime de ZCode…");
    let runtime = {
        let explicit = explicit_bin.map(|s| s.to_string());
        tokio::task::spawn_blocking(move || zcode_launcher::find_runtime(explicit.as_deref()))
            .await
            .map_err(|e| anyhow::anyhow!("launcher: {e}"))?
    }
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "zcode-tui: runtime encontrado — {} {}",
        runtime.program,
        runtime.args.join(" ")
    );

    if !runtime.env.is_empty() {
        println!(
            "zcode-tui: usando config de providers del usuario ({})",
            runtime
                .env
                .iter()
                .map(|(k, v)| format!("{k}={}", v.split('/').last().unwrap_or(v)))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let (agent, events) = crate::stdio_agent::StdioAgent::spawn(
        &runtime.program,
        &runtime.args,
        workspace,
        &runtime.env,
    )
    .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    // prioridad: --model > guardado de la última corrida > default
    let saved = std::fs::read_to_string(model_file_path()).ok().and_then(|c| {
        serde_json::from_str::<serde_json::Value>(&c).ok().map(|v| {
            let p = v.get("providerId").and_then(|x| x.as_str()).unwrap_or("zai-api").to_string();
            let m = v.get("modelId").and_then(|x| x.as_str()).unwrap_or("GLM-5.3-Flash").to_string();
            let l = v.get("reasoningLevel").and_then(|x| x.as_str()).map(|s| s.to_string());
            format!("{p}/{m}@{}", l.unwrap_or_else(|| "max".into()))
        })
    });
    let model = Some(model_flag.or(saved)
        .unwrap_or_else(|| "zai-api/GLM-5.3-Flash@high".into()))
        .map(|spec| {
            let (sel, level) = match spec.split_once('@') {
                Some((s, l)) => (s.to_string(), Some(l.to_string())),
                None => (spec, None),
            };
            let (provider, model) = sel
                .split_once('/')
                .ok_or_else(|| anyhow::anyhow!("--model espera providerId/modelId[@nivel]"))?;
            Ok::<_, anyhow::Error>((provider.to_string(), model.to_string(), level))
        })
        .transpose()?;
    let (connector, rx) = connect_stdio(agent, events, workspace.to_string(), model)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok((connector, Some(rx)))
}

/// Ruta del archivo que recuerda el último modelo elegido.
fn model_file_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            std::path::PathBuf::from(home).join(".config")
        });
    base.join("zcode-tui/model.json")
}

async fn connect_ws(
    url: &str,
    token: Option<String>,
    workspace: &str,
) -> anyhow::Result<(Connector, tokio::sync::mpsc::UnboundedReceiver<agent::AgentEvent>)> {
    let (outbound, inbound) = zcode_client::transport::spawn_websocket(url, token.as_deref())
        .await
        .map_err(|e| anyhow::anyhow!("conexión falló: {e}"))?;
    let client = zcode_client::ChannelClient::connect(outbound, inbound)
        .await
        .map_err(|e| anyhow::anyhow!("handshake falló: {e}"))?;
    connect_zcode(client, workspace.to_string())
        .await
        .map_err(|e| anyhow::anyhow!("agente: {e}"))
}
