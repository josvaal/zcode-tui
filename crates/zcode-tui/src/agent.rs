//! Conexión con el harness de ZCode. Tres modos:
//! - `Demo`: streaming simulado local para desarrollar la TUI sin runtime.
//! - `Stdio`: **automático** — `zcode app-server` (ZCode Protocol NDJSON) como subproceso.
//! - `Zcode`: WebSocket contra el servidor headless de ZCode (canal `zcode-agent`).

use std::sync::Arc;
pub static LIVE_DELTAS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static FRAME_DELTAS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use crate::{frame_projector::FrameProjector, stdio_agent::ProtocolEvent, zcode_agent::ZcodeAgentHandle};

/// Una sesión reciente del workspace.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub title: String,
    pub updated_at: i64,
}

/// Un modelo elegible del picker.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub provider_id: String,
    pub model_id: String,
    pub label: String,
    pub provider_label: String,
    pub default_level: Option<String>,
}

/// Eventos que la TUI consume.
#[derive(Debug)]
pub enum AgentEvent {
    /// Delta de texto para el mensaje del asistente en curso.
    Delta(String),
    /// Delta del thinking (filas `reasoning`).
    ReasoningDelta(String),
    /// Estado de una tool call (identificada para actualizar in situ).
    ToolCall { call_id: String, line: String },
    /// Diff de archivo de una tool call (líneas con prefijo +/-/ ).
    ToolDiff {
        path: String,
        additions: usize,
        deletions: usize,
        lines: Vec<String>,
    },
    /// Salida de una herramienta (bash).
    ToolOutput(String),
    /// Modelos disponibles para el picker (tras session/create).
    ModelsAvailable(Vec<ModelInfo>),
    /// Sesiones recientes del workspace (para la sidebar).
    SessionsList(Vec<SessionInfo>),
    /// La sesión quedó creada/reanudada con este id.
    SessionReady(String),
    /// Prompt del usuario del historial (resume).
    HistoryUser(String),
    /// El servidor pide aprobación de permiso. (token, resumen, opciones)
    PermissionRequest {
        token: u64,
        tool_name: String,
        summary: String,
        options: Vec<(String, String)>, // (optionId, label)
    },
    /// El mensaje del asistente terminó.
    Done,
    Error(String),
}

enum SessionCmd {
    SendPrompt(String),
    /// Respuesta del usuario a una petición de permiso.
    PermissionAnswer { token: u64, allow: bool },
    /// Reanudar una sesión previa.
    ResumeSession { session_id: String },
    /// Cambiar el modelo de la sesión en caliente.
    SetModel {
        provider_id: String,
        model_id: String,
        level: Option<String>,
    },
}

/// Connector: envía un prompt; los eventos salen por el rx de la app.
pub enum Connector {
    Demo,
    Stdio {
        cmd_tx: mpsc::UnboundedSender<SessionCmd>,
    },
    Zcode {
        handle: Arc<ZcodeAgentHandle>,
    },
}

impl Connector {
    /// Cambia el modelo de la sesión (modo Stdio) y lo recuerda para la próxima.
    /// Reanuda una sesión previa (modo Stdio).
    pub fn resume_session(&self, session_id: String) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::ResumeSession { session_id });
        }
    }

    pub fn set_model(&self, provider_id: String, model_id: String, level: Option<String>) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::SetModel {
                provider_id: provider_id.clone(),
                model_id: model_id.clone(),
                level: level.clone(),
            });
            save_last_model(&provider_id, &model_id, level.as_deref());
        }
    }

    /// Responde una petición de permiso pendiente (modo Stdio).
    pub fn answer_permission(&self, token: u64, allow: bool) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::PermissionAnswer { token, allow });
        }
    }

    pub fn send(&self, prompt: String, tx: mpsc::UnboundedSender<AgentEvent>) {
        match self {
            Connector::Demo => demo_stream(prompt, tx),
            Connector::Stdio { cmd_tx } => {
                let _ = cmd_tx.send(SessionCmd::SendPrompt(prompt));
                // los deltas llegan por el stream de frames; Done lo dispara el frame watcher
                std::mem::forget(tx); // mantenemos el canal vivo (un solo envío por turno)
            }
            Connector::Zcode { handle } => {
                let handle = handle.clone();
                tokio::spawn(async move {
                    match handle.send_prompt(&prompt).await {
                        Ok(()) => {}
                        Err(e) => {
                            let _ = tx.send(AgentEvent::Error(e));
                            let _ = tx.send(AgentEvent::Done);
                        }
                    }
                });
            }
        }
    }
}

fn demo_stream(prompt: String, tx: mpsc::UnboundedSender<AgentEvent>) {
    tokio::spawn(async move {
        let reply = format!(
            "**Demo** — recibí: {prompt}\n\nEsto es streaming simulado mientras no hay runtime de ZCode.\n\n```rust\nlet tui = \"zcode-tui\"; // conectado al harness\n```"
        );
        for word in reply.split_inclusive(' ') {
            if tx.send(AgentEvent::Delta(word.to_string())).is_err() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(18)).await;
        }
        let _ = tx.send(AgentEvent::Done);
    });
}

// ---------------------------------------------------------------------------
// Modo stdio: sesión sobre `zcode app-server`
// ---------------------------------------------------------------------------

/// Ejecuta el flujo de sesión completo contra el app-server y devuelve el
/// conector + el canal de eventos para la TUI.
pub async fn connect_stdio(
    agent: Arc<crate::stdio_agent::StdioAgent>,
    mut events: mpsc::UnboundedReceiver<ProtocolEvent>,
    workspace: String,
    model: Option<(String, String, Option<String>)>, // (providerId, modelId, reasoningLevel)
) -> Result<(Connector, mpsc::UnboundedReceiver<AgentEvent>), String> {
    // el modelSelection DEBE ir en session/send: sin él la creación del modelo
    // falla con "Reasoning level is required" (verificado contra app-server real)
    let mut current_model = model.as_ref().map(|(p, m, level)| {
        json!({
            "providerId": p,
            "modelId": m,
            "options": { "reasoningLevel": level.clone().unwrap_or_else(|| "max".into()) },
        })
    });
    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<SessionCmd>();

    // 1. session/create
    let mut create_params = json!({ "workspace": { "workspacePath": &workspace, "workspaceKey": &workspace } });
    if let Some((provider_id, model_id, level)) = &model {
        create_params["model"] = json!({
            "providerId": provider_id,
            "modelId": model_id,
            "options": { "reasoningLevel": level.clone().unwrap_or_else(|| "high".into()) },
        });
    }
    agent.send(1, "session/create", &create_params).await?;
    let mut next_id = 2u64;
    let mut perm_token = 0u64;
    let mut pending_perms: std::collections::HashMap<u64, (serde_json::Value, Vec<(String, String)>)> =
        Default::default();
    // requestIds de permiso ya mostrados (el server re-envía el mismo request)
    let mut seen_perm_requests: std::collections::BTreeSet<String> = Default::default();
    let mut session_id: Option<String> = None;
    let mut projector = FrameProjector::default();


    let err_tx = agent_tx.clone();
    let connection_id = format!("zcode-tui-{}", std::process::id());
    let mut pending_resume: Option<(u64, String)> = None;
    let workspace_for_list = workspace.clone();
    // solo aceptamos fin de turno después de ver la fase running del turno actual
    let mut saw_running = false;
    let flow = async move {
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(SessionCmd::SetModel { provider_id, model_id, level }) => {
                            current_model = Some(json!({
                                "providerId": provider_id,
                                "modelId": model_id,
                                "options": { "reasoningLevel": level.clone().unwrap_or_else(|| "max".into()) },
                            }));
                            if let Some(sid) = &session_id {
                                let _ = agent
                                    .send(
                                        next_id,
                                        "session/setModel",
                                        &json!({
                                            "sessionId": sid,
                                            "model": current_model.clone().unwrap_or(Value::Null),
                                        }),
                                    )
                                    .await;
                                next_id += 1;
                            }
                        }
                        Some(SessionCmd::ResumeSession { session_id: target }) => {
                            let _ = agent
                                .send(
                                    next_id,
                                    "session/resume",
                                    &json!({
                                        "sessionId": target,
                                        "workspace": { "workspacePath": workspace_for_list, "workspaceKey": workspace_for_list },
                                    }),
                                )
                                .await;
                            pending_resume = Some((next_id, target));
                            next_id += 1;
                        }
                        Some(SessionCmd::PermissionAnswer { token, allow }) => {
                            if let Some((raw_id, _options)) = pending_perms.remove(&token) {
                                // formato legacy del broker: {decision: allow|deny|escalate|modify}
                                let decision = if allow { "allow" } else { "deny" };
                                let _ = agent.respond(&raw_id, json!({ "decision": decision })).await;
                            }
                        }
                        Some(SessionCmd::SendPrompt(content)) => {
                            if let Some(sid) = &session_id {
                                let mut params = json!({
                                    "sessionId": sid,
                                    "content": content,
                                });
                                if let Some(ms) = &current_model {
                                    params["modelSelection"] = ms.clone();
                                }
                                agent.send(next_id, "session/send", &params).await?;
                                next_id += 1;
                            } else {
                                let _ = agent_tx.send(AgentEvent::Error("sesión no creada".into()));
                            }
                        }
                        None => break,
                    }
                }
                ev = events.recv() => {
                    match ev {
                        Some(ProtocolEvent::Response(id, result)) if id == 1 => {
                            let result = result.map_err(|e| format!("session/create: {e}"))?;
                            let sid = result
                                .pointer("/session/sessionId")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .or_else(|| find_string(&result, "sessionId"))
                                .ok_or("session/create sin sessionId")?;
                            session_id = Some(sid);
                            // modelos disponibles para el picker
                            let mut models = Vec::new();
                            if let Some(arr) =
                                result.pointer("/settings/model/available").and_then(|v| v.as_array())
                            {
                                for m in arr {
                                    let (provider_id, model_id) = match m.pointer("/ref") {
                                        Some(r) => (
                                            r.get("providerId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                            r.get("modelId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                        ),
                                        None => continue,
                                    };
                                    if provider_id.is_empty() || model_id.is_empty() {
                                        continue;
                                    }
                                    models.push(ModelInfo {
                                        provider_id,
                                        model_id,
                                        label: m.get("label").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                                        provider_label: m.get("providerLabel").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                                        default_level: m
                                            .pointer("/reasoning/defaultLevel")
                                            .and_then(|v| v.as_str())
                                            .map(|s| s.to_string()),
                                    });
                                }
                            }
                            let _ = agent_tx.send(AgentEvent::ModelsAvailable(models));
                            // listar sesiones recientes del workspace para la sidebar
                            let _ = agent
                                .send(
                                    next_id,
                                    "session/list",
                                    &json!({
                                        "workspace": { "workspacePath": workspace_for_list, "workspaceKey": workspace_for_list },
                                        "limit": 25,
                                    }),
                                )
                                .await;
                            next_id += 1;
                            let sid = session_id.clone().unwrap();
                            // 2. suscripción v4 al stream de frames
                            agent.send(next_id, "v4/conversation/subscribe", &json!({
                                "topic": format!("conversation/{sid}"),
                                "connectionId": connection_id.clone(),
                                "clientMode": "web-remote-replayable",
                                "visibility": "foreground",
                            })).await?;
                            next_id += 1;
                            // stream legacy de deltas EN VIVO (thinking/texto token a token)
                            agent.send(next_id, "session/subscribe", &json!({
                                "sessionId": sid,
                                "deliveryKind": "desktop-continuous",
                            })).await?;
                            next_id += 1;
                        }
                        Some(ProtocolEvent::Response(id, result)) => {
                            // respuesta de session/list → poblar sidebar
                            if result.is_ok() {
                                if let Some(arr) = result
                                    .as_ref()
                                    .ok()
                                    .and_then(|r| r.pointer("/sessions"))
                                    .and_then(|v| v.as_array())
                                {
                                    let mut list: Vec<crate::agent::SessionInfo> = arr
                                        .iter()
                                        .filter_map(|si| {
                                            Some(crate::agent::SessionInfo {
                                                session_id: si.get("sessionId")?.as_str()?.to_string(),
                                                title: si.get("title").and_then(|v| v.as_str()).unwrap_or("(sin título)").to_string(),
                                                updated_at: si.get("updatedAt").and_then(|v| v.as_i64()).unwrap_or(0),
                                            })
                                        })
                                        .collect();
                                    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                                    let _ = agent_tx.send(AgentEvent::SessionsList(list));
                                }
                            }
                            // respuesta de session/resume → re-suscribir a la sesión reanudada
                            if let Some((rid, newsid)) = pending_resume.clone() {
                                if rid == id {
                                    session_id = Some(newsid.clone());
                                    pending_resume = None;
                                    let _ = agent_tx.send(AgentEvent::SessionReady(newsid.clone()));
                                    let _ = agent
                                        .send(
                                            next_id,
                                            "v4/conversation/subscribe",
                                            &json!({
                                                "topic": format!("conversation/{newsid}"),
                                                "connectionId": connection_id,
                                                "clientMode": "web-remote-replayable",
                                                "visibility": "foreground",
                                            }),
                                        )
                                        .await;
                                    next_id += 1;
                                    let _ = agent
                                        .send(
                                            next_id,
                                            "session/subscribe",
                                            &json!({ "sessionId": newsid, "deliveryKind": "desktop-continuous" }),
                                        )
                                        .await;
                                    next_id += 1;
                                    // resetear contadores de dedupe: historial nuevo
                                    projector = FrameProjector::default();
                                }
                                continue;
                            }
                            if let Err(e) = &result {
                                let _ = agent_tx.send(AgentEvent::Error(e.clone()));
                                let _ = agent_tx.send(AgentEvent::Done);
                            }
                        }
                        Some(ProtocolEvent::Notification(method, params)) => {
                            match method.as_str() {
                                "v4/conversation/frame" => {
                                    projector.apply_flex(&params, &agent_tx);
                                    // ventana drenada para que el servidor siga enviando
                                    let _ = agent
                                        .send(
                                            next_id,
                                            "v4/connection/flow",
                                            &json!({
                                                "connectionId": connection_id,
                                                "state": "drained",
                                            }),
                                        )
                                        .await;
                                    next_id += 1;
                                }
                                "state.updated" => {
                                    let status = params.pointer("/patch/status").and_then(|v| v.as_str());
                                    let phase = params
                                        .pointer("/patch/control/phase")
                                        .and_then(|v| v.as_str());
                                    if status == Some("running") || phase == Some("running") {
                                        saw_running = true;
                                    }
                                    // errores del turno/provider: visibles en el transcript
                                    if let Some(err) = params.pointer("/patch/control/lastError") {
                                    let mut msg = err
                                        .get("message")
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("error desconocido")
                                        .to_string();
                                    if let Some(u) =
                                        err.get("underlyingErrorMessage").and_then(|m| m.as_str())
                                    {
                                        msg = u.to_string();
                                    }
                                    let code = err
                                        .get("code")
                                        .and_then(|c| c.as_str())
                                        .unwrap_or_default()
                                        .to_string();
                                    let _ = agent_tx.send(AgentEvent::Error(format!(
                                        "⚠ error {code}: {msg}"
                                    )));
                                    }
                                    let finished = status == Some("idle")
                                        || matches!(
                                            phase,
                                            Some("completedSuccess") | Some("completedInterrupted") | Some("error")
                                        );
                                    if finished && saw_running {
                                        saw_running = false;
                                        let _ = agent_tx.send(AgentEvent::Done);
                                    }
                                }
                                "session/event" => {
                                    // streaming en vivo: reasoning_delta / text_delta
                                    LIVE_DELTAS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    projector.apply_live_delta(&params, &agent_tx);
                                }
                                _ => {}
                            }
                        }
                        Some(ProtocolEvent::ServerRequest(id, method, req_params)) => {
                            if method == "interaction/requestPermission" {
                                let request_id = req_params
                                    .get("requestId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string();
                                if !request_id.is_empty()
                                    && !seen_perm_requests.insert(request_id)
                                {
                                    continue;
                                }
                                perm_token += 1;
                                let token = perm_token;
                                let tool_name = req_params
                                    .get("toolName")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?")
                                    .to_string();
                                let summary = req_params
                                    .get("summary")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let options: Vec<(String, String)> = req_params
                                    .pointer("/payload/options")
                                    .or_else(|| req_params.get("options"))
                                    .and_then(|o| o.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|o| {
                                                Some((
                                                    o.get("optionId")?.as_str()?.to_string(),
                                                    o.get("label")?.as_str()?.to_string(),
                                                ))
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                pending_perms.insert(token, (id.clone(), options.clone()));
                                let _ = agent_tx.send(AgentEvent::PermissionRequest {
                                    token,
                                    tool_name,
                                    summary,
                                    options,
                                });
                                continue;
                            }
                            // responder peticiones del servidor con valores por defecto
                            let result = match method.as_str() {
                                "session/requestRuntimePreferences" => Some(json!({
                                    "nativeSearchEnhancementsEnabled": false,
                                    "memoryEnabled": false,
                                    "askUserQuestionAutoResolutionEnabled": true,
                                })),
                                "interaction/requestOfficialMcpAuthHeaders" => Some(json!({ "headers": {} })),
                                _ => None,
                            };
                            if let Some(result) = result {
                                let _ = agent.respond(&id, result).await;
                            }
                        }
                        Some(ProtocolEvent::Closed(reason)) => {
                            let msg = if reason.is_empty() {
                                "app-server cerró la conexión".to_string()
                            } else {
                                format!("app-server cerró la conexión — log (/tmp/zcode-tui-appserver.log): {reason}")
                            };
                            let _ = agent_tx.send(AgentEvent::Error(msg));
                            let _ = agent_tx.send(AgentEvent::Done);
                            break;
                        }
                        None => break,
                    }
                }
            }
        }
        Ok::<(), String>(())
    };

    tokio::spawn(async move {
        if let Err(e) = flow.await {
            let _ = err_tx.send(AgentEvent::Error(e));
            let _ = err_tx.send(AgentEvent::Done);
        }
    });

    Ok((Connector::Stdio { cmd_tx }, agent_rx))
}

/// Envía una petición y espera su respuesta específica (helper bloqueante de la tarea).
/// Nota: la versión simple usa el hecho de que la tarea es single-threaded.
impl Connector {}


/// Busca recursivamente un string por clave en un JSON.
fn find_string(v: &serde_json::Value, key: &str) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(s) = map.get(key).and_then(|x| x.as_str()) {
                return Some(s.to_string());
            }
            for (_, child) in map {
                if let Some(found) = find_string(child, key) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(items) => items.iter().find_map(|i| find_string(i, key)),
        _ => None,
    }
}

/// Conecta al harness real vía WebSocket (canal `zcode-agent`) — alternativa --url.
pub async fn connect_zcode(
    client: zcode_client::ChannelClient,
    workspace: String,
) -> Result<(Connector, mpsc::UnboundedReceiver<AgentEvent>), String> {
    let (handle, rx) = crate::zcode_agent::ZcodeAgent::connect(client, workspace).await?;
    Ok((Connector::Zcode { handle: Arc::new(handle) }, rx))
}

/// Guarda el último modelo elegido en ~/.config/zcode-tui/model.json.
fn save_last_model(provider_id: &str, model_id: &str, level: Option<&str>) {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            std::path::PathBuf::from(home).join(".config")
        });
    let dir = base.join("zcode-tui");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let mut v = serde_json::json!({
        "providerId": provider_id,
        "modelId": model_id,
    });
    if let Some(level) = level {
        v["reasoningLevel"] = serde_json::Value::String(level.to_string());
    }
    let _ = std::fs::write(dir.join("model.json"), serde_json::to_string(&v).unwrap_or_default());
}

#[allow(dead_code)]
type _KeepOneshot = oneshot::Sender<()>;
