use anyhow::Result;
use axum::{extract::Query, response::Html, routing::get, Router};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use std::{collections::HashMap, fs, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::sync::{oneshot, Mutex as TokioMutex};
use url::Url;

#[derive(Debug, Clone)]
struct OidcEndpoints {
    authorization_endpoint: String,
    token_endpoint: String,
}

#[derive(Serialize, Deserialize)]
struct TokenData {
    /// The access token used to authenticate API requests
    access_token: String,

    /// Optional refresh token that can be used to obtain a new access token
    /// when the current one expires, enabling offline access without user interaction
    refresh_token: Option<String>,

    /// When the access token expires (if known)
    /// Used to determine when a token needs to be refreshed
    expires_at: Option<DateTime<Utc>>,
}

struct TokenCache {
    cache_path: PathBuf,
}

fn get_base_path() -> PathBuf {
    // Use a similar pattern to Goose but for g3
    // macOS/Linux: ~/.config/g3/databricks/oauth
    // Windows: ~\AppData\Roaming\g3\config\databricks\oauth\
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("g3");
    path.push("databricks");
    path.push("oauth");
    path
}

impl TokenCache {
    fn new(host: &str, client_id: &str, scopes: &[String]) -> Self {
        let mut hasher = sha2::Sha256::new();
        hasher.update(host.as_bytes());
        hasher.update(client_id.as_bytes());
        hasher.update(scopes.join(",").as_bytes());
        let hash = format!("{:x}", hasher.finalize());

        fs::create_dir_all(get_base_path()).unwrap_or(());
        let cache_path = get_base_path().join(format!("{}.json", hash));

        Self { cache_path }
    }

    fn load_token(&self) -> Option<TokenData> {
        if let Ok(contents) = fs::read_to_string(&self.cache_path) {
            if let Ok(token_data) = serde_json::from_str::<TokenData>(&contents) {
                // Only return tokens that have a refresh token
                if token_data.refresh_token.is_some() {
                    // If token is not expired, return it for immediate use
                    if let Some(expires_at) = token_data.expires_at {
                        if expires_at > Utc::now() {
                            return Some(token_data);
                        }
                        // If token is expired but has refresh token, return it so we can refresh
                        return Some(token_data);
                    }
                    // No expiration time but has refresh token, return it
                    return Some(token_data);
                }
                // Token doesn't have a refresh token, ignore it to force a new OAuth flow
            }
        }
        None
    }

    fn save_token(&self, token_data: &TokenData) -> Result<()> {
        if let Some(parent) = self.cache_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string(token_data)?;
        fs::write(&self.cache_path, contents)?;
        Ok(())
    }
}

async fn get_workspace_endpoints(host: &str) -> Result<OidcEndpoints> {
    let base_url = Url::parse(host).expect("Invalid host URL");
    let oidc_url = base_url
        .join("oidc/.well-known/oauth-authorization-server")
        .expect("Invalid OIDC URL");

    let client = reqwest::Client::new();
    let resp = client.get(oidc_url.clone()).send().await?;

    if !resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to get OIDC configuration from {}",
            oidc_url
        ));
    }

    let oidc_config: Value = resp.json().await?;

    let authorization_endpoint = oidc_config
        .get("authorization_endpoint")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("authorization_endpoint not found in OIDC configuration"))?
        .to_string();

    let token_endpoint = oidc_config
        .get("token_endpoint")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("token_endpoint not found in OIDC configuration"))?
        .to_string();

    Ok(OidcEndpoints {
        authorization_endpoint,
        token_endpoint,
    })
}

struct OAuthFlow {
    endpoints: OidcEndpoints,
    client_id: String,
    redirect_url: String,
    scopes: Vec<String>,
    state: String,
    verifier: String,
}

impl OAuthFlow {
    fn new(
        endpoints: OidcEndpoints,
        client_id: String,
        redirect_url: String,
        scopes: Vec<String>,
    ) -> Self {
        Self {
            endpoints,
            client_id,
            redirect_url,
            scopes,
            state: nanoid::nanoid!(16),
            verifier: nanoid::nanoid!(64),
        }
    }

    /// Extracts token data from an OAuth 2.0 token response.
    fn extract_token_data(
        &self,
        token_response: &Value,
        old_refresh_token: Option<&str>,
    ) -> Result<TokenData> {
        // Extract access token (required)
        let access_token = token_response
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("access_token not found in token response"))?
            .to_string();

        // Extract refresh token if available
        let refresh_token = token_response
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| old_refresh_token.map(|s| s.to_string()));

        // Handle token expiration
        let expires_at =
            if let Some(expires_in) = token_response.get("expires_in").and_then(|v| v.as_u64()) {
                // Traditional OAuth flow with expires_in seconds
                Some(Utc::now() + chrono::Duration::seconds(expires_in as i64))
            } else {
                // If the server doesn't provide any expiration info, log it but don't set an expiration
                tracing::debug!(
                    "No expiration information provided by server, token expiration unknown."
                );
                None
            };

        Ok(TokenData {
            access_token,
            refresh_token,
            expires_at,
        })
    }

    fn get_authorization_url(&self) -> String {
        let challenge = {
            let digest = sha2::Sha256::digest(self.verifier.as_bytes());
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
        };

        let params = [
            ("response_type", "code"),
            ("client_id", &self.client_id),
            ("redirect_uri", &self.redirect_url),
            ("scope", &self.scopes.join(" ")),
            ("state", &self.state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ];

        format!(
            "{}?{}",
            self.endpoints.authorization_endpoint,
            serde_urlencoded::to_string(params).unwrap()
        )
    }

    async fn exchange_code_for_token(&self, code: &str) -> Result<TokenData> {
        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &self.redirect_url),
            ("code_verifier", &self.verifier),
            ("client_id", &self.client_id),
        ];

        let client = reqwest::Client::new();
        let resp = client
            .post(&self.endpoints.token_endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&params)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err_text = resp.text().await?;
            return Err(anyhow::anyhow!(
                "Failed to exchange code for token: {}",
                err_text
            ));
        }

        let token_response: Value = resp.json().await?;
        self.extract_token_data(&token_response, None)
    }

    async fn refresh_token(&self, refresh_token: &str) -> Result<TokenData> {
        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &self.client_id),
        ];

        tracing::debug!("Refreshing token using refresh_token");

        let client = reqwest::Client::new();
        let resp = client
            .post(&self.endpoints.token_endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&params)
            .send()
            .await?;

        if !resp.status().is_success() {
            let err_text = resp.text().await?;
            return Err(anyhow::anyhow!("Failed to refresh token: {}", err_text));
        }

        let token_response: Value = resp.json().await?;
        self.extract_token_data(&token_response, Some(refresh_token))
    }

    async fn execute(&self) -> Result<TokenData> {
        // Create a channel that will send the auth code from the app process
        let (tx, rx) = oneshot::channel();
        let state = self.state.clone();
        let tx = Arc::new(TokioMutex::new(Some(tx)));

        // Setup a server that will receive the redirect, capture the code, and display success/failure
        let app = Router::new().route(
            "/",
            get(move |Query(params): Query<HashMap<String, String>>| {
                let tx = Arc::clone(&tx);
                let state = state.clone();
                async move {
                    let code = params.get("code").cloned();
                    let received_state = params.get("state").cloned();

                    if let (Some(code), Some(received_state)) = (code, received_state) {
                        if received_state == state {
                            if let Some(sender) = tx.lock().await.take() {
                                if sender.send(code).is_ok() {
                                    return Html(
                                        "<h2>G3 Authentication Success</h2><p>You can close this window and return to your terminal.</p>",
                                    );
                                }
                            }
                            Html("<h2>Error</h2><p>Authentication already completed.</p>")
                        } else {
                            Html("<h2>Error</h2><p>State mismatch.</p>")
                        }
                    } else {
                        Html("<h2>Error</h2><p>Authentication failed.</p>")
                    }
                }
            }),
        );

        // Start the server to accept the oauth code
        let redirect_url = Url::parse(&self.redirect_url)?;
        let port = redirect_url.port().unwrap_or(80);
        let addr = SocketAddr::from(([127, 0, 0, 1], port));

        let listener = tokio::net::TcpListener::bind(addr).await?;

        let server_handle = tokio::spawn(async move {
            let server = axum::serve(listener, app);
            server.await.unwrap();
        });

        // Open the browser which will redirect with the code to the server
        let authorization_url = self.get_authorization_url();
        if std::env::var("G3_RETRO_MODE").is_err() {
            println!("🔐 Opening browser for Databricks authentication...");
        }
        if webbrowser::open(&authorization_url).is_err() {
            println!(
                "Please open this URL in your browser:\n{}",
                authorization_url
            );
        }

        // Wait for the authorization code with a timeout
        let code = tokio::time::timeout(
            std::time::Duration::from_secs(120), // 2 minute timeout
            rx,
        )
        .await
        .map_err(|_| anyhow::anyhow!("Authentication timed out after 2 minutes"))??;

        // Stop the server
        server_handle.abort();

        if std::env::var("G3_RETRO_MODE").is_err() {
            println!("✅ Authentication successful! Exchanging code for token...");
        }

        // Exchange the code for a token
        self.exchange_code_for_token(&code).await
    }
}

pub async fn get_oauth_token_async(
    host: &str,
    client_id: &str,
    redirect_url: &str,
    scopes: &[String],
) -> Result<String> {
    let token_cache = TokenCache::new(host, client_id, scopes);

    // Try cache first
    if let Some(token) = token_cache.load_token() {
        // If token has an expiration time, check if it's expired
        if let Some(expires_at) = token.expires_at {
            if expires_at > Utc::now() {
                tracing::debug!("Using cached token");
                return Ok(token.access_token);
            }
            // Token is expired, will try to refresh below
            tracing::debug!("Token is expired, attempting to refresh");
        } else {
            // No expiration time was provided by the server
            tracing::debug!("Token has no expiration time, using cached token");
            return Ok(token.access_token);
        }

        // Token is expired or has no expiration, try to refresh if we have a refresh token
        if let Some(refresh_token) = token.refresh_token {
            // Get endpoints for token refresh
            match get_workspace_endpoints(host).await {
                Ok(endpoints) => {
                    let flow = OAuthFlow::new(
                        endpoints,
                        client_id.to_string(),
                        redirect_url.to_string(),
                        scopes.to_vec(),
                    );

                    // Try to refresh the token
                    match flow.refresh_token(&refresh_token).await {
                        Ok(new_token) => {
                            if let Err(e) = token_cache.save_token(&new_token) {
                                tracing::warn!("Failed to save refreshed token: {}", e);
                            }
                            tracing::info!("Successfully refreshed token");
                            return Ok(new_token.access_token);
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to refresh token, will try new auth flow: {}",
                                e
                            );
                            // Continue to new auth flow
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to get endpoints for token refresh: {}", e);
                    // Continue to new auth flow
                }
            }
        }
    }

    // Get endpoints and execute flow for a new token
    let endpoints = get_workspace_endpoints(host).await?;
    let flow = OAuthFlow::new(
        endpoints,
        client_id.to_string(),
        redirect_url.to_string(),
        scopes.to_vec(),
    );

    // Execute the OAuth flow and get token
    let token = flow.execute().await?;

    // Cache and return
    token_cache.save_token(&token)?;
    if std::env::var("G3_RETRO_MODE").is_err() {
        println!("🎉 Databricks authentication complete!");
    }
    Ok(token.access_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_cache() -> Result<()> {
        let cache = TokenCache::new(
            "https://example.com",
            "test-client",
            &["scope1".to_string()],
        );

        // Test with expiration time
        let token_data = TokenData {
            access_token: "test-token".to_string(),
            refresh_token: Some("test-refresh-token".to_string()),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
        };

        cache.save_token(&token_data)?;

        let loaded_token = cache.load_token().unwrap();
        assert_eq!(loaded_token.access_token, token_data.access_token);
        assert_eq!(loaded_token.refresh_token, token_data.refresh_token);
        assert!(loaded_token.expires_at.is_some());

        Ok(())
    }
}
