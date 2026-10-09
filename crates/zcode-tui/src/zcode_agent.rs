//! Conector real al harness de ZCode vía el canal RPC `zcode-agent`.
//!
//! Flujo (mapeado desde `packages/services/src/zcode-agent/`):
//! 1. `initialize({workspacePath})` → runtime del agente en el workspace
//! 2. `createSession({workspacePath})` → snapshot con `sessionId`
//! 3. `helloConversationV4()` + `initializeConversationV4(clientHello)` → handshake v4
//! 4. `listen("zcode-agent", "onDynamicConversationFrame", [{workspacePath}])` → frames wire
//! 5. `subscribeConversationV4({workspacePath, sessionId})` → {subscriptionId}
//! 6. `sendPrompt({workspacePath, sessionId, content})` → el turno corre
//! 7. frames: `row.appended` / `row.upserted` / `row.delta` → texto del asistente
//!
//! Los argumentos viajan como array posicional (`ProxyChannel.fromService`
//! aplica `target.apply(handler, args)`).

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use zcode_client::{ChannelClient, RpcValue};

use crate::agent::AgentEvent;

const CHANNEL: &str = "zcode-agent";

pub struct ZcodeAgent {
    client: ChannelClient,
    workspace_path: String,
    session_id: String,
    client_id: String,
    /// texto ya emitido por fila (rowId → chars emitidos)
    emitted: BTreeMap<i64, usize>,
}

fn call_arg(params: Value) -> RpcValue {
    RpcValue::Array(vec![RpcValue::Object(params)])
}

impl ZcodeAgent {
    /// Ejecuta el handshake completo y lanza la tarea que traduce frames a eventos.
    pub async fn connect(
        client: ChannelClient,
        workspace_path: String,
    ) -> Result<(ZcodeAgentHandle, mpsc::UnboundedReceiver<AgentEvent>), String> {
        let client_id = format!("zcode-tui-{}", std::process::id());

        let init = client
            .call(CHANNEL, "initialize", call_arg(json!({ "workspacePath": workspace_path })))
            .await
            .map_err(|e| format!("initialize: {e}"))?
            .map_err(|e| format!("initialize: {e}"))?;
        eprintln!("[zcode-agent] agent initialize: {}", init.to_json());

        let snapshot = client
            .call(CHANNEL, "createSession", call_arg(json!({ "workspacePath": workspace_path })))
            .await
            .map_err(|e| format!("createSession: {e}"))?
            .map_err(|e| format!("createSession: {e}"))?;
        let session_id = snapshot
            .to_json()
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or("createSession no devolvió sessionId")?
            .to_string();

        // handshake v4 de conversación
        let _hello = client
            .call(CHANNEL, "helloConversationV4", RpcValue::Array(vec![]))
            .await
            .map_err(|e| format!("hello: {e}"))?
            .map_err(|e| format!("hello: {e}"))?;
        let client_hello = json!({
            "kind": "clientHello",
            "protocolVersion": 1,
            "clientId": client_id,
            "appVersion": env!("CARGO_PKG_VERSION"),
        });
        client
            .call(CHANNEL, "initializeConversationV4", call_arg(client_hello))
            .await
            .map_err(|e| format!("clientHello: {e}"))?
            .map_err(|e| format!("clientHello: {e}"))?;

        // subscribirse al stream de frames del workspace
        let mut frames = client.listen(
            CHANNEL,
            "onDynamicConversationFrame",
            call_arg(json!({ "workspacePath": workspace_path })),
        );
        let sub = client
            .call(
                CHANNEL,
                "subscribeConversationV4",
                call_arg(json!({
                    "workspacePath": workspace_path,
                    "sessionId": session_id,
                    "visibility": "foreground",
                })),
            )
            .await
            .map_err(|e| format!("subscribe: {e}"))?
            .map_err(|e| format!("subscribe: {e}"))?;
        eprintln!("[zcode-agent] subscribe: {}", sub.to_json());

        let (tx, rx) = mpsc::unbounded_channel();
        let agent = ZcodeAgent {
            client: client.clone(),
            workspace_path: workspace_path.clone(),
            session_id: session_id.clone(),
            client_id,
            emitted: BTreeMap::new(),
        };
        let mut agent_for_frames = ZcodeAgent {
            client,
            workspace_path,
            session_id,
            client_id: String::new(),
            emitted: BTreeMap::new(),
        };
        tokio::spawn(async move {
            while let Some(frame) = frames.recv().await {
                if let Err(e) = agent_for_frames.apply_wire_frame(&frame.to_json(), &tx) {
                    eprintln!("[zcode-agent] frame: {e}");
                }
            }
        });

        Ok((ZcodeAgentHandle { inner: agent }, rx))
    }

    fn apply_wire_frame(
        &mut self,
        wire: &Value,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<(), String> {
        match wire.get("kind").and_then(|k| k.as_str()) {
            Some("complete") => {
                let frame = wire.get("frame").ok_or("complete frame sin payload")?;
                self.apply_topic_frame(frame, tx)
            }
            Some("fragment") => {
                // ensamblado de fragmentos: pospuesto (frames gigantes); se ignora
                Ok(())
            }
            _ => Err("wire frame sin kind".into()),
        }
    }

    fn apply_topic_frame(
        &mut self,
        frame: &Value,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<(), String> {
        let payload = frame.get("payload").ok_or("frame sin payload")?;
        match payload.get("kind").and_then(|k| k.as_str()) {
            Some("snapshot") => {
                if let Some(rows) = payload
                    .get("snapshot")
                    .and_then(|s| s.get("rows"))
                    .and_then(|r| r.as_array())
                {
                    for row in rows {
                        self.emit_row_text(row, tx);
                    }
                }
                Ok(())
            }
            Some("deltas") => {
                for delta in payload
                    .get("deltas")
                    .and_then(|d| d.as_array())
                    .ok_or("deltas no es array")?
                {
                    match delta.get("op").and_then(|o| o.as_str()) {
                        Some("row.appended") | Some("row.upserted") => {
                            if let Some(row) = delta.get("row") {
                                self.emit_row_text(row, tx);
                            }
                        }
                        Some("row.delta") => {
                            let row_id = delta.get("rowId").and_then(|v| v.as_i64()).unwrap_or(-1);
                            let append = delta
                                .get("append")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();
                            let _ = tx.send(AgentEvent::Delta(append.to_string()));
                            *self.emitted.entry(row_id).or_insert(0) += append.chars().count();
                        }
                        _ => {} // state.updated, workflowRun.*, row.removed: hito 5
                    }
                }
                Ok(())
            }
            _ => Err("payload sin kind".into()),
        }
    }

    /// Emite el texto nuevo de una fila `assistantText` (diff contra lo ya emitido).
    fn emit_row_text(&mut self, row: &Value, tx: &mpsc::UnboundedSender<AgentEvent>) {
        let kind = row.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
        if kind != "assistantText" {
            return;
        }
        let row_id = row.get("rowId").and_then(|v| v.as_i64()).unwrap_or(-1);
        let text = row.get("text").and_then(|t| t.as_str()).unwrap_or_default();
        let emitted_len = self.emitted.entry(row_id).or_insert(0);
        let new_text: String = text.chars().skip(*emitted_len).collect();
        if !new_text.is_empty() {
            let _ = tx.send(AgentEvent::Delta(new_text));
            *emitted_len = text.chars().count();
        }
    }
}

/// Handle liviano para enviar prompts desde la UI.
pub struct ZcodeAgentHandle {
    inner: ZcodeAgent,
}

impl ZcodeAgentHandle {
    pub async fn send_prompt(&self, content: &str) -> Result<(), String> {
        self.inner
            .client
            .call(
                CHANNEL,
                "sendPrompt",
                call_arg(json!({
                    "workspacePath": self.inner.workspace_path,
                    "sessionId": self.inner.session_id,
                    "content": content,
                    "clientMode": "web-remote-replayable",
                })),
            )
            .await
            .map_err(|e| format!("sendPrompt rpc: {e}"))?
            .map_err(|e| format!("sendPrompt: {e}"))?;
        Ok(())
    }
}
