//! DingTalk channel for IronClaw.

pub mod client;
pub mod sender;
pub mod token;
pub mod types;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

use crate::channels::channel::{Channel, IncomingMessage, MessageStream, OutgoingResponse};
use crate::channels::dingtalk::client::{StreamClient, StreamEvent};
use crate::channels::dingtalk::sender::MessageSender;
use crate::channels::dingtalk::token::TokenManager;
use crate::error::ChannelError;

#[derive(Debug, Clone)]
pub struct DingTalkConfig {
    pub app_key: String,
    pub app_secret: String,
    pub robot_code: String,
    pub owner_id: String,
    pub dm_policy: String,
    pub group_policy: String,
    pub allow_from: Vec<String>,
    pub group_allow_from: Vec<String>,
    pub message_type: String,
}

impl Default for DingTalkConfig {
    fn default() -> Self {
        Self {
            app_key: String::new(),
            app_secret: String::new(),
            robot_code: String::new(),
            owner_id: String::new(),
            dm_policy: "open".to_string(),
            group_policy: "open".to_string(),
            allow_from: Vec::new(),
            group_allow_from: Vec::new(),
            message_type: "text".to_string(),
        }
    }
}

pub struct DingTalkChannel {
    config: DingTalkConfig,
    sender: Arc<MessageSender>,
    client: Arc<StreamClient>,
}

impl DingTalkChannel {
    pub async fn new(config: DingTalkConfig) -> Result<Self, ChannelError> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ChannelError::StartupFailed {
                name: "dingtalk".to_string(),
                reason: format!("Failed to build HTTP client: {e}"),
            })?;

        let token_manager = TokenManager::new(
            http_client.clone(),
            config.app_key.clone(),
            config.app_secret.clone(),
        );

        let sender = Arc::new(MessageSender::new(
            http_client.clone(),
            token_manager.clone(),
            config.robot_code.clone(),
        ));

        let stream_client = Arc::new(StreamClient::new(token_manager));

        Ok(Self {
            config,
            sender,
            client: stream_client,
        })
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn name(&self) -> &str {
        "dingtalk"
    }

    async fn start(&self) -> Result<MessageStream, ChannelError> {
        let (tx, rx) = mpsc::channel::<IncomingMessage>(256);
        let client = self.client.clone();
        let config = self.config.clone();

        tokio::spawn(async move {
            let client_ref = client.clone();
            tokio::spawn(async move {
                client_ref.run().await;
            });

            let mut event_rx = match client.take_receiver().await {
                Some(rx) => rx,
                None => {
                    tracing::error!("DingTalk StreamClient receiver already taken");
                    return;
                }
            };

            while let Some(event) = event_rx.recv().await {
                match event {
                    StreamEvent::Connected(connection_id) => {
                        tracing::info!(connection_id = %connection_id, "DingTalk channel connected");
                    }
                    StreamEvent::Heartbeat => {
                        tracing::trace!("DingTalk heartbeat");
                    }
                    StreamEvent::MessageReceived(msg_event) => {
                        let msg_event = *msg_event;
                        let conversation_type = msg_event
                            .conversation_type
                            .clone()
                            .unwrap_or_else(|| "1".to_string());

                        let policy_ok = match conversation_type.as_str() {
                            "1" => {
                                config.dm_policy == "open"
                                    || (config.dm_policy == "allowlist"
                                        && msg_event
                                            .sender_id
                                            .as_ref()
                                            .is_some_and(|id| config.allow_from.contains(id)))
                            }
                            "2" => {
                                config.group_policy == "open"
                                    || (config.group_policy == "allowlist"
                                        && msg_event
                                            .sender_id
                                            .as_ref()
                                            .is_some_and(|id| config.group_allow_from.contains(id)))
                            }
                            _ => config.dm_policy == "open",
                        };

                        if !policy_ok {
                            tracing::debug!(sender = ?msg_event.sender_id, "DingTalk message rejected by policy");
                            continue;
                        }

                        let metadata = serde_json::json!({
                            "conversation_type": conversation_type,
                            "target_id": if conversation_type == "2" {
                                msg_event.chat_id.clone().unwrap_or_default()
                            } else {
                                msg_event.sender_id.clone().unwrap_or_default()
                            },
                            "chat_id": msg_event.chat_id,
                            "msg_id": msg_event.msg_id,
                        });

                        let sender_id = msg_event.sender_id.clone().unwrap_or_default();
                        let content = msg_event.text.clone().unwrap_or_default();

                        let incoming = IncomingMessage {
                            id: Uuid::new_v4(),
                            channel: "dingtalk".to_string(),
                            user_id: sender_id.clone(),
                            owner_id: config.owner_id.clone(),
                            sender_id: sender_id.clone(),
                            user_name: msg_event.sender_nick.clone(),
                            content,
                            thread_id: None,
                            conversation_scope_id: None,
                            received_at: chrono::Utc::now(),
                            metadata,
                            timezone: None,
                            attachments: Vec::new(),
                            is_internal: false,
                        };

                        if tx.send(incoming).await.is_err() {
                            tracing::warn!("DingTalk channel receiver dropped");
                            break;
                        }
                    }
                }
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    async fn respond(
        &self,
        msg: &IncomingMessage,
        response: OutgoingResponse,
    ) -> Result<(), ChannelError> {
        let metadata = &msg.metadata;
        let conversation_type = metadata
            .get("conversation_type")
            .and_then(|v| v.as_str())
            .unwrap_or("1");
        let target_id = metadata
            .get("target_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if target_id.is_empty() {
            return Err(ChannelError::SendFailed {
                name: "dingtalk".to_string(),
                reason: "No target_id in metadata".to_string(),
            });
        }

        match self.config.message_type.as_str() {
            "markdown" => self
                .sender
                .send_markdown(conversation_type, target_id, "IronClaw", &response.content)
                .await
                .map_err(|e| ChannelError::SendFailed {
                    name: "dingtalk".to_string(),
                    reason: e,
                }),
            _ => self
                .sender
                .send_text(conversation_type, target_id, &response.content)
                .await
                .map_err(|e| ChannelError::SendFailed {
                    name: "dingtalk".to_string(),
                    reason: e,
                }),
        }
    }

    async fn health_check(&self) -> Result<(), ChannelError> {
        self.sender
            .check_health()
            .await
            .map_err(|e| ChannelError::HealthCheckFailed {
                name: format!("dingtalk: {e}"),
            })
    }

    fn conversation_context(&self, metadata: &serde_json::Value) -> HashMap<String, String> {
        let mut ctx = HashMap::new();
        if let Some(conv_type) = metadata.get("conversation_type").and_then(|v| v.as_str()) {
            ctx.insert(
                "channel_type".to_string(),
                if conv_type == "2" { "group" } else { "dm" }.to_string(),
            );
        }
        if let Some(chat_id) = metadata.get("chat_id").and_then(|v| v.as_str()) {
            ctx.insert("chat_id".to_string(), chat_id.to_string());
        }
        ctx
    }

    async fn shutdown(&self) -> Result<(), ChannelError> {
        tracing::info!("DingTalk channel shutting down");
        Ok(())
    }
}
