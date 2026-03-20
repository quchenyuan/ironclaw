//! DingTalk API type definitions.

use serde::{Deserialize, Serialize};

// ============================================================================
// Token API
// ============================================================================

/// Request body for token exchange.
#[derive(Debug, Serialize)]
pub struct TokenRequest {
    pub appkey: String,
    pub appsecret: String,
}

/// Response from token exchange.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[serde(rename = "expireIn")]
    pub expire_in: u64,
}

/// Error response from DingTalk API.
#[derive(Debug, Deserialize)]
pub struct DingTalkError {
    pub code: Option<String>,
    pub message: Option<String>,
}

// ============================================================================
// Stream API (WebSocket)
// ============================================================================

/// DingTalk Stream API message envelope.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamMessage {
    /// Connection identifier for this session.
    pub connection_id: Option<String>,
    /// Message type (e.g., CONNECT_ACK, EVENT, HEARTBEAT).
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Headers containing metadata.
    pub headers: Option<StreamHeaders>,
    /// Event data payload.
    pub data: Option<String>,
}

/// Headers in a Stream API message.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamHeaders {
    /// App key.
    pub app_key: Option<String>,
    /// Connection ID.
    pub connection_id: Option<String>,
    /// Event type.
    pub event_type: Option<String>,
    /// Event born time (timestamp).
    pub event_born_time: Option<String>,
    /// Event ID.
    pub event_id: Option<String>,
}

// ============================================================================
// Event Payloads
// ============================================================================

/// Message received event payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageReceivedEvent {
    /// Conversation type: 1 = private chat, 2 = group chat.
    pub conversation_type: Option<String>,
    /// Conversation ID.
    pub conversation_id: Option<String>,
    /// Chat ID (group ID for group chats).
    pub chat_id: Option<String>,
    /// Sender staff ID.
    pub sender_id: Option<String>,
    /// Sender union ID.
    pub sender_union_id: Option<String>,
    /// Sender staff ID (in group).
    pub sender_staff_id: Option<String>,
    /// Sender nickname.
    pub sender_nick: Option<String>,
    /// Is admin in group.
    pub is_admin: Option<bool>,
    /// Message content.
    pub text: Option<String>,
    /// Message type.
    pub msg_type: Option<String>,
    /// Message ID.
    pub msg_id: Option<String>,
    /// Create time (timestamp string).
    pub create_time: Option<String>,
    /// At users (for group mentions).
    pub at_users: Option<Vec<AtUser>>,
}

/// At user in a message.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtUser {
    pub dingtalk_id: Option<String>,
    pub staff_id: Option<String>,
}

// ============================================================================
// Send API
// ============================================================================

/// Request to send a message.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    pub robot_code: String,
    /// For private chat: user ID. For group chat: chat ID.
    pub open_conversation_id: Option<String>,
    /// Conversation type: 1 = private, 2 = group.
    #[serde(rename = "conversationType")]
    pub conversation_type: String,
    /// Recipients for private chat.
    pub user_ids: Option<Vec<String>>,
    /// Message type: text, markdown, image, file.
    pub msg_key: String,
    /// Message content as JSON string.
    pub msg_param: String,
}

/// Response from send API.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageResponse {
    pub process_query_key: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,
}

/// Text message content.
#[derive(Debug, Serialize)]
pub struct TextContent {
    pub content: String,
}

/// Markdown message content.
#[derive(Debug, Serialize)]
pub struct MarkdownContent {
    pub title: String,
    pub text: String,
}

/// Image message content.
#[derive(Debug, Serialize)]
pub struct ImageContent {
    pub photo_url: String,
}
