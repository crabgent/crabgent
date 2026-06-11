//! Socket Mode transport abstraction.

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::error::SlackError;
use crate::events::SocketModeEnvelope;

type TungsteniteStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Socket Mode input observed by the connection loop.
#[derive(Debug, Clone)]
pub enum SocketModeFrame {
    /// A Slack Socket Mode envelope.
    Envelope(SocketModeEnvelope),
    /// A non-envelope WebSocket frame that proves the connection is alive.
    Heartbeat,
}

/// Socket Mode client contract used by the connection loop.
#[async_trait]
pub trait SocketModeClient: Send + Sync {
    async fn connect(&self, url: &str) -> Result<(), SlackError>;
    async fn next_envelope(&self) -> Result<SocketModeEnvelope, SlackError>;
    async fn ack(&self, envelope_id: &str) -> Result<(), SlackError>;

    async fn next_frame(&self) -> Result<SocketModeFrame, SlackError> {
        self.next_envelope().await.map(SocketModeFrame::Envelope)
    }

    async fn ping(&self) -> Result<(), SlackError> {
        Ok(())
    }

    async fn close(&self) -> Result<(), SlackError> {
        Ok(())
    }
}

/// Production Socket Mode client backed by `tokio-tungstenite`.
#[derive(Default)]
pub struct TungsteniteSocketModeClient {
    stream: Mutex<Option<TungsteniteStream>>,
}

impl TungsteniteSocketModeClient {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stream: Mutex::const_new(None),
        }
    }
}

#[async_trait]
impl SocketModeClient for TungsteniteSocketModeClient {
    async fn connect(&self, url: &str) -> Result<(), SlackError> {
        let (stream, _) = connect_async(url).await.map_err(|error| {
            SlackError::Internal(format!("Socket Mode connect failed: {error}"))
        })?;
        *self.stream.lock().await = Some(stream);
        Ok(())
    }

    async fn next_envelope(&self) -> Result<SocketModeEnvelope, SlackError> {
        loop {
            if let SocketModeFrame::Envelope(envelope) = self.next_frame().await? {
                return Ok(envelope);
            }
        }
    }

    async fn ack(&self, envelope_id: &str) -> Result<(), SlackError> {
        let mut guard = self.stream.lock().await;
        let stream = guard
            .as_mut()
            .ok_or_else(|| SlackError::Internal("Socket Mode client is not connected".into()))?;
        let payload = serde_json::json!({"envelope_id": envelope_id}).to_string();
        stream
            .send(Message::Text(payload.into()))
            .await
            .map_err(|error| SlackError::Internal(format!("Socket Mode ACK failed: {error}")))
    }

    async fn next_frame(&self) -> Result<SocketModeFrame, SlackError> {
        let mut guard = self.stream.lock().await;
        let stream = guard
            .as_mut()
            .ok_or_else(|| SlackError::Internal("Socket Mode client is not connected".into()))?;
        let message = stream
            .next()
            .await
            .ok_or_else(|| SlackError::Internal("Socket Mode stream ended".into()))?
            .map_err(|error| SlackError::Internal(format!("Socket Mode read failed: {error}")))?;
        socket_mode_frame_from_message(message)
    }

    async fn ping(&self) -> Result<(), SlackError> {
        let mut guard = self.stream.lock().await;
        let stream = guard
            .as_mut()
            .ok_or_else(|| SlackError::Internal("Socket Mode client is not connected".into()))?;
        stream
            .send(Message::Ping(Vec::new().into()))
            .await
            .map_err(|error| SlackError::Internal(format!("Socket Mode ping failed: {error}")))
    }

    async fn close(&self) -> Result<(), SlackError> {
        let mut guard = self.stream.lock().await;
        let Some(stream) = guard.as_mut() else {
            return Ok(());
        };
        let result = stream
            .close(None)
            .await
            .map_err(|error| SlackError::Internal(format!("Socket Mode close failed: {error}")));
        *guard = None;
        result
    }
}

fn socket_mode_frame_from_message(message: Message) -> Result<SocketModeFrame, SlackError> {
    match message {
        Message::Text(text) => serde_json::from_str(&text)
            .map(SocketModeFrame::Envelope)
            .map_err(SlackError::Serde),
        Message::Close(_) => {
            crabgent_log::info!("Slack Socket Mode stream closed by server");
            Err(SlackError::Internal(
                "Socket Mode stream closed by server".into(),
            ))
        }
        Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
            Ok(SocketModeFrame::Heartbeat)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_frame_maps_to_explicit_close_error() {
        let error =
            socket_mode_frame_from_message(Message::Close(None)).expect_err("close is an error");

        assert!(error.to_string().contains("closed by server"));
    }

    #[test]
    fn pong_frame_maps_to_heartbeat() {
        let frame =
            socket_mode_frame_from_message(Message::Pong(Vec::new().into())).expect("heartbeat");

        assert!(matches!(frame, SocketModeFrame::Heartbeat));
    }
}
