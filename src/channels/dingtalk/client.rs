//! DingTalk Stream API WebSocket client.

use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::channels::dingtalk::token::TokenManager;
use crate::channels::dingtalk::types::{MessageReceivedEvent, StreamMessage};

const STREAM_WS_URL: &str = "wss://api.dingtalk.com/v1.0/agent/ws";

#[derive(Debug, Clone)]
pub enum StreamEvent {
    MessageReceived(Box<MessageReceivedEvent>),
    Connected(String),
    Heartbeat,
}

pub struct StreamClient {
    token_manager: TokenManager,
    event_tx: mpsc::Sender<StreamEvent>,
    event_rx: tokio::sync::Mutex<Option<mpsc::Receiver<StreamEvent>>>,
}

impl StreamClient {
    pub fn new(token_manager: TokenManager) -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self {
            token_manager,
            event_tx: tx,
            event_rx: tokio::sync::Mutex::new(Some(rx)),
        }
    }

    pub async fn take_receiver(&self) -> Option<mpsc::Receiver<StreamEvent>> {
        let mut lock = self.event_rx.lock().await;
        lock.take()
    }

    pub async fn run(&self) {
        loop {
            match self.connect_and_listen().await {
                Ok(()) => {
                    tracing::info!("DingTalk Stream connection closed normally");
                }
                Err(e) => {
                    tracing::warn!("DingTalk Stream error: {e}, reconnecting in 5s");
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    }

    async fn connect_and_listen(&self) -> Result<(), String> {
        let token = self.token_manager.get_token().await?;
        let url = format!("{STREAM_WS_URL}?accessToken={token}");

        tracing::info!("Connecting to DingTalk Stream API");

        let (ws_stream, _) = connect_async(&url)
            .await
            .map_err(|e| format!("WebSocket connect failed: {e}"))?;

        tracing::info!("DingTalk Stream connected");

        let (mut write, mut read) = ws_stream.split();

        let connect_frame = serde_json::json!({
            "type": "CONNECT",
            "headers": {
                "appKey": self.token_manager.app_key(),
            }
        });
        write
            .send(Message::Text(connect_frame.to_string().into()))
            .await
            .map_err(|e| format!("Failed to send CONNECT: {e}"))?;

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    self.handle_text(&text, &mut write).await?;
                }
                Ok(Message::Close(_)) => {
                    tracing::info!("DingTalk Stream closed by server");
                    break;
                }
                Err(e) => {
                    return Err(format!("WebSocket error: {e}"));
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn handle_text(
        &self,
        text: &str,
        write: &mut futures::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    ) -> Result<(), String> {
        let stream_msg: StreamMessage = serde_json::from_str(text)
            .map_err(|e| format!("Failed to parse stream message: {e}"))?;

        match stream_msg.msg_type.as_str() {
            "CONNECT_ACK" => {
                let connection_id = stream_msg.connection_id.clone().unwrap_or_default();
                tracing::info!(connection_id = %connection_id, "DingTalk Stream CONNECT_ACK");
                let _ = self
                    .event_tx
                    .send(StreamEvent::Connected(connection_id))
                    .await;
            }
            "HEARTBEAT" => {
                let ack = serde_json::json!({ "type": "HEARTBEAT_ACK" });
                let _ = write.send(Message::Text(ack.to_string().into())).await;
                let _ = self.event_tx.send(StreamEvent::Heartbeat).await;
            }
            "EVENT" => {
                if let Some(ref data) = stream_msg.data
                    && let Some(ref headers) = stream_msg.headers
                {
                    let event_type = headers.event_type.as_deref().unwrap_or("");
                    if event_type == "MessageReceive" {
                        match serde_json::from_str::<MessageReceivedEvent>(data) {
                            Ok(event) => {
                                tracing::debug!(sender = ?event.sender_nick, "DingTalk message received");
                                let _ = self
                                    .event_tx
                                    .send(StreamEvent::MessageReceived(Box::new(event)))
                                    .await;
                            }
                            Err(e) => {
                                tracing::warn!("Failed to parse MessageReceive event: {e}");
                            }
                        }
                    }
                }
            }
            _ => {
                tracing::debug!(msg_type = %stream_msg.msg_type, "DingTalk Stream unknown message type");
            }
        }

        Ok(())
    }
}
