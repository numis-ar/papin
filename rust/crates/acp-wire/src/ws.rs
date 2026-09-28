use crate::Envelope;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use tracing::warn;

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
/// Connect to a WebSocket URL (ws:// or wss://).
pub async fn ws_connect(url: &str) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    ws_connect_with(url, &[]).await
}

/// Connect with extra request headers (e.g. `Authorization`).
pub async fn ws_connect_with(
    url: &str,
    headers: &[(&str, &str)],
) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url.into_client_request()?;
    for (k, v) in headers {
        request.headers_mut().insert(
            k.parse::<http::HeaderName>().expect("header name"),
            v.parse().expect("header value"),
        );
    }
    let (stream, _) = connect_async(request).await?;
    Ok(stream)
}

/// Serialize an envelope into a WS text message.
pub fn envelope_to_message(env: &Envelope) -> Message {
    Message::Text(serde_json::to_string(env).unwrap_or_default().into())
}

/// Send one envelope as a WS text frame.
pub async fn ws_send_envelope(
    stream: &mut WsStream,
    env: &Envelope,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    stream.send(envelope_to_message(env)).await
}

/// Receive the next envelope; `None` on close. Skips non-JSON frames with a warning.
pub async fn ws_recv_envelope(stream: &mut WsStream) -> Option<Envelope> {
    loop {
        let msg = stream.next().await?;
        match msg {
            Ok(Message::Text(text)) => match serde_json::from_str::<Envelope>(&text) {
                Ok(env) => return Some(env),
                Err(err) => {
                    warn!(?err, "ignoring malformed ACP frame");
                }
            },
            Ok(Message::Close(_)) => return None,
            Ok(_) => continue,
            Err(err) => {
                warn!(?err, "websocket error");
                return None;
            }
        }
    }
}
