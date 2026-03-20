//! DingTalk message sender.

use reqwest::Client;

use crate::channels::dingtalk::token::TokenManager;
use crate::channels::dingtalk::types::{
    MarkdownContent, SendMessageRequest, SendMessageResponse, TextContent,
};

const SEND_API: &str = "https://api.dingtalk.com/v1.0/robot/sendToGroup";
const SEND_DM_API: &str = "https://api.dingtalk.com/v1.0/robot/sendToSingleChat";

/// DingTalk message sender.
pub struct MessageSender {
    client: Client,
    token_manager: TokenManager,
    robot_code: String,
}

impl MessageSender {
    pub fn new(client: Client, token_manager: TokenManager, robot_code: String) -> Self {
        Self {
            client,
            token_manager,
            robot_code,
        }
    }

    /// Check health by validating token access.
    pub async fn check_health(&self) -> Result<(), String> {
        self.token_manager.get_token().await?;
        Ok(())
    }

    pub async fn send_text(
        &self,
        conversation_type: &str,
        target_id: &str,
        text: &str,
    ) -> Result<(), String> {
        let content = serde_json::to_string(&TextContent {
            content: text.to_string(),
        })
        .map_err(|e| format!("Failed to serialize text: {e}"))?;

        self.send(conversation_type, target_id, "text", &content)
            .await
    }

    pub async fn send_markdown(
        &self,
        conversation_type: &str,
        target_id: &str,
        title: &str,
        text: &str,
    ) -> Result<(), String> {
        let content = serde_json::to_string(&MarkdownContent {
            title: title.to_string(),
            text: text.to_string(),
        })
        .map_err(|e| format!("Failed to serialize markdown: {e}"))?;

        self.send(conversation_type, target_id, "markdown", &content)
            .await
    }

    async fn send(
        &self,
        conversation_type: &str,
        target_id: &str,
        msg_key: &str,
        msg_param: &str,
    ) -> Result<(), String> {
        let token = self.token_manager.get_token().await?;

        let api_url = if conversation_type == "1" {
            SEND_DM_API
        } else {
            SEND_API
        };

        let req_body = SendMessageRequest {
            robot_code: self.robot_code.clone(),
            open_conversation_id: if conversation_type == "2" {
                Some(target_id.to_string())
            } else {
                None
            },
            conversation_type: conversation_type.to_string(),
            user_ids: if conversation_type == "1" {
                Some(vec![target_id.to_string()])
            } else {
                None
            },
            msg_key: msg_key.to_string(),
            msg_param: msg_param.to_string(),
        };

        let resp = self
            .client
            .post(api_url)
            .header("x-acs-dingtalk-access-token", &token)
            .json(&req_body)
            .send()
            .await
            .map_err(|e| format!("Send request failed: {e}"))?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(format!("Send API returned {status}: {body}"));
        }

        let send_resp: SendMessageResponse = serde_json::from_str(&body)
            .map_err(|e| format!("Failed to parse send response: {e}"))?;

        if let Some(ref code) = send_resp.code
            && code != "0"
            && code != "OK"
        {
            return Err(format!(
                "DingTalk send error: {} - {}",
                code,
                send_resp.message.as_deref().unwrap_or("unknown")
            ));
        }

        tracing::debug!(
            conversation_type = conversation_type,
            target_id = target_id,
            "DingTalk message sent"
        );

        Ok(())
    }
}
