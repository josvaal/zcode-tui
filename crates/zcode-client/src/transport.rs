//! WebSocket transport adapter: bridges binary WS messages to the frame
//! channel the [`crate::channel::ChannelClient`] consumes.

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

use crate::{
    error::ClientError,
    framing::{Frame, FrameParser},
};

/// Connects to a zcode HTTP server's `/ws` endpoint and pumps frames in both
/// directions until the socket closes. Returns `(outbound, inbound)`.
/// Auth: `?token=` query param (the server accepts it, see `hasValidLiteToken`).
pub async fn spawn_websocket(
    url: &str,
    token: Option<&str>,
) -> Result<
    (
        mpsc::UnboundedSender<Frame>,
        mpsc::UnboundedReceiver<Frame>,
    ),
    ClientError,
> {
    let request = mk_request(url, token)?;
    let (ws_stream, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| ClientError::Handshake(format!("websocket connect failed: {e}")))?;

    let (frame_tx, frame_rx) = mpsc::unbounded_channel::<Frame>();
    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    // outbound: frames -> WS binary messages
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Frame>();
    tokio::spawn(async move {
        while let Some(frame) = outbound_rx.recv().await {
            if ws_sink
                .send(Message::Binary(frame.encode().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // inbound: WS binary messages -> frames
    tokio::spawn(async move {
        let mut parser = FrameParser::new();
        while let Some(msg) = ws_stream.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    parser.feed(&data);
                    while let Some(frame) = parser.next_frame() {
                        if frame_tx.send(frame).is_err() {
                            return;
                        }
                    }
                }
                Ok(Message::Ping(_)) => {
                    // tungstenite responde los pings automáticamente
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });

    Ok((outbound_tx, frame_rx))
}

fn mk_request(url: &str, token: Option<&str>) -> Result<http_uri::Uri, ClientError> {
    let mut request = url
        .into_client_request()
        .map_err(|e| ClientError::Handshake(format!("invalid url: {e}")))?;
    if let Some(token) = token {
        let uri = request.uri().clone();
        let sep = if uri.query().is_some() { "&" } else { "?" };
        let new = format!("{uri}{sep}token={token}");
        let parsed: http_uri::Uri = new
            .parse()
            .map_err(|e| ClientError::Handshake(format!("bad uri: {e}")))?;
        *request.uri_mut() = parsed;
    }
    Ok(request.uri().clone())
}

/// Minimal re-export shim so `mk_request` stays readable.
mod http_uri {
    pub type Uri = tokio_tungstenite::tungstenite::http::Uri;
}
