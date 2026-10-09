//! Layer-3: channel RPC client, mirroring `ChannelClient` from
//! `packages/rpc/src/channelClient.ts`.
//!
//! Every message is `serialize(header) + serialize(body)` where the header is
//! `[type, id, channelName, commandName]`:
//!
//! - requests:  `Promise = 100`, `PromiseCancel = 101`, `EventListen = 102`, `EventDispose = 103`
//! - responses: `Initialize = 200`, `PromiseSuccess = 201`, `PromiseError = 202`,
//!   `PromiseErrorObj = 203`, `EventFire = 204`
//!
//! The server sends an `Initialize` frame right after the connection opens;
//! [`ChannelClient::connect`] waits for it before returning.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
};

use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    error::{ClientError, RpcError},
    framing::Frame,
    value::{deserialize, serialize_to_vec, RpcValue},
};

pub mod req {
    pub const PROMISE: i64 = 100;
    pub const PROMISE_CANCEL: i64 = 101;
    pub const EVENT_LISTEN: i64 = 102;
    pub const EVENT_DISPOSE: i64 = 103;
}

pub mod res {
    pub const INITIALIZE: i64 = 200;
    pub const PROMISE_SUCCESS: i64 = 201;
    pub const PROMISE_ERROR: i64 = 202;
    pub const PROMISE_ERROR_OBJ: i64 = 203;
    pub const EVENT_FIRE: i64 = 204;
}

type PendingMap = Arc<Mutex<HashMap<u32, Pending>>>;

enum Pending {
    Call(oneshot::Sender<Result<RpcValue, RpcError>>),
    Event(mpsc::UnboundedSender<RpcValue>),
}

#[derive(Clone)]
pub struct ChannelClient {
    frame_tx: mpsc::UnboundedSender<Frame>,
    next_id: Arc<AtomicU32>,
    pending: PendingMap,
}

impl ChannelClient {
    /// Splits a full-duplex frame channel into a client (send half) and its
    /// reader task (which routes responses/events). Waits for the server's
    /// `Initialize` before returning.
    pub async fn connect(
        frame_tx: mpsc::UnboundedSender<Frame>,
        mut frame_rx: mpsc::UnboundedReceiver<Frame>,
    ) -> Result<ChannelClient, ClientError> {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (init_tx, init_rx) = oneshot::channel::<Result<(), ClientError>>();

        let pending_reader = pending.clone();
        tokio::spawn(async move {
            let outcome = read_loop(&mut frame_rx, pending_reader, init_tx).await;
            if let Err(err) = outcome {
                tracing::debug!("reader loop ended: {err}");
            }
        });

        let client = ChannelClient {
            frame_tx,
            next_id: Arc::new(AtomicU32::new(0)),
            pending,
        };
        match tokio::time::timeout(std::time::Duration::from_secs(10), init_rx).await {
            Ok(Ok(Ok(()))) => Ok(client),
            Ok(Ok(Err(err))) => Err(err),
            Ok(Err(_)) => Err(ClientError::Handshake("reader task died".into())),
            Err(_) => Err(ClientError::Handshake("timeout: no Initialize in 10s".into())),
        }
    }

    pub fn call(
        &self,
        channel: &str,
        command: &str,
        arg: RpcValue,
    ) -> oneshot::Receiver<Result<RpcValue, RpcError>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, Pending::Call(tx));
        let header = RpcValue::Array(vec![
            RpcValue::Int(req::PROMISE),
            RpcValue::Int(id as i64),
            RpcValue::String(channel.into()),
            RpcValue::String(command.into()),
        ]);
        let mut payload = serialize_to_vec(&header);
        payload.extend_from_slice(&serialize_to_vec(&arg));
        let _ = self.frame_tx.send(Frame::regular(payload));
        rx
    }

    pub fn listen(
        &self,
        channel: &str,
        event: &str,
        arg: RpcValue,
    ) -> mpsc::UnboundedReceiver<RpcValue> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending.lock().unwrap().insert(id, Pending::Event(tx));
        let header = RpcValue::Array(vec![
            RpcValue::Int(req::EVENT_LISTEN),
            RpcValue::Int(id as i64),
            RpcValue::String(channel.into()),
            RpcValue::String(event.into()),
        ]);
        let mut payload = serialize_to_vec(&header);
        payload.extend_from_slice(&serialize_to_vec(&arg));
        let _ = self.frame_tx.send(Frame::regular(payload));
        rx
    }

    pub fn dispose_event(&self, listen_id: u32) {
        self.pending.lock().unwrap().remove(&listen_id);
        let header = RpcValue::Array(vec![
            RpcValue::Int(req::EVENT_DISPOSE),
            RpcValue::Int(listen_id as i64),
            RpcValue::String(String::new()),
            RpcValue::String(String::new()),
        ]);
        let _ = self.frame_tx.send(Frame::regular(serialize_to_vec(&header)));
    }
}

async fn read_loop(
    frame_rx: &mut mpsc::UnboundedReceiver<Frame>,
    pending: PendingMap,
    init_tx: oneshot::Sender<Result<(), ClientError>>,
) -> Result<(), ClientError> {
    let mut init_tx = Some(init_tx);
    let mut init_sent = false;
    while let Some(frame) = frame_rx.recv().await {
        let header_bytes = extract_header(&frame.payload)?;
        let body_bytes = extract_body(&frame.payload)?;
        let header = deserialize(&header_bytes).map_err(ClientError::Malformed)?;
        // A body is always on the wire (`undefined` = single 0x00 byte);
        // tolerate truncation defensively.
        let body = deserialize(&body_bytes).unwrap_or(RpcValue::Undefined);
        let fields = match &header {
            RpcValue::Array(items) => items,
            _ => return Err(ClientError::Malformed("response header is not an array".into())),
        };
        let rtype = match fields.first() {
            Some(RpcValue::Int(t)) => *t,
            _ => return Err(ClientError::Malformed("response header missing type".into())),
        };

        if !init_sent {
            init_sent = true;
            let tx = init_tx.take();
            if rtype == res::INITIALIZE {
                if let Some(tx) = tx {
                    let _ = tx.send(Ok(()));
                }
                continue;
            }
            if let Some(tx) = tx {
                let _ = tx.send(Err(ClientError::Handshake(format!(
                    "expected Initialize, got {rtype}"
                ))));
            }
            continue;
        }

        let id = match fields.get(1) {
            Some(RpcValue::Int(id)) => *id as u32,
            _ => 0,
        };
        let mut pending = pending.lock().unwrap();
        match rtype {
            res::PROMISE_SUCCESS => {
                if let Some(Pending::Call(tx)) = pending.remove(&id) {
                    let _ = tx.send(Ok(body));
                }
            }
            res::PROMISE_ERROR | res::PROMISE_ERROR_OBJ => {
                if let Some(Pending::Call(tx)) = pending.remove(&id) {
                    let json = body.to_json();
                    let (name, message) = match &json {
                        serde_json::Value::Object(map) => (
                            map.get("name").and_then(|v| v.as_str()).unwrap_or("Error"),
                            map.get("message").and_then(|v| v.as_str()).unwrap_or(""),
                        ),
                        _ => ("Error", ""),
                    };
                    let _ = tx.send(Err(RpcError {
                        name: name.into(),
                        message: message.into(),
                        data: Some(json),
                    }));
                }
            }
            res::EVENT_FIRE => {
                if let Some(Pending::Event(tx)) = pending.get(&id) {
                    let _ = tx.send(body);
                }
            }
            _ => {}
        }
    }
    Err(ClientError::Closed)
}

/// Response layout is `serialize(header) + serialize(body)`; to recover the
/// body we re-parse the header and skip past its exact encoded length.
fn split_response(payload: &[u8]) -> (usize, usize) {
    // Encode-time cost of the header equals its serialized length; we recover
    // it by parsing once and recording cursor position via a re-serialize.
    // Simpler: deserialize header with a peeking reader is not exposed, so we
    // compute it by matching serialized length.
    let header_len = serialized_header_len(payload);
    (header_len, payload.len() - header_len)
}

fn extract_header(payload: &[u8]) -> Result<Vec<u8>, ClientError> {
    let (hlen, _) = split_response(payload);
    Ok(payload[..hlen].to_vec())
}

fn extract_body(payload: &[u8]) -> Result<Vec<u8>, ClientError> {
    let (hlen, blen) = split_response(payload);
    Ok(payload[hlen..hlen + blen].to_vec())
}

fn serialized_header_len(payload: &[u8]) -> usize {
    // Walk the first serialized value to find where it ends.
    let mut pos = 1usize; // skip tag
    let tag = payload[0];
    match tag {
        0 => 1,
        1 | 2 | 3 | 5 => {
            // VQL length prefix + data
            let mut v: u32 = 0;
            let mut n = 0;
            loop {
                let b = payload[pos];
                pos += 1;
                v |= ((b & 0x7F) as u32) << n;
                if b & 0x80 == 0 {
                    break;
                }
                n += 7;
            }
            pos + v as usize
        }
        4 => {
            // array: walk elements
            let mut v: u32 = 0;
            let mut n = 0;
            loop {
                let b = payload[pos];
                pos += 1;
                v |= ((b & 0x7F) as u32) << n;
                if b & 0x80 == 0 {
                    break;
                }
                n += 7;
            }
            let mut end = pos;
            for _ in 0..v {
                end += serialized_value_len(&payload[end..]);
            }
            end
        }
        6 => {
            loop {
                let b = payload[pos];
                pos += 1;
                if b & 0x80 == 0 {
                    break;
                }
            }
            pos
        }
        _ => payload.len(),
    }
}

fn serialized_value_len(payload: &[u8]) -> usize {
    let mut pos = 1usize;
    match payload[0] {
        0 => 1,
        1 | 2 | 3 | 5 => {
            let mut v: u32 = 0;
            let mut n = 0;
            loop {
                let b = payload[pos];
                pos += 1;
                v |= ((b & 0x7F) as u32) << n;
                if b & 0x80 == 0 {
                    break;
                }
                n += 7;
            }
            pos + v as usize
        }
        4 => {
            let mut v: u32 = 0;
            let mut n = 0;
            loop {
                let b = payload[pos];
                pos += 1;
                v |= ((b & 0x7F) as u32) << n;
                if b & 0x80 == 0 {
                    break;
                }
                n += 7;
            }
            let mut end = pos;
            for _ in 0..v {
                end += serialized_value_len(&payload[end..]);
            }
            end
        }
        6 => {
            loop {
                let b = payload[pos];
                pos += 1;
                if b & 0x80 == 0 {
                    break;
                }
            }
            pos
        }
        _ => payload.len(),
    }
}

/// Keeps the WS message import referenced for transport adapters.
#[allow(dead_code)]
fn _ws_message_type_hint(_m: &Message) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{obj, serialize};
    use serde_json::json;

    /// Minimal in-process mock of zcode's `ChannelServer`: sends Initialize and
    /// echoes Promise requests back as PromiseSuccess.
    fn mock_server_respond(payload: &[u8]) -> Vec<u8> {
        let header = deserialize(payload).unwrap();
        // body starts right after the header's serialized bytes
        let hlen = serialized_header_len(payload);
        let body = deserialize(&payload[hlen..]).unwrap_or(RpcValue::Undefined);
        let fields = match &header {
            RpcValue::Array(f) => f.clone(),
            _ => panic!(),
        };
        assert_eq!(fields[0], RpcValue::Int(req::PROMISE));
        let id = match fields[1] {
            RpcValue::Int(id) => id,
            _ => panic!(),
        };
        let mut out = Vec::new();
        let resp_header = RpcValue::Array(vec![RpcValue::Int(res::PROMISE_SUCCESS), RpcValue::Int(id)]);
        serialize(&resp_header, &mut out);
        serialize(
            &RpcValue::Object(json!({ "echo": body.to_json(), "channel": fields[2].to_json() })),
            &mut out,
        );
        out
    }

    fn init_frame() -> Frame {
        let mut payload = serialize_to_vec(&RpcValue::Array(vec![RpcValue::Int(res::INITIALIZE)]));
        // the real server always serializes a body; `undefined` is one 0x00 byte
        payload.extend_from_slice(&serialize_to_vec(&RpcValue::Undefined));
        Frame::regular(payload)
    }

    #[tokio::test]
    async fn call_roundtrip_against_mock_server() {
        let (client_tx, client_rx) = mpsc::unbounded_channel::<Frame>();
        let client_tx2 = client_tx.clone();
        let (server_tx, server_rx) = mpsc::unbounded_channel::<Frame>();

        // pump: what the client sends goes to the "server", responses come back framed
        tokio::spawn(async move {
            let mut server_rx = server_rx;
            while let Some(frame) = server_rx.recv().await {
                let resp = mock_server_respond(&frame.payload);
                let _ = client_tx.send(Frame::regular(resp));
            }
        });

        // server's Initialize (straight to the client's inbound channel)
        let _ = client_tx2.send(init_frame());

        let _server_tx_keepalive = server_tx;
        let client = ChannelClient::connect(_server_tx_keepalive, client_rx).await.unwrap();

        let arg = obj(&[("prompt", json!("hola"))]);
        let result = client
            .call("zcode-agent", "sendMessage", arg.clone())
            .await
            .unwrap()
            .unwrap();
        let json = result.to_json();
        assert_eq!(json["channel"], json!("zcode-agent"));
        assert_eq!(json["echo"]["prompt"], json!("hola"));
    }

    #[tokio::test]
    async fn event_listen_receives_fires() {
        let (client_tx, client_rx) = mpsc::unbounded_channel::<Frame>();
        let (server_tx, mut server_rx) = mpsc::unbounded_channel::<Frame>();

        let _ = client_tx.send(init_frame());

        let client = ChannelClient::connect(server_tx.clone(), client_rx).await.unwrap();
        let mut events = client.listen("zcode-agent", "onEvent", RpcValue::Undefined);

        // The server sees the EventListen request; capture its id and fire an event.
        let frame = server_rx.recv().await.unwrap();
        let header = deserialize(&frame.payload).unwrap();
        let id = match &header {
            RpcValue::Array(f) => match f[1] {
                RpcValue::Int(id) => id as u32,
                _ => panic!(),
            },
            _ => panic!(),
        };
        let fire = RpcValue::Array(vec![RpcValue::Int(res::EVENT_FIRE), RpcValue::Int(id as i64)]);
        let mut payload = serialize_to_vec(&fire);
        payload.extend_from_slice(&serialize_to_vec(&RpcValue::Object(
            json!({ "delta": "tok" }),
        )));
        let _ = client_tx.send(Frame::regular(payload));

        let got = events.recv().await.unwrap();
        assert_eq!(got.to_json()["delta"], json!("tok"));
    }
}

