//! DingTalk token management.

use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Client;
use tokio::sync::RwLock;

use crate::channels::dingtalk::types::{TokenRequest, TokenResponse};

const TOKEN_API: &str = "https://api.dingtalk.com/v1.0/oauth2/accessToken";

/// Cached access token with expiry.
#[derive(Clone)]
struct CachedToken {
    token: String,
    expires_at: Instant,
}

/// DingTalk token manager.
#[derive(Clone)]
pub struct TokenManager {
    client: Client,
    app_key: String,
    app_secret: String,
    cached: Arc<RwLock<Option<CachedToken>>>,
}

impl TokenManager {
    pub fn new(client: Client, app_key: String, app_secret: String) -> Self {
        Self {
            client,
            app_key,
            app_secret,
            cached: Arc::new(RwLock::new(None)),
        }
    }

    pub fn app_key(&self) -> &str {
        &self.app_key
    }

    pub async fn get_token(&self) -> Result<String, String> {
        {
            let cached = self.cached.read().await;
            if let Some(ref t) = *cached
                && t.expires_at > Instant::now() + Duration::from_secs(60)
            {
                return Ok(t.token.clone());
            }
        }
        self.refresh_token().await
    }

    async fn refresh_token(&self) -> Result<String, String> {
        let req_body = TokenRequest {
            appkey: self.app_key.clone(),
            appsecret: self.app_secret.clone(),
        };

        let resp = self
            .client
            .post(TOKEN_API)
            .json(&req_body)
            .send()
            .await
            .map_err(|e| format!("Token request failed: {e}"))?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(format!("Token API returned {status}: {body}"));
        }

        let token_resp: TokenResponse = serde_json::from_str(&body)
            .map_err(|e| format!("Failed to parse token response: {e} - body: {body}"))?;

        let cached = CachedToken {
            token: token_resp.access_token.clone(),
            expires_at: Instant::now() + Duration::from_secs(token_resp.expire_in),
        };

        let mut lock = self.cached.write().await;
        *lock = Some(cached);

        tracing::info!(
            "DingTalk access token refreshed, expires in {}s",
            token_resp.expire_in
        );

        Ok(token_resp.access_token)
    }
}
