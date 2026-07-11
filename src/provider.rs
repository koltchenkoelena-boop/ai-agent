use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{Stream, TryStreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;

use crate::types::*;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Chunk timeout exceeded ({0:?})")]
    ChunkTimeout(Duration),

    #[error("API Error (Status {status}): {body}")]
    ApiError { status: u16, body: String },

    #[error("Execution error: {0}")]
    Execution(String),
}

// ---------------------------------------------------------------------------
// Stream type alias
// ---------------------------------------------------------------------------

pub type ProviderStream = Pin<Box<dyn Stream<Item = Result<ChatChunk, ProviderError>> + Send>>;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn stream_chat(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<ProviderStream, ProviderError>;
}

// ---------------------------------------------------------------------------
// CredentialRotator — потокобезопасная round-robin ротация эндпоинтов/ключей
// ---------------------------------------------------------------------------

/// Thread-safe round-robin rotator for a pool of endpoints or API keys.
///
/// Each call to `get_next()` returns the next URL/credential in the list,
/// wrapping around modulo the pool length.
#[derive(Clone)]
pub struct CredentialRotator {
    endpoints: Vec<String>,
    counter: Arc<AtomicUsize>,
}

impl CredentialRotator {
    /// Create a new rotator from a non-empty list of endpoints.
    ///
    /// # Panics
    /// Panics if `endpoints` is empty.
    pub fn new(endpoints: Vec<String>) -> Self {
        assert!(
            !endpoints.is_empty(),
            "CredentialRotator requires at least one endpoint"
        );
        Self {
            endpoints,
            counter: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Return the next endpoint URL in round-robin order.
    ///
    /// Returns `None` only when the pool is empty (should not happen
    /// after construction via `new`).
    pub fn get_next(&self) -> Option<String> {
        if self.endpoints.is_empty() {
            return None;
        }
        let idx = self.counter.fetch_add(1, Ordering::Relaxed) % self.endpoints.len();
        Some(self.endpoints[idx].clone())
    }
}

// ---------------------------------------------------------------------------
// Ollama provider — OpenAI-compatible streaming via SSE (data: lines)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    chunk_timeout: Duration,
    rotator: Option<CredentialRotator>,
    api_key: Option<String>,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>, chunk_timeout: Duration) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            chunk_timeout,
            rotator: None,
            api_key: None,
        }
    }

    /// Shortcut pointing at default local Ollama (http://localhost:11434).
    pub fn local() -> Self {
        Self::new("http://localhost:11434", Duration::from_secs(10))
    }

    /// Attach a credential rotator for round-robin endpoint switching.
    pub fn with_rotator(mut self, rotator: CredentialRotator) -> Self {
        self.rotator = Some(rotator);
        self
    }

    /// Set an API key for Bearer token authentication.
    pub fn with_api_key(mut self, key: String) -> Self {
        self.api_key = Some(key);
        self
    }
}

// --- Wire types matching OpenAI chat completions API (streaming) -----------

#[derive(Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAITool>>,
    stream: bool,
}

#[derive(Serialize)]
struct OpenAITool {
    r#type: String,
    function: ToolDefinition,
}

#[derive(Deserialize, Debug)]
struct OpenAIStreamChunk {
    choices: Vec<OpenAIChoice>,
}

#[derive(Deserialize, Debug)]
struct OpenAIChoice {
    delta: OpenAIDelta,
}

#[derive(Deserialize, Debug)]
struct OpenAIDelta {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCallChunk>>,
}

// --- Implementation ---------------------------------------------------------

#[async_trait]
impl ModelProvider for OllamaProvider {
    async fn stream_chat(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<ProviderStream, ProviderError> {
        // Pick base URL from the rotator (round-robin) or fall back to the fixed one.
        let base_url = match &self.rotator {
            Some(r) => r.get_next().unwrap_or_else(|| self.base_url.clone()),
            None => self.base_url.clone(),
        };
        let url = format!("{}/v1/chat/completions", base_url);

        let payload = OpenAIRequest {
            model: model.to_string(),
            messages,
            tools: tools.map(|t| {
                t.into_iter()
                    .map(|td| OpenAITool {
                        r#type: "function".into(),
                        function: td,
                    })
                    .collect()
            }),
            stream: true,
        };

        // ---- POST with exponential back-off on 503 -------------------------
        let mut attempts = 0usize;
        let mut delay = Duration::from_secs(2);
        let response = loop {
            let mut http_req = self.client.post(&url).json(&payload);
            if let Some(ref key) = self.api_key {
                http_req = http_req.header("Authorization", format!("Bearer {key}"));
            }
            match http_req.send().await {
                Ok(r) if r.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE => {
                    attempts += 1;
                    if attempts > 5 {
                        return Err(ProviderError::ApiError {
                            status: 503,
                            body: "Ollama loading timeout after 5 retries".into(),
                        });
                    }
                    tokio::time::sleep(delay).await;
                    delay = delay.saturating_mul(2);
                }
                Ok(r) if !r.status().is_success() => {
                    return Err(ProviderError::ApiError {
                        status: r.status().as_u16(),
                        body: r.text().await.unwrap_or_default(),
                    });
                }
                Ok(r) => break r,
                Err(e) => return Err(ProviderError::Network(e)),
            }
        };

        // ---- Wire body bytes → lines → parsed chunks ------------------------
        // Convert the reqwest byte stream into an AsyncRead, then read line by line.
        let reader = StreamReader::new(
            response
                .bytes_stream()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
        );
        let mut lines = BufReader::new(reader).lines();
        let timeout = self.chunk_timeout;

        let stream = async_stream::try_stream! {
            loop {
                let maybe_line = tokio::time::timeout(timeout, lines.next_line())
                    .await
                    .map_err(|_| ProviderError::ChunkTimeout(timeout))?;

                match maybe_line {
                    Ok(None) => break,   // EOF
                    Err(e) => Err(ProviderError::Execution(e.to_string()))?,
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if trimmed == "data: [DONE]" {
                            break;
                        }
                        if let Some(data) = trimmed.strip_prefix("data: ") {
                            let parsed: OpenAIStreamChunk =
                                serde_json::from_str(data)?;
                            if let Some(choice) = parsed.choices.first() {
                                yield ChatChunk {
                                    delta_content: choice.delta.content.clone(),
                                    delta_tool_calls: choice.delta.tool_calls.clone(),
                                };
                            }
                        }
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}
