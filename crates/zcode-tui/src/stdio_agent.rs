//! Cliente del **ZCode Protocol** por stdio (NDJSON JSON-RPC).
//!
//! Es el transporte oficial headless del CLI: `zcode app-server` habla
//! `{id, method, params}` / `{id, result}` / `{method, params}` por líneas
//! JSON en stdin/stdout (`packages/shared/src/zcode-protocol/index.ts`).

use std::{process::Stdio, sync::Arc};

/// Lee el final del log de stderr del app-server para diagnosticar muertes.
fn child_status_reason() -> Option<String> {
    let meta = std::fs::metadata("/tmp/zcode-tui-appserver.log").ok()?;
    let size = meta.len() as usize;
    let start = size.saturating_sub(600);
    let content = std::fs::read_to_string("/tmp/zcode-tui-appserver.log").ok()?;
    let tail: String = content
        .chars()
        .skip(content.chars().count().saturating_sub(300))
        .collect();
    let _ = start;
    Some(tail.trim().to_string())
}

use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
};

pub struct StdioAgent {
    stdin: tokio::sync::Mutex<tokio::process::ChildStdin>,
    child: tokio::sync::Mutex<Child>,
}

/// Identificador de petición (nuestro u64 o el string del servidor, p. ej. "server-1").
#[derive(Debug, Clone)]
pub enum ReqId {
    Ours(u64),
    Server(String),
}

#[derive(Debug)]
pub enum ProtocolEvent {
    Response(u64, Result<Value, String>),
    /// Petición del servidor que exige respuesta (`{id, method, params}` con id no numérico).
    ServerRequest(Value, String, Value),
    Notification(String, Value),
    /// El proceso app-server terminó (con diagnóstico si hay log).
    Closed(String),
}

impl StdioAgent {
    /// Lanza el proceso `app-server` y arranca el lector de NDJSON.
    pub async fn spawn(
        program: &str,
        args: &[String],
        workspace: &str,
        env: &[(String, String)],
    ) -> Result<(Arc<StdioAgent>, mpsc::UnboundedReceiver<ProtocolEvent>), String> {
        // stderr del app-server → log, para diagnosticar crashes de arranque
        let stderr_log = std::fs::File::create("/tmp/zcode-tui-appserver.log")
            .map(std::process::Stdio::from)
            .unwrap_or(Stdio::null());
        let mut child = Command::new(program)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .arg("--cwd")
            .arg(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr_log)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("no pude ejecutar {program}: {e}"))?;

        let stdin = child.stdin.take().ok_or("sin stdin")?;
        let stdout = child.stdout.take().ok_or("sin stdout")?;

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                            continue; // línea no-JSON: ruido, ignorar
                        };
                        let has_method = msg.get("method").is_some();
                        if let Some(id) = msg.get("id").cloned() {
                            // petición del servidor (id string + method): exige respuesta
                            if has_method {
                                if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                                    let _ = event_tx.send(ProtocolEvent::ServerRequest(
                                        id,
                                        method.to_string(),
                                        msg.get("params").cloned().unwrap_or(Value::Null),
                                    ));
                                }
                                continue;
                            }
                            let Some(id_num) = id.as_u64() else { continue };
                            let result = if let Some(err) = msg.get("error") {
                                Err(format!(
                                    "{} (code {})",
                                    err.get("message").and_then(|m| m.as_str()).unwrap_or("?"),
                                    err.get("code").and_then(|c| c.as_i64()).unwrap_or(0)
                                ))
                            } else {
                                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                            };
                            let _ = event_tx.send(ProtocolEvent::Response(id_num, result));
                        } else if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
                            let _ = event_tx.send(ProtocolEvent::Notification(
                                method.to_string(),
                                msg.get("params").cloned().unwrap_or(Value::Null),
                            ));
                        }
                    }
                    _ => break,
                }
            }
            // motivo: estado de salida + cola del log de stderr
            let reason = child_status_reason().unwrap_or_default();
            let _ = event_tx.send(ProtocolEvent::Closed(reason));
        });

        Ok((
            Arc::new(StdioAgent {
                stdin: tokio::sync::Mutex::new(stdin),
                child: tokio::sync::Mutex::new(child),
            }),
            event_rx,
        ))
    }

    /// Escribe una petición NDJSON. La respuesta llega por el canal de eventos
    /// con el mismo id; el emparejamiento lo hace la tarea de sesión.
    pub async fn send(&self, id: u64, method: &str, params: &Value) -> Result<(), String> {
        let msg = serde_json::json!({ "id": id, "method": method, "params": params });
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(serde_json::to_string(&msg).unwrap().as_bytes())
            .await
            .map_err(|e| format!("stdin: {e}"))?;
        stdin.write_all(b"\n").await.map_err(|e| format!("stdin: {e}"))?;
        stdin.flush().await.map_err(|e| format!("stdin: {e}"))?;
        Ok(())
    }

    /// Responde una petición del servidor.
    pub async fn respond(&self, id: &Value, result: Value) -> Result<(), String> {
        let msg = serde_json::json!({ "id": id, "result": result });
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(serde_json::to_string(&msg).unwrap().as_bytes())
            .await
            .map_err(|e| format!("stdin: {e}"))?;
        stdin.write_all(b"\n").await.map_err(|e| format!("stdin: {e}"))?;
        stdin.flush().await.map_err(|e| format!("stdin: {e}"))?;
        Ok(())
    }

    pub async fn shutdown(&self) {
        if let Ok(mut child) = self.child.try_lock() {
            let _ = child.kill().await;
        }
    }
}
