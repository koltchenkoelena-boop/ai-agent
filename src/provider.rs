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
// ProviderKind — формат запроса/ответа провайдера
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderKind {
    /// OpenAI-compatible /v1/chat/completions (SSE `data:` lines)
    #[serde(rename = "openai")]
    OpenAI,
    /// Ollama-native /api/chat (NDJSON — raw JSON lines)
    #[serde(rename = "ollama_chat")]
    OllamaChat,
}

impl Default for ProviderKind {
    fn default() -> Self {
        Self::OpenAI
    }
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

    /// Получить embedding вектора для заданного текста.
    ///
    /// Использует модель, указанную в `embedding_model` (например, `nomic-embed-text`).
    async fn get_embedding(&self, text: &str) -> Result<Vec<f32>, ProviderError>;
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
    /// Модель для эмбеддингов (например, `nomic-embed-text`).
    /// Можно переопределить через переменную окружения `AI_AGENT_EMBEDDING_MODEL`.
    embedding_model: String,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>, chunk_timeout: Duration) -> Self {
        let embedding_model = std::env::var("AI_AGENT_EMBEDDING_MODEL")
            .unwrap_or_else(|_| "nomic-embed-text".into());
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            chunk_timeout,
            rotator: None,
            api_key: None,
            embedding_model,
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

// ---------------------------------------------------------------------------
// ProviderConfig — конфигурация одного провайдера LLM
// ---------------------------------------------------------------------------

/// Конфигурация одного провайдера в пуле FallbackProvider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Человеческое имя (для логов)
    pub name: String,
    /// Базовый URL API (например, http://localhost:11434 или https://openrouter.ai/api/v1)
    pub base_url: String,
    /// Имя модели (будет подставлено в поле "model" JSON-запроса)
    pub model_name: String,
    /// Bearer API-ключ (если требуется)
    pub api_key: Option<String>,
    /// Поддерживает ли этот провайдер эмбеддинги (/api/embeddings)
    pub supports_embeddings: bool,
    /// Формат взаимодействия (OpenAI / OllamaChat)
    #[serde(default)]
    pub kind: ProviderKind,
}

/// Собрать пул провайдеров из переменных окружения.
///
/// Порядок приоритета:
/// 1. `AGENT_PROVIDER_POOL` — явный список эндпоинтов (каждый становится провайдером)
/// 2. `OPENROUTER_API_KEY` — OpenRouter (если ключ задан)
/// 3. Локальная Ollama (всегда, как финальный fallback)
pub fn build_provider_pool() -> Vec<ProviderConfig> {
    let mut providers: Vec<ProviderConfig> = Vec::new();

    let default_model = std::env::var("AI_AGENT_MODEL").unwrap_or_else(|_| "qwen2.5-coder:7b".into());
    let ollama_api_key = std::env::var("OLLAMA_API_KEY").ok();

    // 1. AGENT_PROVIDER_POOL — явный пул эндпоинтов
    if let Ok(val) = std::env::var("AGENT_PROVIDER_POOL") {
        let val = val.trim().to_string();
        if !val.is_empty() {
            let endpoints: Vec<&str> = val
                .split(',')
                .map(|s| s.trim().trim_matches(&['[', ']', '"', '\''][..]))
                .filter(|s| !s.is_empty())
                .collect();
            for (i, ep) in endpoints.iter().enumerate() {
                providers.push(ProviderConfig {
                    name: format!("pool-{i}"),
                    base_url: ep.to_string(),
                    model_name: default_model.clone(),
                    api_key: ollama_api_key.clone(),
                    supports_embeddings: true,
                    kind: ProviderKind::OpenAI,
                });
            }
        }
    }

    // 2. OpenRouter (если задан OPENROUTER_API_KEY)
    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            let or_model = std::env::var("OPENROUTER_MODEL")
                .unwrap_or_else(|_| "qwen/qwen-2.5-coder-32b-instruct:free".into());
            providers.push(ProviderConfig {
                name: "openrouter".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
                model_name: or_model,
                api_key: Some(key),
                supports_embeddings: false,
                kind: ProviderKind::OpenAI,
            });
        }
    }

    // 2.5. Ollama Cloud (когда задан OLLAMA_CLOUD_API_KEY)
    if let Ok(key) = std::env::var("OLLAMA_CLOUD_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            let cloud_model = std::env::var("AI_AGENT_MODEL")
                .unwrap_or_else(|_| "nemotron-3-super:cloud".into());
            let cloud_base = std::env::var("OLLAMA_CLOUD_BASE_URL")
                .unwrap_or_else(|_| "https://ollama.com".into());
            providers.push(ProviderConfig {
                name: "ollama-cloud".into(),
                base_url: cloud_base,
                model_name: cloud_model,
                api_key: Some(key),
                supports_embeddings: false,
                kind: ProviderKind::OllamaChat,
            });
        }
    }

    // 3. Локальная Ollama (всегда — финальный fallback)
    {
        let local_url = std::env::var("OLLAMA_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:11434".into());
        providers.push(ProviderConfig {
            name: "ollama-local".into(),
            base_url: local_url,
            model_name: default_model,
            api_key: ollama_api_key,
            supports_embeddings: true,
            kind: ProviderKind::OpenAI,
        });
    }

    providers
}

// ---------------------------------------------------------------------------
// SSE stream helper
// ---------------------------------------------------------------------------

/// Преобразовать HTTP-ответ (SSE-поток data: ...) в `ProviderStream`.
///
/// Используется внутри `FallbackProvider` (и может быть переиспользован
/// `OllamaProvider`).
fn response_to_sse_stream(
    response: reqwest::Response,
    chunk_timeout: Duration,
) -> ProviderStream {
    let reader = StreamReader::new(
        response
            .bytes_stream()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
    );
    let mut lines = BufReader::new(reader).lines();
    let timeout = chunk_timeout;

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

    Box::pin(stream)
}

/// Преобразовать HTTP-ответ (Ollama NDJSON — одна JSON-строка на чанк) в `ProviderStream`.
///
/// Ollama `/api/chat` возвращает raw NDJSON:
/// ```json
/// {"model":"...","created_at":"...","message":{"role":"assistant","content":"..."},"done":false}
/// {"model":"...","created_at":"...","message":{"role":"assistant","content":""},"done":true}
/// ```
fn response_to_ndjson_stream(
    response: reqwest::Response,
    chunk_timeout: Duration,
) -> ProviderStream {
    let reader = StreamReader::new(
        response
            .bytes_stream()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
    );
    let mut lines = BufReader::new(reader).lines();
    let timeout = chunk_timeout;

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
                    // Парсим Ollama NDJSON: {"message":{"content":"..."},"done":false}
                    let parsed: OllamaChatChunk = serde_json::from_str(trimmed)?;
                    if parsed.done {
                        break;
                    }
                    yield ChatChunk {
                        delta_content: Some(parsed.message.content),
                        delta_tool_calls: None,
                    };
                }
            }
        }
    };

    Box::pin(stream)
}

// ---------------------------------------------------------------------------
// FallbackProvider — отказоустойчивый прокси-провайдер
// ---------------------------------------------------------------------------

/// Multi-provider провайдер с автоматическим failover.
///
/// Реализует `ModelProvider`, владея пулом `ProviderConfig`.
/// При ошибке сети, таймауте или HTTP-статусе != 2xx переключается
/// на следующего провайдера в пуле (round-robin).
///
/// Контекст переключается прозрачно — `stream_chat` передаёт один
/// и тот же массив сообщений всем провайдерам; Agent не видит разницы.
#[derive(Clone)]
pub struct FallbackProvider {
    client: reqwest::Client,
    providers: Vec<ProviderConfig>,
    current_index: Arc<AtomicUsize>,
    chunk_timeout: Duration,
    /// Модель для эмбеддингов (Ollama-specific /api/embeddings).
    embedding_model: String,
}

impl FallbackProvider {
    pub fn new(providers: Vec<ProviderConfig>, chunk_timeout: Duration) -> Self {
        let embedding_model = std::env::var("AI_AGENT_EMBEDDING_MODEL")
            .unwrap_or_else(|_| "nomic-embed-text".into());
        Self {
            client: reqwest::Client::new(),
            providers,
            current_index: Arc::new(AtomicUsize::new(0)),
            chunk_timeout,
            embedding_model,
        }
    }

    /// Shortcut: создать FallbackProvider из переменных окружения.
    pub fn from_env() -> Self {
        Self::new(build_provider_pool(), Duration::from_secs(10))
    }

    /// Количество провайдеров в пуле.
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }
}

#[async_trait]
impl ModelProvider for FallbackProvider {
    async fn stream_chat(
        &self,
        _model: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<ProviderStream, ProviderError> {
        let n = self.providers.len();
        let mut last_error = ProviderError::Execution("All providers failed".into());

        for _ in 0..n {
            let idx = self.current_index.fetch_add(1, Ordering::Relaxed) % n;
            let cfg = &self.providers[idx];

            let base_url = cfg.base_url.trim_end_matches('/');

            // ---- URL path depends on provider kind ---------------------------
            let url = match cfg.kind {
                ProviderKind::OpenAI => format!("{base_url}/v1/chat/completions"),
                ProviderKind::OllamaChat => format!("{base_url}/api/chat"),
            };

            // ---- Build JSON payload ------------------------------------------
            let openai_tools = tools.as_ref().map(|t| {
                t.iter()
                    .map(|td| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": td.name,
                                "description": td.description,
                                "parameters": td.parameters,
                            }
                        })
                    })
                    .collect::<Vec<_>>()
            });

            let mut payload = serde_json::json!({
                "model": cfg.model_name,
                "messages": messages,
                "stream": true,
            });
            if let Some(ref ot) = openai_tools {
                payload["tools"] = serde_json::json!(ot);
            }

            // ---- POST --------------------------------------------------------
            let mut http_req = self.client.post(&url).json(&payload);
            if let Some(ref key) = cfg.api_key {
                http_req = http_req.header("Authorization", format!("Bearer {key}"));
            }

            let response = match http_req.send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "FallbackProvider: provider '{}' network error: {e} — trying next",
                        cfg.name,
                    );
                    last_error = ProviderError::Network(e);
                    continue;
                }
            };

            // ---- Проверка статуса --------------------------------------------
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();

                // 413 пробрасываем сразу (обрабатывается в Agent::run_step)
                if status == 413 {
                    return Err(ProviderError::ApiError { status, body });
                }

                tracing::warn!(
                    "FallbackProvider: provider '{}' returned HTTP {status}: {body} — trying next",
                    cfg.name,
                );
                last_error = ProviderError::ApiError { status, body };
                continue;
            }

            tracing::info!(
                "FallbackProvider: streaming from '{}' (model: {}, kind: {:?})",
                cfg.name,
                cfg.model_name,
                cfg.kind,
            );

            // ---- Response parsing depends on provider kind --------------------
            return match cfg.kind {
                ProviderKind::OpenAI => {
                    Ok(response_to_sse_stream(response, self.chunk_timeout))
                }
                ProviderKind::OllamaChat => {
                    Ok(response_to_ndjson_stream(response, self.chunk_timeout))
                }
            };
        }

        Err(last_error)
    }

    async fn get_embedding(&self, text: &str) -> Result<Vec<f32>, ProviderError> {
        let n = self.providers.len();
        let mut last_error =
            ProviderError::Execution("All providers failed for embedding".into());

        for _ in 0..n {
            let idx = self.current_index.fetch_add(1, Ordering::Relaxed) % n;
            let cfg = &self.providers[idx];

            // Пропускаем провайдеров без поддержки эмбеддингов
            if !cfg.supports_embeddings {
                continue;
            }

            let base_url = cfg.base_url.trim_end_matches('/');
            let url = format!("{base_url}/api/embeddings");

            let payload = OllamaEmbeddingRequest {
                model: self.embedding_model.clone(),
                prompt: text.to_string(),
            };

            let mut http_req = self.client.post(&url).json(&payload);
            if let Some(ref key) = cfg.api_key {
                http_req = http_req.header("Authorization", format!("Bearer {key}"));
            }

            let response = match http_req.send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "FallbackProvider: embedding from '{}' network error: {e} — trying next",
                        cfg.name,
                    );
                    last_error = ProviderError::Network(e);
                    continue;
                }
            };

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                tracing::warn!(
                    "FallbackProvider: embedding from '{}' returned HTTP {status}: {body} — trying next",
                    cfg.name,
                );
                last_error = ProviderError::ApiError { status, body };
                continue;
            }

            let parsed: OllamaEmbeddingResponse = match response.json().await {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        "FallbackProvider: embedding parse from '{}' failed: {e} — trying next",
                        cfg.name,
                    );
                    last_error = ProviderError::Network(e);
                    continue;
                }
            };

            tracing::info!(
                "FallbackProvider: embedding from '{}' (dim: {})",
                cfg.name,
                parsed.embedding.len(),
            );
            return Ok(parsed.embedding);
        }

        Err(last_error)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use httpmock::prelude::*;

    // -------------------------------------------------------------------
    // Mock-сервер для тестирования failover
    // -------------------------------------------------------------------

    /// Проверяем, что если первый провайдер возвращает 429, FallbackProvider
    /// бесшовно переключается на второго провайдера и возвращает успешный стрим.
    #[tokio::test]
    async fn test_fallback_on_http_error() {
        let server1 = MockServer::start();
        let server2 = MockServer::start();

        let providers = vec![
            ProviderConfig {
                name: "rate-limited".into(),
                base_url: server1.base_url(),
                model_name: "model-a".into(),
                api_key: None,
                supports_embeddings: false,
                kind: ProviderKind::OpenAI,
            },
            ProviderConfig {
                name: "working".into(),
                base_url: server2.base_url(),
                model_name: "model-b".into(),
                api_key: None,
                supports_embeddings: false,
                kind: ProviderKind::OpenAI,
            },
        ];

        let fb = FallbackProvider::new(providers, Duration::from_secs(5));

        // Первый провайдер — 429
        let _m1 = server1.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(429)
                .header("Content-Type", "application/json")
                .body(r#"{"error":"rate limited"}"#);
        });

        // Второй провайдер — успешный SSE-стрим
        let sse_body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\",\"tool_calls\":null}}]}\n\ndata: [DONE]\n\n";
        let _m2 = server2.mock(|when, then| {
            when.method(POST).path("/v1/chat/completions");
            then.status(200)
                .header("Content-Type", "text/event-stream")
                .body(sse_body);
        });

        let mut stream = fb
            .stream_chat("test", vec![Message::new(Role::User, "hi")], None)
            .await
            .unwrap();

        // Проверяем, что получили чанк со второго провайдера
        let chunk = stream.next().await.transpose().unwrap();
        assert!(chunk.is_some(), "Expected a chunk from the fallback provider");
        assert_eq!(
            chunk.unwrap().delta_content.as_deref(),
            Some("Hello"),
            "Content should come from the working provider"
        );

        // Проверяем, что оставшийся стрим пуст (DONE)
        let done = stream.next().await;
        assert!(done.is_none(), "Stream should be exhausted after [DONE]");
    }

    /// Проверяем, что тест build_provider_pool возвращает хотя бы local Ollama
    #[test]
    fn test_build_provider_pool_has_fallback() {
        let pool = build_provider_pool();
        assert!(!pool.is_empty(), "Pool must not be empty");
        assert!(
            pool.iter().any(|p| p.name == "ollama-local"),
            "Pool must include local Ollama fallback"
        );
    }
}

// --- Wire types matching Ollama /api/chat NDJSON response ------------------+

#[derive(Deserialize, Debug)]
struct OllamaChatChunk {
    message: OllamaChatMessage,
    done: bool,
}

#[derive(Deserialize, Debug)]
struct OllamaChatMessage {
    content: String,
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

// --- Wire types matching Ollama embedding API -----------------------------------+

#[derive(Serialize)]
struct OllamaEmbeddingRequest {
    model: String,
    prompt: String,
}

#[derive(Deserialize)]
struct OllamaEmbeddingResponse {
    embedding: Vec<f32>,
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

    async fn get_embedding(&self, text: &str) -> Result<Vec<f32>, ProviderError> {
        // Pick base URL from the rotator (round-robin) or fall back to the fixed one.
        let base_url = match &self.rotator {
            Some(r) => r.get_next().unwrap_or_else(|| self.base_url.clone()),
            None => self.base_url.clone(),
        };
        let url = format!("{}/api/embeddings", base_url);

        let payload = OllamaEmbeddingRequest {
            model: self.embedding_model.clone(),
            prompt: text.to_string(),
        };

        let mut http_req = self.client.post(&url).json(&payload);
        if let Some(ref key) = self.api_key {
            http_req = http_req.header("Authorization", format!("Bearer {key}"));
        }

        let response = http_req.send().await?;

        if !response.status().is_success() {
            return Err(ProviderError::ApiError {
                status: response.status().as_u16(),
                body: response.text().await.unwrap_or_default(),
            });
        }

        let parsed: OllamaEmbeddingResponse = response.json().await?;
        Ok(parsed.embedding)
    }
}
