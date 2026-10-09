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
    /// La sesión se bifurcó: (id original, id de la copia).
    Forked { from: String, to: String },
    /// Prompt del usuario del historial (resume).
    HistoryUser(String),
    /// El servidor pide aprobación de permiso. (token, resumen, opciones)
    PermissionRequest {
        token: u64,
        tool_name: String,
        summary: String,
        options: Vec<(String, String)>, // (optionId, label)
    },
    /// El servidor pide una respuesta del usuario (AskUserQuestion / plan).
    UserInputRequest {
        token: u64,
        prompt: String,
        questions: Vec<UiQuestion>,
    },
    /// Uso de tokens de la sesión (respuesta de session/usage).
    Usage(String),
    /// El mensaje del asistente terminó.
    Done,
    Error(String),
}

/// Una pregunta del userInput request del server.
#[derive(Debug, Clone)]
pub struct UiQuestion {
    pub question: String,
    pub header: String,
    /// (value, label)
    pub options: Vec<(String, String)>,
}

enum SessionCmd {
    SendPrompt(String),
    /// Respuesta del usuario a una petición de permiso.
    PermissionAnswer { token: u64, allow: bool, always: bool },
    /// Respuesta del usuario a un AskUserQuestion.
    AnswerUserInput {
        token: u64,
        accept: bool,
        /// (header, value) por pregunta
        answers: Vec<(String, String)>,
    },
    /// Interrumpir el turno en curso.
    Stop,
    /// Compactar la sesión (/compact).
    Compact,
    /// Modo de colaboración: build | edit | plan | yolo.
    SetMode(String),
    /// Renombrar la sesión actual.
    RenameSession(String),
    /// Borrar la sesión actual.
    DeleteSession,
    /// Consultar uso de tokens de la sesión.
    Usage,
    /// Reanudar una sesión previa.
    ResumeSession { session_id: String },
    /// Bifurcar la sesión actual: copia con todo el historial hasta el último
    /// checkpoint, dejando la sesión original intacta.
    ForkSession,
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

    /// Bifurca la sesión actual: crea una copia nueva con el historial
    /// completo y cambia a ella; la original queda intacta.
    pub fn fork_session(&self) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::ForkSession);
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
    /// `always` agrega regla permanente para la tool (always allow).
    pub fn answer_permission(&self, token: u64, allow: bool, always: bool) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::PermissionAnswer { token, allow, always });
        }
    }

    pub fn answer_user_input(&self, token: u64, accept: bool, answers: Vec<(String, String)>) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::AnswerUserInput { token, accept, answers });
        }
    }

    /// Interrumpe el turno en curso (session/stop).
    pub fn stop(&self) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::Stop);
        }
    }

    /// Compacta la sesión (session/compact).
    pub fn compact(&self) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::Compact);
        }
    }

    pub fn set_mode(&self, mode: String) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::SetMode(mode));
        }
    }

    pub fn rename_session(&self, title: String) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::RenameSession(title));
        }
    }

    pub fn delete_session(&self) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::DeleteSession);
        }
    }

    pub fn request_usage(&self) {
        if let Connector::Stdio { cmd_tx } = self {
            let _ = cmd_tx.send(SessionCmd::Usage);
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
    let (agent_tx, agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
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
    let mut pending_perms: std::collections::HashMap<
        u64,
        (serde_json::Value, Vec<(String, String)>, String),
    > = Default::default();
    let mut pending_user_inputs: std::collections::HashMap<u64, serde_json::Value> =
        Default::default();
    let mut pending_usage: Option<u64> = None;
    // requestIds de permiso ya mostrados (el server re-envía el mismo request)
    let mut seen_perm_requests: std::collections::BTreeSet<String> = Default::default();
    let mut session_id: Option<String> = None;
    let mut projector = FrameProjector::default();


    let err_tx = agent_tx.clone();
    let connection_id = format!("zcode-tui-{}", std::process::id());
    let mut pending_resume: Option<(u64, String)> = None;
    let mut pending_fork: Option<u64> = None;
    // turnos enviados en esta sesión (para el fallback de fork por índice)
    let mut turn_count: usize = 0;
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
                        Some(SessionCmd::ForkSession) => {
                            // el server rechaza el fork con un prompt corriendo
                            if let Some(sid) = &session_id {
                                let _ = agent
                                    .send(
                                        next_id,
                                        "session/fork",
                                        &json!({
                                            "sessionId": sid,
                                            "target": { "kind": "latestCheckpoint" },
                                        }),
                                    )
                                    .await;
                                pending_fork = Some(next_id);
                                next_id += 1;
                            }
                        }
                        Some(SessionCmd::PermissionAnswer { token, allow, always }) => {
                            if let Some((raw_id, _options, tool_name)) =
                                pending_perms.remove(&token)
                            {
                                // formato legacy del broker: {decision: allow|deny|escalate|modify}
                                let decision = if allow { "allow" } else { "deny" };
                                let mut response = json!({ "decision": decision });
                                if allow && always {
                                    // always allow: regla permanente para esta tool
                                    response["permissionUpdates"] = json!([{
                                        "type": "addRules",
                                        "behavior": "allow",
                                        "rules": [{ "toolName": tool_name }],
                                    }]);
                                }
                                let _ = agent.respond(&raw_id, response).await;
                            }
                        }
                        Some(SessionCmd::AnswerUserInput { token, accept, answers }) => {
                            if let Some(raw_id) = pending_user_inputs.remove(&token) {
                                let mut content = serde_json::Map::new();
                                for (header, value) in answers {
                                    content.insert(header, Value::String(value));
                                }
                                let response = if accept {
                                    json!({ "action": "accept", "content": content })
                                } else {
                                    json!({ "action": "cancel" })
                                };
                                let _ = agent.respond(&raw_id, response).await;
                            }
                        }
                        Some(SessionCmd::Stop) => {
                            if let Some(sid) = &session_id {
                                let _ = agent
                                    .send(
                                        next_id,
                                        "session/stop",
                                        &json!({ "sessionId": sid }),
                                    )
                                    .await;
                                next_id += 1;
                            }
                        }
                        Some(SessionCmd::Compact) => {
                            if let Some(sid) = &session_id {
                                let _ = agent
                                    .send(
                                        next_id,
                                        "session/compact",
                                        &json!({ "sessionId": sid }),
                                    )
                                    .await;
                                next_id += 1;
                            }
                        }
                        Some(SessionCmd::SetMode(mode)) => {
                            if let Some(sid) = &session_id {
                                let _ = agent
                                    .send(
                                        next_id,
                                        "session/setMode",
                                        &json!({ "sessionId": sid, "mode": mode }),
                                    )
                                    .await;
                                next_id += 1;
                            }
                        }
                        Some(SessionCmd::Usage) => {
                            if let Some(sid) = &session_id {
                                let _ = agent
                                    .send(
                                        next_id,
                                        "session/usage",
                                        &json!({ "sessionId": sid }),
                                    )
                                    .await;
                                pending_usage = Some(next_id);
                                next_id += 1;
                            }
                        }
                        Some(SessionCmd::RenameSession(title)) => {
                            if let Some(sid) = &session_id {
                                let _ = agent
                                    .send(
                                        next_id,
                                        "v4/command",
                                        &command_envelope(sid, "renameSession", json!({ "title": title })),
                                    )
                                    .await;
                                next_id += 1;
                            }
                        }
                        Some(SessionCmd::DeleteSession) => {
                            if let Some(sid) = &session_id {
                                let _ = agent
                                    .send(
                                        next_id,
                                        "v4/command",
                                        &command_envelope(sid, "deleteSession", json!({})),
                                    )
                                    .await;
                                next_id += 1;
                            }
                        }
                        Some(SessionCmd::SendPrompt(content)) => {
                            turn_count += 1;
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
                            // respuesta de session/usage → tokens para la status bar
                            if pending_usage == Some(id) {
                                pending_usage = None;
                                if let Ok(r) = &result {
                                    let grab = |p: &[&str]| {
                                        for path in p {
                                            if let Some(n) = r.pointer(path).and_then(|v| v.as_i64()) {
                                                return Some(n);
                                            }
                                        }
                                        None
                                    };
                                    let input = grab(&[
                                        "/usage/inputTokens",
                                        "/inputTokens",
                                        "/usage/promptTokens",
                                        "/result/usage/inputTokens",
                                        "/result/inputTokens",
                                        "/taskTokenUsage/usage/inputTokens",
                                    ]);
                                    let output = grab(&[
                                        "/usage/outputTokens",
                                        "/outputTokens",
                                        "/usage/completionTokens",
                                        "/result/usage/outputTokens",
                                        "/result/outputTokens",
                                        "/taskTokenUsage/usage/outputTokens",
                                    ]);
                                    let total = grab(&[
                                        "/usage/totalTokens",
                                        "/totalTokens",
                                        "/result/usage/totalTokens",
                                        "/result/totalTokens",
                                        "/taskTokenUsage/usage/totalTokens",
                                    ]);
                                    let mut parts = Vec::new();
                                    if let (Some(i), Some(o)) = (input, output) {
                                        parts.push(format!("{i}↑ {o}↓"));
                                    }
                                    if let Some(t) = total {
                                        parts.push(format!("{t} tok"));
                                    }
                                    if !parts.is_empty() {
                                        let _ = agent_tx.send(AgentEvent::Usage(parts.join(" · ")));
                                    }
                                }
                            }
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
                            // respuesta de session/fork → cambiar a la copia y re-suscribir
                            if let Some(rid) = pending_fork {
                                if rid == id {
                                    pending_fork = None;
                                    match &result {
                                        Ok(r) => {
                                            let newsid = r
                                                .get("forkedSessionId")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                                .or_else(|| find_string(r, "forkedSessionId"));
                                            if let Some(newsid) = newsid {
                                                let old =
                                                    session_id.clone().unwrap_or_default();
                                                session_id = Some(newsid.clone());
                                                let _ = agent_tx.send(AgentEvent::Forked {
                                                    from: old,
                                                    to: newsid.clone(),
                                                });
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
                                            } else {
                                                let _ = agent_tx.send(AgentEvent::Error(
                                                    format!("session/fork sin forkedSessionId: {r}"),
                                                ));
                                            }
                                        }
                                        Err(e) => {
                                            let msg = e.to_string();
                                            // sin checkpoint (sesión solo-texto): el
                                            // server acepta fork por índice de turno
                                            if msg.contains("checkpoint") && turn_count > 0 {
                                                if let Some(sid) = session_id.clone() {
                                                    let _ = agent
                                                        .send(
                                                            next_id,
                                                            "session/fork",
                                                            &json!({
                                                                "sessionId": sid,
                                                                "target": {
                                                                    "kind": "turn",
                                                                    "turnIndex": turn_count - 1,
                                                                },
                                                            }),
                                                        )
                                                        .await;
                                                    pending_fork = Some(next_id);
                                                    next_id += 1;
                                                    continue;
                                                }
                                            }
                                            let _ = agent_tx.send(AgentEvent::Error(
                                                format!("session/fork: {msg}"),
                                            ));
                                        }
                                    }
                                    continue;
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
                                pending_perms.insert(
                                    token,
                                    (id.clone(), options.clone(), tool_name.clone()),
                                );
                                let _ = agent_tx.send(AgentEvent::PermissionRequest {
                                    token,
                                    tool_name,
                                    summary,
                                    options,
                                });
                                continue;
                            }
                            if method == "interaction/requestUserInput" {
                                let request_id = req_params
                                    .get("requestId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string();
                                if !request_id.is_empty()
                                    && !seen_perm_requests.insert(format!("ui:{request_id}"))
                                {
                                    let _ = agent.respond(&id, json!({ "action": "cancel" })).await;
                                    continue;
                                }
                                let prompt = req_params
                                    .get("prompt")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let questions: Vec<UiQuestion> = req_params
                                    .get("questions")
                                    .and_then(|q| q.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .map(|q| UiQuestion {
                                                question: q.get("question").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                                header: q.get("header").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                                options: q
                                                    .get("options")
                                                    .and_then(|o| o.as_array())
                                                    .map(|opts| {
                                                        opts.iter()
                                                            .filter_map(|o| {
                                                                Some((
                                                                    o.get("value")?.as_str()?.to_string(),
                                                                    o.get("label")?.as_str()?.to_string(),
                                                                ))
                                                            })
                                                            .collect()
                                                    })
                                                    .unwrap_or_default(),
                                            })
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                if questions.is_empty() && prompt.is_empty() {
                                    let _ = agent.respond(&id, json!({ "action": "cancel" })).await;
                                    continue;
                                }
                                perm_token += 1;
                                let token = perm_token;
                                pending_user_inputs.insert(token, id.clone());
                                let _ = agent_tx.send(AgentEvent::UserInputRequest {
                                    token,
                                    prompt,
                                    questions,
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

/// Envelope v4 para `v4/command` (sin CAS: renameSession/deleteSession no lo exigen).
fn command_envelope(session_id: &str, cmd_type: &str, payload: serde_json::Value) -> serde_json::Value {
    let command_id = format!(
        "019{:08x}-7tui-7{:x}-8{:x}-{:012x}",
        std::process::id() as u32 & 0xffff_ffff,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() & 0xffff)
            .unwrap_or(0),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    );
    json!({
        "commandId": command_id,
        "clientId": "zcode-tui",
        "sessionId": session_id,
        "type": cmd_type,
        "payload": payload,
        "issuedAt": iso_now(),
    })
}

/// Timestamp RFC3339 UTC sin dependencias.
fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // días civiles → (año, mes, día), algoritmo de Howard Hinnant
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}
