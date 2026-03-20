//! DingTalk Stream API WebSocket client.
//!
//! Implements the DingTalk Stream protocol:
//! - https://open-dingtalk.github.io/developerpedia/docs/learn/stream/protocol

use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::channels::dingtalk::token::TokenManager;
use crate::channels::dingtalk::types::MessageReceivedEvent;

const CONNECTION_API: &str = "https://api.dingtalk.com/v1.0/gateway/connections/open";

#[derive(Debug, Clone)]
pub enum StreamEvent {
    MessageReceived(Box<MessageReceivedEvent>),
    Connected(String),
    Heartbeat,
}

/// DingTalk Stream envelope format.
#[derive(Debug, serde::Deserialize)]
struct StreamEnvelope {
    #[serde(rename = "specVersion")]
    #[serde(default)]
    _spec_version: Option<String>,
    #[serde(rename = "type")]
    msg_type: String,
    headers: StreamHeaders,
    data: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct StreamHeaders {
    topic: Option<String>,
    #[serde(rename = "messageId")]
    message_id: Option<String>,
    #[serde(rename = "contentType")]
    content_type: Option<String>,
    time: Option<String>,
    #[serde(rename = "eventType")]
    event_type: Option<String>,
}

/// ACK response format for DingTalk Stream.
#[derive(Debug, serde::Serialize)]
struct AckResponse {
    code: u32,
    message: String,
    headers: AckHeaders,
    data: String,
}

#[derive(Debug, serde::Serialize)]
struct AckHeaders {
    #[serde(rename = "messageId")]
    message_id: String,
    #[serde(rename = "contentType")]
    content_type: String,
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
        self.event_rx.lock().await.take()
    }

    pub async fn run(&self) {
        loop {
            match self.connect_and_listen().await {
                Ok(()) => tracing::info!("DingTalk Stream closed normally"),
                Err(e) => tracing::warn!("DingTalk Stream error: {e}, reconnecting in 5s"),
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    }

    async fn register_connection(&self) -> Result<(String, String), String> {
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "clientId": self.token_manager.app_key(),
            "clientSecret": self.token_manager.app_secret(),
            "ua": "ironclaw-rust/0.19.0",
            "subscriptions": [
                { "topic": "*", "type": "EVENT" },
                { "topic": "/v1.0/im/bot/messages/get", "type": "CALLBACK" }
            ]
        });
        let resp = client
            .post(CONNECTION_API)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Registration failed: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("Registration {status}: {text}"));
        }
        let data: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("Parse error: {e}"))?;
        let endpoint = data
            .get("endpoint")
            .and_then(|v| v.as_str())
            .ok_or("Missing endpoint")?
            .to_string();
        let ticket = data
            .get("ticket")
            .and_then(|v| v.as_str())
            .ok_or("Missing ticket")?
            .to_string();
        tracing::info!(endpoint = %endpoint, "DingTalk connection registered");
        Ok((endpoint, ticket))
    }

    async fn connect_and_listen(&self) -> Result<(), String> {
        let (endpoint, ticket) = self.register_connection().await?;
        let url = format!("{endpoint}?ticket={ticket}");
        tracing::info!(url = %url, "Connecting to DingTalk Stream");
        let (ws_stream, _) = connect_async(url.as_str())
            .await
            .map_err(|e| format!("WS error: {e:?}"))?;
        tracing::info!("DingTalk Stream connected");
        let _ = self
            .event_tx
            .send(StreamEvent::Connected("stream".to_string()))
            .await;
        let (mut write, mut read) = ws_stream.split();

        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    tracing::debug!(raw = %text, "DingTalk WS raw");
                    self.handle_frame(&text, &mut write).await?;
                }
                Ok(Message::Binary(data)) => {
                    if let Ok(text) = String::from_utf8(data.to_vec()) {
                        tracing::debug!(raw = %text, "DingTalk WS binary");
                        self.handle_frame(&text, &mut write).await?;
                    }
                }
                Ok(Message::Close(_)) => {
                    tracing::info!("DingTalk Stream closed");
                    break;
                }
                Ok(Message::Ping(data)) => {
                    let _ = write.send(Message::Pong(data)).await;
                }
                Ok(_) => {}
                Err(e) => {
                    return Err(format!("WebSocket error: {e}"));
                }
            }
        }
        Ok(())
    }

    async fn handle_frame(
        &self,
        text: &str,
        write: &mut futures::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    ) -> Result<(), String> {
        let envelope: StreamEnvelope =
            serde_json::from_str(text).map_err(|e| format!("Parse error: {e}"))?;
        let message_id = envelope.headers.message_id.clone().unwrap_or_default();

        // Send ACK response
        if !message_id.is_empty() {
            let ack = AckResponse {
                code: 200,
                message: "OK".to_string(),
                headers: AckHeaders {
                    message_id: message_id.clone(),
                    content_type: "application/json".to_string(),
                },
                data: "{\"response\": null}".to_string(),
            };
            if let Ok(ack_text) = serde_json::to_string(&ack) {
                let _ = write.send(Message::Text(ack_text.into())).await;
            }
        }

        match envelope.msg_type.as_str() {
            "SYSTEM" => {
                let topic = envelope.headers.topic.as_deref().unwrap_or("");
                if topic == "ping" {
                    tracing::trace!("DingTalk ping");
                    let _ = self.event_tx.send(StreamEvent::Heartbeat).await;
                } else if topic == "disconnect" {
                    tracing::info!("DingTalk server requesting disconnect");
                }
            }
            "CALLBACK" => {
                let topic = envelope.headers.topic.as_deref().unwrap_or("");
                if topic == "/v1.0/im/bot/messages/get" {
                    if let Some(ref data_str) = envelope.data {
                        match serde_json::from_str::<serde_json::Value>(data_str) {
                            Ok(msg_data) => {
                                let event = MessageReceivedEvent {
                                    conversation_type: msg_data
                                        .get("conversationType")
                                        .and_then(|v| v.as_str())
                                        .map(String::from),
                                    conversation_id: msg_data
                                        .get("conversationId")
                                        .and_then(|v| v.as_str())
                                        .map(String::from),
                                    chat_id: None,
                                    sender_id: msg_data
                                        .get("senderId")
                                        .and_then(|v| v.as_str())
                                        .map(String::from),
                                    sender_union_id: None,
                                    sender_staff_id: msg_data
                                        .get("senderStaffId")
                                        .and_then(|v| v.as_str())
                                        .map(String::from),
                                    sender_nick: msg_data
                                        .get("senderNick")
                                        .and_then(|v| v.as_str())
                                        .map(String::from),
                                    is_admin: msg_data.get("isAdmin").and_then(|v| v.as_bool()),
                                    text: msg_data
                                        .get("text")
                                        .and_then(|t| t.get("content"))
                                        .and_then(|v| v.as_str())
                                        .map(String::from),
                                    msg_type: msg_data
                                        .get("msgtype")
                                        .and_then(|v| v.as_str())
                                        .map(String::from),
                                    msg_id: msg_data
                                        .get("msgId")
                                        .and_then(|v| v.as_str())
                                        .map(String::from),
                                    create_time: msg_data
                                        .get("createAt")
                                        .and_then(|v| v.as_i64())
                                        .map(|v| v.to_string()),
                                    at_users: None,
                                };
                                tracing::debug!(sender = ?event.sender_nick, content = ?event.text, "DingTalk msg received");
                                let _ = self
                                    .event_tx
                                    .send(StreamEvent::MessageReceived(Box::new(event)))
                                    .await;
                            }
                            Err(e) => tracing::warn!("Parse callback data failed: {e}"),
                        }
                    }
                }
            }
            "EVENT" => {
                tracing::debug!(topic = ?envelope.headers.topic, "DingTalk event received");
            }
            _ => {
                tracing::debug!(msg_type = %envelope.msg_type, "DingTalk unknown type");
            }
        }
        Ok(())
    }
}
