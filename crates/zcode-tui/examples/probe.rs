//! Sonda headless: session/create + v4 subscribe + session/send contra app-server real.
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use std::process::Stdio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = zcode_launcher::find_runtime(None)?;
    println!("runtime: {} env={:?}", runtime.program, runtime.env);
    let workspace = std::env::current_dir()?.display().to_string();

    let mut child = tokio::process::Command::new(&runtime.program)
        .args(&runtime.args)
        .envs(runtime.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .arg("--surface").arg("desktop").arg("--cwd").arg(&workspace)
        .stdin(Stdio::piped()).stdout(Stdio::piped())
        .stderr(Stdio::from(
            std::fs::File::create("/tmp/zcode-tui-appserver.log")?,
        ))
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    async fn send(stdin: &mut tokio::process::ChildStdin, id: u64, method: &str, params: Value) {
        let req = json!({ "id": id, "method": method, "params": params });
        let _ = stdin.write_all(serde_json::to_string(&req).unwrap().as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.flush().await;
    }
    async fn respond(stdin: &mut tokio::process::ChildStdin, id: &Value, result: Value) {
        let msg = json!({ "id": id, "result": result });
        let _ = stdin.write_all(serde_json::to_string(&msg).unwrap().as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.flush().await;
    }

    send(&mut stdin, 1, "session/create", json!({
        "workspace": { "workspacePath": workspace, "workspaceKey": workspace },
        "model": model_from_env(),
    })).await;

    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut next_id = 2u64;
    let mut session_id: Option<String> = None;
    let mut subscribed = false;
    let mut sent = false;
    let mut saw_running = false;
    let mut finished = false;
    let mut text = String::new();
    let mut reasoning = String::new();

    while !finished {
        if tokio::time::Instant::now() > deadline { println!("timeout global"); break; }
        let line = match tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line()).await {
            Err(_) => {
                if sent && !subscribed { continue; }
                continue;
            }
            Ok(Err(e)) => { println!("read err: {e}"); break; }
            Ok(Ok(None)) => { println!("stdout cerrado"); break; }
            Ok(Ok(Some(l))) => l,
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue; };

        // peticiones del servidor → responder por defecto
        if let (Some(id), Some(method)) = (msg.get("id").cloned(), msg.get("method").and_then(|m| m.as_str()).map(|s| s.to_string())) {
            match method.as_str() {
                "session/requestRuntimePreferences" => {
                    respond(&mut stdin, &id, json!({
                        "nativeSearchEnhancementsEnabled": false,
                        "memoryEnabled": false,
                        "askUserQuestionAutoResolutionEnabled": true,
                    })).await;
                }
                "interaction/requestOfficialMcpAuthHeaders" => {
                    respond(&mut stdin, &id, json!({ "headers": {} })).await;
                }
                "interaction/requestPermission" => {
                    let opts = msg.pointer("/params/options").and_then(|o| o.as_array());
                    let allow_id = opts
                        .map(|arr| {
                            arr.iter()
                                .find_map(|o| {
                                    let oid = o.get("optionId")?.as_str()?;
                                    oid.contains("allow").then(|| oid.to_string())
                                })
                                .or_else(|| arr.first().and_then(|o| o.get("optionId")).and_then(|v| v.as_str()).map(|s| s.to_string()))
                        })
                        .flatten()
                        .unwrap_or_else(|| "allow".into());
                    println!("[permiso] auto-allow (decision)");
                    respond(&mut stdin, &id, json!({ "decision": "allow" })).await;
                }
                _ => {}
            }
            continue;
        }
        // respuesta a session/create
        if msg.get("id").and_then(|v| v.as_u64()) == Some(1) {
            if let Some(err) = msg.get("error") {
                println!("ERROR session/create: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or("?"));
                break;
            }
            if let Some(arr) = msg.pointer("/result/settings/model/available").and_then(|v| v.as_array()) {
                println!("=== MODELOS DISPONIBLES ({}) ===", arr.len());
                for m in arr {
                    println!("  [{}] {} — {} (nivel default: {})",
                        m.pointer("/ref/providerId").and_then(|v| v.as_str()).unwrap_or("?"),
                        m.get("label").and_then(|v| v.as_str()).unwrap_or("?"),
                        m.get("providerLabel").and_then(|v| v.as_str()).unwrap_or("?"),
                        m.pointer("/reasoning/defaultLevel").and_then(|v| v.as_str()).unwrap_or("?"));
                }
            }
            session_id = msg.pointer("/result/session/sessionId").and_then(|v| v.as_str()).map(|s| s.to_string());
            println!("sessionId: {}", session_id.as_deref().unwrap_or("?"));
            if let Some(sid) = &session_id {
                send(&mut stdin, next_id, "session/subscribe", json!({
                    "sessionId": sid,
                    "deliveryKind": "desktop-continuous",
                })).await;
                next_id += 1;
                send(&mut stdin, next_id, "v4/conversation/subscribe", json!({
                    "topic": format!("conversation/{sid}"),
                    "connectionId": format!("zcode-tui-probe-{}", std::process::id()),
                    "clientMode": "desktop-continuous",
                    "visibility": "foreground",
                })).await;
                next_id += 1;
            }
            continue;
        }
        if let Some(method) = msg.get("method").and_then(|m| m.as_str()) {
            match method {
                "v4/conversation/frame" => {
                    // reportar ventana drenada
                    if let Some(sid) = &session_id.clone() {
                        let _ = &sid;
                    }
                    send(&mut stdin, next_id, "v4/connection/flow", json!({
                        "connectionId": format!("zcode-tui-probe-{}", std::process::id()),
                        "state": "drained",
                    })).await;
                    next_id += 1;

                    let frame = if msg.pointer("/params/kind").is_some() {
                        msg.pointer("/params/frame").cloned().unwrap_or(Value::Null)
                    } else {
                        msg.pointer("/params").cloned().unwrap_or(Value::Null)
                    };
                    if !subscribed {
                        subscribed = true;
                        println!("[suscripción OK]");
                    }
                    let payload = frame.get("payload").cloned().unwrap_or(Value::Null);
                    let kind = payload.get("kind").and_then(|k| k.as_str()).unwrap_or("").to_string();
                    match kind.as_str() {
                        "deltas" => {
                            if let Some(arr) = payload.get("deltas").and_then(|d| d.as_array()) {
                                for d in arr {
                                    match d.get("op").and_then(|o| o.as_str()) {
                                        Some("row.appended") | Some("row.upserted") => {
                                            if let Some(row) = d.get("row") {
                                                let rk = row.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                                                let rt = row.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                                let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() % 100000;
                                                match rk {
                                                    "assistantText" => { text.push_str(rt); println!("[{t}] [texto +{} chars: {:?}]", rt.chars().count(), rt.chars().take(30).collect::<String>()); }
                                                    "reasoning" => { reasoning.push_str(rt); println!("[{t}] [thinking +{} chars: {:?}]", rt.chars().count(), rt.chars().take(30).collect::<String>()); }
                                                    other => println!("[{t}] [row {other}]"),
                                                }
                                            }
                                        }
                                        Some("row.delta") => {
                                            let append = d.get("append").and_then(|a| a.as_str()).unwrap_or("");
                                            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() % 100000;
                                            println!("[{t}] [delta +{}: {:?}]", append.chars().count(), append.chars().take(25).collect::<String>());
                                            text.push_str(append);
                                        }
                                        Some("state.updated") => {
                                            let phase = d.pointer("/patch/control/phase").and_then(|v| v.as_str()).unwrap_or("");
                                            let status = d.pointer("/patch/status").and_then(|v| v.as_str()).unwrap_or("");
                                            if phase == "error" || d.pointer("/patch/lastError").is_some() {
                                                println!("[ERROR DETALLE] {}", serde_json::to_string(d).unwrap_or_default().chars().take(2200).collect::<String>());
                                            }
                                            let retry = d.pointer("/patch/control/apiRetry");
                                            if phase == "running" || status == "running" { saw_running = true; }
                                            let done = status == "idle" || matches!(phase, "completedSuccess" | "completedInterrupted" | "error");
                                            if done && saw_running { finished = true; }
                                            if let Some(r) = retry {
                                                if !r.is_null() {
                                                    println!("[apiRetry] {}", serde_json::to_string(r).unwrap_or_default().chars().take(200).collect::<String>());
                                                }
                                            }
                                            println!("[state] phase={phase} status={status}");
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        "snapshot" => {
                            let phase = payload.pointer("/snapshot/control/phase").and_then(|v| v.as_str()).unwrap_or("");
                            let retry = payload.pointer("/snapshot/control/apiRetry");
                            if let Some(r) = retry {
                                if !r.is_null() {
                                    println!("[apiRetry snapshot] {}", serde_json::to_string(r).unwrap_or_default().chars().take(200).collect::<String>());
                                }
                            }
                            let _ = phase;
                        }
                        _ => {}
                    }
                    if subscribed && !sent {
                        if let Some(sid) = &session_id {
                            println!("enviando session/send…");
                            send(&mut stdin, next_id, "session/send", json!({
                                "sessionId": sid,
                                "content": "Edita /tmp/zcode-tui-diff.txt: cambia 'linea dos' por 'linea DOS editada'.",
                                "modelSelection": model_from_env(),
                            })).await;
                            next_id += 1;
                            sent = true;
                        }
                    }
                }
                "state.updated" => {
                    if let Some(err) = msg.pointer("/params/patch/lastError") {
                        println!("[LASTERROR COMPLETO] {}", serde_json::to_string(err).unwrap_or_default());
                    }
                    let status = msg.pointer("/params/patch/status").and_then(|v| v.as_str()).unwrap_or("");
                    let phase = msg.pointer("/params/patch/control/phase").and_then(|v| v.as_str()).unwrap_or("");
                    let retry = msg.pointer("/params/patch/control/apiRetry");
                    if status == "running" || phase == "running" { saw_running = true; }
                    if !retry.map(|r| r.is_null()).unwrap_or(true) {
                        println!("[apiRetry] {}", serde_json::to_string(retry.unwrap()).unwrap_or_default().chars().take(200).collect::<String>());
                    }
                    println!("[notif] phase={phase} status={status}");
                    let done = status == "idle" || matches!(phase, "completedSuccess" | "completedInterrupted" | "error");
                    if done && saw_running { finished = true; }
                }
                "session/event" => {
                    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() % 100000;
                    let pk = msg.pointer("/params/payload/kind")
                        .and_then(|v| v.as_str()).unwrap_or("");
                    let delta = msg.pointer("/params/payload/delta")
                        .and_then(|v| v.as_str()).unwrap_or("");
                    match pk {
                        "text_delta" | "reasoning_delta" => {
                            println!("[{t}][LEGACY] [{pk} +{}: {:?}]", delta.chars().count(), delta.chars().take(20).collect::<String>());
                        }
                        "" => { println!("[{t}] RAW {}", line.chars().take(150).collect::<String>()); }
                        other => { println!("[{t}] [{other}]"); }
                    }
                }
                _ => {}
            }
        }
    }
    println!("=== FIN === thinking: {:?} | texto: {:?}", reasoning.chars().take(150).collect::<String>(), text.chars().take(200).collect::<String>());
    let _ = child.kill().await;
    Ok(())
}

fn model_from_env() -> Value {
    let spec = std::env::var("ZCODE_TUI_MODEL").unwrap_or_else(|_| "zai-api/GLM-5.3-Flash@high".into());
    let (sel, level) = match spec.split_once('@') {
        Some((s, l)) => (s.to_string(), l.to_string()),
        None => (spec.clone(), "high".to_string()),
    };
    let mut parts = sel.splitn(2, '/');
    let provider = parts.next().unwrap_or("zai-api").to_string();
    let model = parts.next().unwrap_or("GLM-5.3-Flash").to_string();
    json!({ "providerId": provider, "modelId": model, "options": { "reasoningLevel": level } })
}
