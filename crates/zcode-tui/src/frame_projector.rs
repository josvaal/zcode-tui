//! Proyector de frames de conversación v4 → deltas de texto.
//!
//! Compartido por el conector WebSocket (`zcode_agent.rs`) y el conector
//! stdio (`stdio_agent.rs`). Aplica los 7 ops del protocolo
//! (`packages/shared/src/zcode-protocol-v4/delta.ts`), extrayendo el texto
//! nuevo de las filas `assistantText`.

use std::collections::BTreeMap;

use serde_json::Value;
use tokio::sync::mpsc;

use crate::agent::AgentEvent;

#[derive(Default)]
pub struct FrameProjector {
    /// rowId → kind de fila ("assistantText" | "reasoning" | "toolCall" | ...)
    pub row_kinds: BTreeMap<i64, String>,
    /// vimos running del turno actual (guard contra idle viejos)
    pub saw_running: bool,
    /// chars ya emitidos del streaming en vivo del turno actual, por kind
    pub turn_text: usize,
    pub turn_reasoning: usize,
    /// rowId del turnHeader ya visto (los frames repiten turnHeader por
    /// deliveryKind initial/online/recovery; solo un rowId nuevo resetea)
    pub last_turn_header: Option<i64>,
    /// rowIds con diff/output ya emitidos (evitar re-emitir en upserts)
    pub diff_emitted: std::collections::BTreeSet<i64>,
    pub output_emitted: std::collections::BTreeSet<i64>,
    /// rowId → chars ya emitidos de esa fila (dedupe de filas repetidas)
    pub row_emitted: BTreeMap<i64, usize>,
    /// el turno anterior ya terminó (recién entonces un turnHeader resetea)
    pub saw_completed: bool,
    /// último mensaje de error emitido (evitar repetirlo)
    pub last_error_msg: Option<String>,
    /// el stream legacy (session/event) está entregando deltas en vivo:
    /// los row.delta de los frames v4 entonces solo actualizan estado
    /// (dos fuentes vivas duplicarían el texto entrelazado)
    pub legacy_live_active: bool,
}

impl FrameProjector {
    /// Aplica `params` de la notificación de frame, tolerando ambos formatos:
    /// wire frame (`{kind: complete|fragment, frame}`) o topic frame directo
    /// (`{topic, payload}`).
    pub fn apply_flex(&mut self, params: &Value, tx: &mpsc::UnboundedSender<AgentEvent>) {
        let result = if params.get("kind").is_some() {
            self.apply_wire_frame(params, tx)
        } else {
            self.apply_topic_frame(params, tx)
        };
        if let Err(e) = result {
            eprintln!("[zcode-frame] {e}");
        }
    }

    /// Aplica un wire frame (con kind complete/fragment) del canal host.
    pub fn apply_wire_frame(
        &mut self,
        wire: &Value,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<(), String> {
        match wire.get("kind").and_then(|k| k.as_str()) {
            Some("complete") => {
                let frame = wire.get("frame").ok_or("complete frame sin payload")?;
                self.apply_topic_frame(frame, tx)
            }
            Some("fragment") => Ok(()), // ensamblado de fragmentos: hito 5
            _ => Err("wire frame sin kind".into()),
        }
    }

    /// Aplica un topic frame directo (params de la notificación `v4/conversation/frame`).
    pub fn apply_topic_frame(
        &mut self,
        frame: &Value,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<(), String> {
        let payload = frame.get("payload").ok_or("frame sin payload")?;
        match payload.get("kind").and_then(|k| k.as_str()) {
            Some("snapshot") => {
                // rows es una ventana: { window: [...], totalCount, firstRowId }
                let total = payload
                    .pointer("/snapshot/rows/totalCount")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(-1);
                if let Some(rows) = payload
                    .pointer("/snapshot/rows/window")
                    .and_then(|r| r.as_array())
                {
                    for row in rows {
                        self.emit_row_text(row, tx);
                    }
                }
                let _ = total;
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
                            let is_reasoning = self
                                .row_kinds
                                .get(&row_id)
                                .map(|k| k == "reasoning")
                                .unwrap_or(false);
                            // dedupe por fila; con el stream legacy activo, los
                            // row.delta de v4 solo contabilizan (no emiten)
                            let seen = self.row_emitted.get(&row_id).copied().unwrap_or(0);
                            if !self.legacy_live_active {
                                self.emit_stream(append, is_reasoning, tx);
                            }
                            self.row_emitted.insert(row_id, seen + append.chars().count());
                        }
                        Some("row.removed") => {
                            // reintento/edit: las filas >= fromRowId desaparecen;
                            // el stream vivo va a empezar de nuevo → resetear contadores
                            let from = delta.get("fromRowId").and_then(|v| v.as_i64()).unwrap_or(0);
                            self.row_emitted.retain(|k, _| *k < from);
                            self.row_kinds.retain(|k, _| *k < from);
                            let mut max_text = 0usize;
                            let mut max_reasoning = 0usize;
                            for (rid, seen) in &self.row_emitted {
                                match self.row_kinds.get(rid).map(|x| x.as_str()) {
                                    Some("reasoning") => max_reasoning = (*seen).max(max_reasoning),
                                    Some("assistantText") => max_text = (*seen).max(max_text),
                                    _ => {}
                                }
                            }
                            self.turn_text = max_text;
                            self.turn_reasoning = max_reasoning;
                            self.saw_completed = false;
                        }
                        Some("state.updated") => {
                            let status = delta.pointer("/patch/status").and_then(|v| v.as_str());
                            let phase = delta
                                .pointer("/patch/control/phase")
                                .and_then(|v| v.as_str());
                            if status == Some("running") || phase == Some("running") {
                                self.saw_running = true;
                            }
                            if phase == Some("error") {
                                if let Some(err) = delta.pointer("/patch/control/lastError") {
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
                                    let full = format!("⚠ error {code}: {msg}");
                                    if self.last_error_msg.as_deref() != Some(full.as_str()) {
                                        self.last_error_msg = Some(full.clone());
                                        let _ = tx.send(AgentEvent::Error(full));
                                    }
                                }
                            }
                            let finished = status == Some("idle")
                                || matches!(
                                    phase,
                                    Some("completedSuccess")
                                        | Some("completedInterrupted")
                                        | Some("error")
                                );
                            if finished && self.saw_running {
                                self.saw_running = false;
                                let _ = tx.send(AgentEvent::Done);
                            }
                        }
                        _ => {} // workflowRun.*, row.removed: hito 5
                    }
                }
                Ok(())
            }
            _ => Err("payload sin kind".into()),
        }
    }

    /// Delta del stream en vivo (session/event). Deduplica con los contadores
    /// del turno: los frames v4 que repiten el texto al final emiten solo el resto.
    pub fn apply_live_delta(&mut self, params: &Value, tx: &mpsc::UnboundedSender<AgentEvent>) {
        self.legacy_live_active = true;
        let payload = params.pointer("/payload").cloned().unwrap_or(Value::Null);
        let kind = payload.get("kind").and_then(|k| k.as_str()).unwrap_or("").to_string();
        let append = payload.get("delta").and_then(|v| v.as_str()).unwrap_or("");
        let kind = kind.as_str();
        match kind {
            "reasoning_delta" => self.emit_stream(append, true, tx),
            "text_delta" => self.emit_stream(append, false, tx),
            _ => {}
        }
    }

    /// Emite un append del streaming, actualizando el contador del turno.
    fn emit_stream(&mut self, append: &str, is_reasoning: bool, tx: &mpsc::UnboundedSender<AgentEvent>) {
        if append.is_empty() {
            return;
        }
        crate::agent::FRAME_DELTAS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let ev = if is_reasoning {
            AgentEvent::ReasoningDelta(append.to_string())
        } else {
            AgentEvent::Delta(append.to_string())
        };
        let _ = tx.send(ev);
        let counter = if is_reasoning {
            &mut self.turn_reasoning
        } else {
            &mut self.turn_text
        };
        *counter += append.chars().count();
    }

    /// Extras de una tool call: diff de archivo y salida (una sola vez por rowId).
    fn emit_tool_extras(&mut self, row: &Value, tx: &mpsc::UnboundedSender<AgentEvent>) {
        let row_id = row.get("rowId").and_then(|v| v.as_i64()).unwrap_or(-1);
        if !self.diff_emitted.contains(&row_id) {
            if let Some(d) = row.pointer("/output/display") {
                if d.get("kind").and_then(|k| k.as_str()) == Some("file_diff") {
                    let mut lines = Vec::new();
                    if let Some(patches) =
                        d.pointer("/structuredPatch").and_then(|p| p.as_array())
                    {
                        for patch in patches {
                            if let Some(pl) = patch.get("lines").and_then(|l| l.as_array()) {
                                for l in pl {
                                    if let Some(text) = l.as_str() {
                                        lines.push(text.to_string());
                                    }
                                }
                            }
                        }
                    }
                    let path = d
                        .get("filePath")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let additions = d.get("additions").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let deletions = d.get("deletions").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    // marcar como emitida SOLO cuando el patch trae contenido;
                    // los avistamientos tempranos llegan sin structuredPatch
                    if !lines.is_empty() {
                        self.diff_emitted.insert(row_id);
                        let _ = tx.send(AgentEvent::ToolDiff {
                            path,
                            additions,
                            deletions,
                            lines,
                        });
                    }
                }
            }
        }
        if !self.output_emitted.contains(&row_id) {
            // preview en vivo de bash (outputPreview) o salida final (display bash_output)
            let text = row
                .pointer("/outputPreview/text")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    row.pointer("/output/display").filter(|d| {
                        d.get("kind").and_then(|k| k.as_str()) == Some("bash_output")
                    })?
                    .get("output")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                });
            if let Some(text) = text {
                let clipped: Vec<&str> = text.lines().take(10).collect();
                let out = clipped.join("\n");
                if !out.trim().is_empty() {
                    self.output_emitted.insert(row_id);
                    let _ = tx.send(AgentEvent::ToolOutput(out));
                }
            }
        }
    }

    fn emit_row_text(&mut self, row: &Value, tx: &mpsc::UnboundedSender<AgentEvent>) {
        let kind = row.get("kind").and_then(|k| k.as_str()).unwrap_or_default().to_string();
        // los turnHeader NO resetean: en reintentos llegan a mitad de turno y
        // resetear haría re-emitir las filas finales que el stream vivo ya mostró.
        // El reset correcto lo señala row.removed (reintentos/edits).
        if kind == "turnHeader" {
            self.last_turn_header = row.get("rowId").and_then(|v| v.as_i64());
        }
        if kind == "toolCall" {
            self.emit_tool_extras(row, tx);
            let tool = row.get("toolName").and_then(|t| t.as_str()).unwrap_or("?");
            let status = row.get("status").and_then(|t| t.as_str()).unwrap_or("?");
            let input = row.get("inputText").and_then(|t| t.as_str()).unwrap_or("");
            let summary = match status {
                "pendingApproval" => format!("⚠ {tool} — esperando aprobación"),
                "running" | "inputStreaming" => format!("⚙ {tool} ▷ ejecutando"),
                "success" => format!("✔ {tool}"),
                "error" => format!("✖ {tool} — error"),
                "cancelled" => format!("⊘ {tool} — cancelado"),
                other => format!("⚙ {tool} ({other})"),
            };
            let detail = if input.is_empty() {
                summary
            } else {
                let one_line: String = input.lines().take(1).collect::<Vec<_>>().join(" ");
                let clipped: String = one_line.chars().take(120).collect();
                format!("{summary} — {clipped}")
            };
            let call_id = row
                .get("toolCallId")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();
            let _ = tx.send(AgentEvent::ToolCall { call_id, line: detail });
            return;
        }
        if kind == "userInput" {
            let row_id = row.get("rowId").and_then(|v| v.as_i64()).unwrap_or(-1);
            self.row_kinds.insert(row_id, kind.clone());
            let text = row.get("text").and_then(|t| t.as_str()).unwrap_or_default();
            if !text.is_empty() {
                let _ = tx.send(AgentEvent::HistoryUser(text.to_string()));
            }
            return;
        }
        if kind != "assistantText" && kind != "reasoning" {
            return;
        }
        let row_id = row.get("rowId").and_then(|v| v.as_i64()).unwrap_or(-1);
        self.row_kinds.insert(row_id, kind.clone());
        let text = row.get("text").and_then(|t| t.as_str()).unwrap_or_default();
        let total = text.chars().count();
        // consumido = lo ya emitido para ESTA fila, o lo que el stream vivo
        // mostró del turno (seeding, solo la primera vez que vemos la fila)
        let consumed = match self.row_emitted.get(&row_id) {
            Some(seen) => *seen,
            None => {
                let live = if kind == "reasoning" { self.turn_reasoning } else { self.turn_text };
                live.min(total)
            }
        };
        if total > consumed {
            let new_text: String = text.chars().skip(consumed).collect();
            let ev = if kind == "reasoning" {
                AgentEvent::ReasoningDelta(new_text)
            } else {
                AgentEvent::Delta(new_text)
            };
            let _ = tx.send(ev);
        }
        self.row_emitted.insert(row_id, total);
        if kind == "reasoning" {
            self.turn_reasoning = self.turn_reasoning.max(total);
        } else {
            self.turn_text = self.turn_text.max(total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_call_row(row_id: i64, status: &str) -> Value {
        json!({
            "rowId": row_id,
            "kind": "toolCall",
            "toolName": "Edit",
            "status": status,
            "inputText": "{\"file_path\":\"/tmp/x.txt\",\"old\":\"a\",\"new\":\"b\"}",
            "output": {
                "display": {
                    "kind": "file_diff",
                    "filePath": "/tmp/x.txt",
                    "additions": 1,
                    "deletions": 1,
                    "structuredPatch": [
                        { "oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
                          "lines": ["- linea vieja", "+ linea nueva", "  contexto"] }
                    ]
                }
            }
        })
    }

    #[test]
    fn tool_diff_emitted_once_per_row() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut p = FrameProjector::default();
        let row = tool_call_row(7, "success");
        p.emit_row_text(&row, &tx);
        let mut diffs = 0;
        let mut sample = None;
        while let Ok(ev) = rx.try_recv() {
            if let AgentEvent::ToolDiff { path, additions, deletions, lines } = ev {
                diffs += 1;
                sample = Some((path, additions, deletions, lines));
            }
        }
        assert_eq!(diffs, 1);
        let (path, a, d, lines) = sample.unwrap();
        assert_eq!(path, "/tmp/x.txt");
        assert_eq!((a, d), (1, 1));
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with('-'));
        assert!(lines[1].starts_with('+'));
        // re-upsert de la misma fila: no re-emite
        p.emit_row_text(&row, &tx);
        while rx.try_recv().is_ok() {}
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn turn_header_never_resets_row_removed_does() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut p = FrameProjector::default();
        let header = |id: i64| json!({ "rowId": id, "kind": "turnHeader" });
        p.emit_row_text(&header(1), &tx);
        p.turn_text = 10;
        p.emit_row_text(&header(2), &tx);
        // headers nunca resetean (los reintentos llegan a mitad de turno)
        assert_eq!(p.turn_text, 10);
        p.emit_row_text(&header(3), &tx);
        assert_eq!(p.turn_text, 10);
        // row.removed (reintento): poda filas >= fromRowId y resetea contadores
        p.emit_row_text(&json!({ "rowId": 9, "kind": "assistantText", "text": "0123456789" }), &tx);
        assert_eq!(p.turn_text, 10);
        let frame = json!({ "payload": { "kind": "deltas", "deltas": [
            { "op": "row.removed", "fromRowId": 5 }
        ]}});
        p.apply_topic_frame(&frame, &tx);
        assert_eq!(p.turn_text, 0);
        assert!(!p.row_emitted.contains_key(&9));
    }

    #[test]
    fn live_delta_then_final_row_no_duplicate() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut p = FrameProjector::default();
        p.emit_row_text(&json!({ "rowId": 1, "kind": "turnHeader" }), &tx);
        // streaming en vivo: 10 chars de thinking
        p.apply_live_delta(&json!({ "payload": { "kind": "reasoning_delta", "delta": "0123456789" } }), &tx);
        // fila final completa (mismos 10 chars): no debe emitir nada nuevo
        p.emit_row_text(&json!({ "rowId": 5, "kind": "reasoning", "text": "0123456789" }), &tx);
        let mut total = 0;
        while let Ok(ev) = rx.try_recv() {
            if let AgentEvent::ReasoningDelta(d) = ev {
                total += d.chars().count();
            }
        }
        assert_eq!(total, 10, "el texto final no debe duplicar el stream vivo");
    }
}
