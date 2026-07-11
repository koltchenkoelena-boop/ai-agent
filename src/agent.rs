// ---------------------------------------------------------------------------
// Agent Loop — оркестратор: LLM-провайдер → Safety → Tool Router → контекст
// ---------------------------------------------------------------------------

use std::collections::HashMap;

use async_trait::async_trait;
use futures_util::StreamExt;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::AsyncBufReadExt;
use tokio::sync::{broadcast, mpsc};

use crate::context::ContextManager;
use crate::hooks::{PostToolHook, PreToolHook};
use crate::memory::vector_db::VectorMemoryStore;
use crate::provider::{ModelProvider, ProviderError};
use crate::safety::{default_pipeline, SafetyDecision, SafetyPipeline};
use crate::tool_routing::frontend::{ClientCommand, FrontendEvent};
use crate::tool_routing::platform::register_platform_tools;
use crate::tool_routing::{AsyncTool, ToolKind, ToolRouter};
use crate::types::*;

// ===========================================================================
// AgentError
// ===========================================================================

/// Ошибки, возникающие в цикле агента.
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("Provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("Safety violation: {0}")]
    SafetyViolation(String),

    #[error("Tool execution failed: {0}")]
    ToolExecution(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("User aborted execution")]
    UserAbort,
}

// ===========================================================================
// StreamAccumulator
// ===========================================================================

/// Промежуточное состояние одного накапливаемого tool_call из чанков.
#[derive(Debug, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// Аккумулирует чанки стрима в готовое `Message`.
///
/// Чанки могут содержать как текстовый контент (delta_content),
/// так и фрагменты tool_calls (delta_tool_calls с index).
#[derive(Debug)]
pub struct StreamAccumulator {
    pub content: String,
    partial_calls: HashMap<usize, PartialToolCall>,
}

impl StreamAccumulator {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            partial_calls: HashMap::new(),
        }
    }

    /// Добавить очередной чанк.
    pub fn push(&mut self, chunk: &ChatChunk) {
        // Текст
        if let Some(ref text) = chunk.delta_content {
            self.content.push_str(text);
        }

        // Tool-call фрагменты
        if let Some(ref calls) = chunk.delta_tool_calls {
            for tc in calls {
                let idx = tc.index.unwrap_or(0);
                let entry = self.partial_calls.entry(idx).or_default();

                if let Some(ref id) = tc.id {
                    entry.id = Some(id.to_string());
                }
                if let Some(ref name) = tc.function.as_ref().and_then(|f| f.name.as_ref()) {
                    entry.name = Some(name.to_string());
                }
                if let Some(ref args) = tc.function.as_ref().and_then(|f| f.arguments.as_ref()) {
                    entry.arguments.push_str(args);
                }
            }
        }
    }

    /// Собрать накопленное в `Message` для роли `Assistant`.
    pub fn into_message(self) -> Message {
        let content_opt = if self.content.is_empty() {
            None
        } else {
            Some(self.content)
        };

        let tool_calls = if self.partial_calls.is_empty() {
            None
        } else {
            let mut calls: Vec<_> = self.partial_calls.into_iter().collect();
            calls.sort_by_key(|(idx, _)| *idx);
            Some(
                calls
                    .into_iter()
                    .map(|(_, partial)| ToolCall {
                        id: partial.id.unwrap_or_else(|| "call_unknown".into()),
                        r#type: "function".into(),
                        function: FunctionCall {
                            name: partial.name.unwrap_or_else(|| "unknown".into()),
                            arguments: partial.arguments,
                        },
                    })
                    .collect(),
            )
        };

        Message::assistant(content_opt, tool_calls)
    }
}

impl Default for StreamAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// DummyTool — встроенный платформенный тул для тестирования пайплайна
// ===========================================================================

/// Простейший тул, который возвращает фиксированный JSON.
/// Используется по умолчанию при создании Agent и в интеграционных тестах.
pub struct DummyTool {
    name: String,
}

impl DummyTool {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

#[async_trait]
impl AsyncTool for DummyTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: "A dummy tool for testing the agent pipeline".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string", "description": "Any input string" }
                },
                "required": ["input"]
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Platform
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        // Эмулируем успешное выполнение
        Ok(format!(r#"{{"status":"ok","tool":"{}","args":{}}}"#, self.name, arguments))
    }
}

// ===========================================================================
// Agent
// ===========================================================================

/// Главный цикл агента. Оркестрирует: LLM → Safety → Hooks → Tool Router.
pub struct Agent<P: ModelProvider> {
    pub provider: P,
    pub context: ContextManager,
    pub router: ToolRouter,
    pub safety: SafetyPipeline,
    /// PreToolHook — блокирующие хуки, вызываемые после Safety, до ToolRouter.
    pub pre_hooks: Vec<Box<dyn PreToolHook>>,
    /// PostToolHook — фоновые хуки, вызываемые после ToolRouter (fire-and-forget).
    pub post_hooks: Vec<Arc<dyn PostToolHook>>,
    /// Отправитель событий фронтенду (broadcast-канал WebSocket).
    pub frontend_tx: Option<broadcast::Sender<FrontendEvent>>,
    /// Получатель команд от фронтенда (mpsc-канал из WebSocket).
    pub safety_approval_rx: Option<mpsc::Receiver<ClientCommand>>,
    /// Долгосрочная векторная память (RAG). При наличии — выполняется retrieval
    /// перед каждым `stream_chat` для подмешивания релевантного контекста.
    pub memory_store: Option<Arc<VectorMemoryStore>>,
}

impl<P: ModelProvider> Agent<P> {
    /// Создать агента с переданным провайдером, пустым контекстом,
    /// пайплайном безопасности по умолчанию, пустыми списками хуков
    /// и всеми платформенными тулами (read_file, write_file, glob, grep).
    pub fn new(provider: P) -> Self {
        let mut router = ToolRouter::new();
        register_platform_tools(&mut router);
        Self {
            provider,
            context: ContextManager::new(),
            router,
            safety: default_pipeline(),
            pre_hooks: Vec::new(),
            post_hooks: Vec::new(),
            frontend_tx: None,
            safety_approval_rx: None,
            memory_store: None,
        }
    }

    /// Автоматически зарегистрировать MCP-тулы из конфигурации контейнеров.
    ///
    /// Для каждого контейнера вызывает `McpToolProvider::discover()`,
    /// и регистрирует найденные тулы в `ToolRouter`.
    ///
    /// В текущей версии `discover` — заглушка (всегда Err). Когда
    /// Nemotron-3-Super реализует MCP-транспорт, этот метод начнёт
    /// работать полноценно.
    pub async fn register_mcp_tools(&mut self, containers: &[crate::tool_routing::mcp_transport::McpContainerConfig]) {
        use crate::tool_routing::mcp_transport::{McpTool, McpToolProvider};

        for cfg in containers {
            match McpToolProvider::discover(&cfg.container_id, &cfg.command).await {
                Ok(tools) => {
                    for t in &tools {
                        let mcp_tool = McpTool::new(
                            t.name.clone(),
                            cfg.container_id.clone(),
                            t.description.clone(),
                            t.parameters.clone(),
                            cfg.command.clone(),
                        );
                        self.router.register(Box::new(mcp_tool));
                        tracing::info!(
                            "Registered MCP tool '{}' from container '{}'",
                            t.name,
                            cfg.container_id,
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "MCP discovery failed for container '{}': {e}",
                        cfg.container_id,
                    );
                }
            }
        }
    }

    /// Зарегистрировать PreToolHook.
    pub fn add_pre_hook(&mut self, hook: Box<dyn PreToolHook>) {
        tracing::debug!("registered PreToolHook");
        self.pre_hooks.push(hook);
    }

    /// Зарегистрировать PostToolHook.
    pub fn add_post_hook(&mut self, hook: Arc<dyn PostToolHook>) {
        tracing::debug!("registered PostToolHook");
        self.post_hooks.push(hook);
    }

    /// Установить отправитель событий фронтенду.
    pub fn set_frontend_tx(&mut self, tx: broadcast::Sender<FrontendEvent>) {
        self.frontend_tx = Some(tx);
    }

    /// Установить получатель команд от фронтенда (для safety approval).
    pub fn set_safety_approval_rx(&mut self, rx: mpsc::Receiver<ClientCommand>) {
        self.safety_approval_rx = Some(rx);
    }

    /// Установить хранилище долгосрочной векторной памяти (RAG).
    pub fn set_memory_store(&mut self, store: Arc<VectorMemoryStore>) {
        self.memory_store = Some(store);
    }

    /// Один шаг (итерация) цикла агента.
    ///
    /// 1. Запрос к LLM (stream_chat с текущим контекстом и определениями тулов)
    /// 2. Аккумуляция чанков → assistant Message
    /// 3. Добавление assistant Message в контекст
    /// 4. Если тулов нет → возвращаем `Some(content)` (финальный ответ)
    /// 5. Если есть тулы → проверка Safety → выполнение → возвращаем `None` (нужна ещё итерация)
    pub async fn run_step(&mut self, model: &str) -> Result<Option<String>, AgentError> {
        // --- Шаг 0: proactive compaction (Step C) ---
        if self.context.estimate_tokens() > 6000 {
            tracing::debug!("Token estimate >6000, triggering proactive compaction");
            self.maybe_compact_context(model).await;
        }

        // --- Шаг 0.5: RAG retrieval из долгосрочной памяти ---
        let extra_messages = if let Some(ref memory_store) = self.memory_store {
            let user_content = self
                .context
                .current_messages()
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .and_then(|m| m.content.as_deref())
                .filter(|c| !c.is_empty());

            if let Some(text) = user_content {
                match self.provider.get_embedding(text).await {
                    Ok(embedding) => {
                        let results = memory_store.query(&embedding, 2);
                        let relevant: Vec<_> = results
                            .into_iter()
                            .filter(|entry| {
                                let sim = crate::memory::vector_db::cosine_similarity(
                                    &embedding,
                                    &entry.embedding,
                                );
                                sim > 0.5
                            })
                            .collect();
                        if !relevant.is_empty() {
                            let facts: Vec<&str> =
                                relevant.iter().map(|e| e.text.as_str()).collect();
                            let summary = facts.join("\n\n---\n\n");
                            tracing::debug!(
                                "RAG: injected {} relevant memory fragment(s)",
                                relevant.len()
                            );
                            vec![Message::new(
                                Role::System,
                                format!(
                                    "[LONG-TERM MEMORY BACKGROUND]: Relevant historical facts:\n\n{summary}",
                                ),
                            )]
                        } else {
                            vec![]
                        }
                    }
                    Err(e) => {
                        tracing::warn!("RAG get_embedding failed: {e}");
                        vec![]
                    }
                }
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        let messages: Vec<Message> = if extra_messages.is_empty() {
            self.context.current_messages().to_vec()
        } else {
            let mut all = extra_messages;
            all.extend(self.context.current_messages().to_vec());
            all
        };
        let definitions = self.router.definitions();
        let tools = if definitions.is_empty() {
            None
        } else {
            // Step B: only send tool schemas when the model may need them.
            // Skip when the last message is an assistant response without tool_calls
            // (model is just continuing the conversation, doesn't need tool definitions).
            let should_provide = self.context.current_messages().last()
                .map(|last| last.role != Role::Assistant || last.tool_calls.is_some())
                .unwrap_or(true);
            if should_provide {
                Some(definitions)
            } else {
                None
            }
        };

        // --- Шаг 1-2: стриминг + аккумуляция ---
        let mut stream = match self.provider.stream_chat(model, messages, tools).await {
            Ok(s) => s,
            Err(ProviderError::ApiError { status: 413, .. }) => {
                // Контекст слишком большой — обрезаем и ретраим
                tracing::warn!("Context overflow (413) — trimming and retrying");
                self.trim_context_for_retry();
                let messages = self.context.current_messages().to_vec();
                let definitions = self.router.definitions();
                let should_provide = self.context.current_messages().last()
                    .map(|last| last.role != Role::Assistant || last.tool_calls.is_some())
                    .unwrap_or(true);
                let tools = if definitions.is_empty() || !should_provide {
                    None
                } else {
                    Some(definitions)
                };
                self.provider.stream_chat(model, messages, tools).await?
            }
            Err(e) => return Err(AgentError::Provider(e)),
        };
        let mut acc = StreamAccumulator::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            acc.push(&chunk);
        }

        let assistant_msg = acc.into_message();
        let has_tool_calls = assistant_msg.tool_calls.is_some();

        // --- Шаг 3: сохраняем ответ модели ---
        self.context.push(assistant_msg);

        // --- Шаг 4: если тулов нет — финальный ответ ---
        if !has_tool_calls {
            // Берём последнее сообщение из контекста
            let msgs = self.context.current_messages();
            return Ok(msgs.last().and_then(|m| m.content.clone()));
        }

        // --- Шаг 5: обработка тулов ---
        let last_msg = self.context.current_messages().last().unwrap();
        let mut tool_calls = last_msg.tool_calls.as_ref().unwrap().clone();

        for call in &mut tool_calls {
            // Safety
            let decision = self
                .safety
                .verify(call, self.context.current_messages())
                .await;

            match decision {
                SafetyDecision::Allow => {
                    tracing::info!(
                        tool = %call.function.name,
                        "[SAFETY] Tool execution APPROVED"
                    );
                }
                SafetyDecision::Deny(reason) => {
                    tracing::error!(
                        tool = %call.function.name,
                        reason = %reason,
                        "[SAFETY] Tool execution DENIED"
                    );
                    return Err(AgentError::SafetyViolation(reason));
                }
                SafetyDecision::RequiresApproval(reason) => {
                    tracing::warn!(
                        tool = %call.function.name,
                        reason = %reason,
                        "[SAFETY] Requires approval"
                    );

                    // Отправить событие фронтенду
                    if let Some(ref tx) = self.frontend_tx {
                        let _ = tx.send(FrontendEvent::SafetyReviewRequired {
                            tool_name: call.function.name.clone(),
                            reason: reason.clone(),
                        });
                    }

                    // CLI-приглашение (всегда)
                    println!("\n⚠️  Requires approval: {reason}");
                    print!("Proceed? (Y/n): ");
                    use std::io::Write;
                    std::io::stdout().flush()?;

                    // Выбор канала получения ответа
                    let approved = if let Some(ref mut rx) = self.safety_approval_rx {
                        tokio::select! {
                            cmd = rx.recv() => {
                                match cmd {
                                    Some(ClientCommand::SafetyResponse { approved }) => approved,
                                    _ => false,
                                }
                            }
                            input = async {
                                let mut buf = String::new();
                                let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
                                reader.read_line(&mut buf).await.ok()?;
                                Some(buf.trim().to_lowercase())
                            } => {
                                match input {
                                    Some(s) => s != "n" && s != "no",
                                    None => false,
                                }
                            }
                        }
                    } else {
                        // Нет фронтенда — только stdin
                        let mut input = String::new();
                        let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
                        reader.read_line(&mut input).await?;
                        input.trim().to_lowercase() != "n" && input.trim().to_lowercase() != "no"
                    };

                    if !approved {
                        return Err(AgentError::UserAbort);
                    }
                }
            }

            // PreToolHook — последовательный вызов всех pre_hooks
            for hook in &self.pre_hooks {
                if let Err(reason) = hook.on_pre_use(call, self.context.current_messages()).await {
                    return Err(AgentError::ToolExecution(format!(
                        "PreToolHook rejected call '{}': {reason}",
                        call.function.name
                    )));
                }
            }

            // Исполнение
            let result = self.router.route(call).await;

            // PostToolHook — fire-and-forget через tokio::spawn
            for hook in &self.post_hooks {
                let call_clone = call.clone();
                let result_clone = match &result {
                    Ok(msg) => Ok(msg.content.clone().unwrap_or_default()),
                    Err(e) => Err(e.clone()),
                };
                let ctx_clone = self.context.current_messages().to_vec();
                let hook = Arc::clone(hook);
                tokio::spawn(async move {
                    hook.on_post_use(&call_clone, &result_clone, &ctx_clone).await;
                });
            }

            match result {
                Ok(msg) => {
                    self.context.push(msg);
                }
                Err(e) => {
                    return Err(AgentError::ToolExecution(e));
                }
            }
        }

        // --- Шаг 6: проверка компакции контекста ---
        self.maybe_compact_context(model).await;

        // Вернули None — сигнал, что нужна следующая итерация
        Ok(None)
    }

    /// Проверить необходимость компакции контекста и выполнить её через
    /// скрытый вызов LLM.
    ///
    /// При ошибке суммаризации просто логируем и продолжаем — это не фатально.
    async fn maybe_compact_context(&mut self, model: &str) {
        let config = crate::context::CompactionConfig::default();
        if !self.context.needs_compaction(&config) {
            return;
        }

        let (start, end) = match self.context.compaction_range(&config) {
            Some(r) => r,
            None => return,
        };

        // Собираем сообщения для суммаризации
        let msgs_to_summarize: Vec<Message> =
            self.context.current_messages()[start..end].to_vec();

        // Промпт для скрытого вызова LLM
        let summary_prompt = Message::new(
            Role::System,
            "You are a conversation summarizer. Summarize the following conversation segment, \
             preserving key facts, decisions, variable values, and tool execution results. \
             Be concise but complete.",
        );

        let summarize_msgs: Vec<Message> =
            std::iter::once(summary_prompt).chain(msgs_to_summarize).collect();

        match self.provider.stream_chat(model, summarize_msgs, None).await {
            Ok(mut stream) => {
                let mut summary = String::new();
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(chunk) => {
                            if let Some(ref text) = chunk.delta_content {
                                summary.push_str(text);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Compaction chunk error: {e}");
                            return;
                        }
                    }
                }
                if !summary.is_empty() {
                    self.context.compact(summary, start, end);
                    tracing::info!(
                        "Context compacted: removed {} messages",
                        end.saturating_sub(start)
                    );
                }
            }
            Err(e) => {
                tracing::warn!("Compaction summarization failed: {e}");
            }
        }
    }

    /// При 413 (контекст переполнен) — удаляем старые tool_call ↔ tool_result пары,
    /// пока не сойдём в лимит. Простейшая стратегия: удаляем одну самую старую пару
    /// за вызов (следующий ретрай снова попадёт сюда, если всё ещё переполнено).
    fn trim_context_for_retry(&mut self) {
        let msgs = self.context.current_messages().to_vec();

        // Ищем первую Tool-сообщение (самый старый результат тула) и
        // соответствующий Assistant-блок с tool_calls.
        let tool_result_idx = msgs.iter().position(|m| m.role == Role::Tool);
        if tool_result_idx.is_none() {
            return;
        }
        let tool_result_idx = tool_result_idx.unwrap();

        // Ищем предшествующий Assistant-блок с tool_calls, который идёт
        // перед этим tool_result. Он будет tool_result_idx - 1 (если там Assistant).
        let remove_end = tool_result_idx + 1;
        let remove_start = if tool_result_idx > 0 && msgs[tool_result_idx - 1].role == Role::Assistant {
            tool_result_idx - 1
        } else {
            tool_result_idx
        };

        tracing::info!(
            "Trimming context: removing messages [{}, {})",
            remove_start,
            remove_end
        );

        self.context.remove_range(remove_start, remove_end);
    }

    pub async fn run(&mut self, model: &str) -> Result<String, AgentError> {
        loop {
            match self.run_step(model).await {
                Ok(Some(content)) => return Ok(content),
                Ok(None) => {
                    // Есть tool_calls — продолжаем цикл
                    tracing::debug!("Agent loop iteration — tool calls detected, continuing");
                }
                Err(e) => return Err(e),
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // -----------------------------------------------------------------------
    // MockProvider — возвращает заранее заданные последовательности чанков
    // -----------------------------------------------------------------------

    struct MockProvider {
        /// Каждый вызов stream_chat возвращает следующий Vec<ChatChunk> из этого списка.
        responses: Vec<Vec<ChatChunk>>,
        call_count: Arc<AtomicUsize>,
    }

    impl MockProvider {
        fn new(responses: Vec<Vec<ChatChunk>>) -> Self {
            Self {
                responses,
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn stream_chat(
            &self,
            _model: &str,
            _messages: Vec<Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> Result<ProviderStream, ProviderError> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            let chunks = self.responses.get(idx).cloned().unwrap_or_default();

            let stream = async_stream::try_stream! {
                for chunk in chunks {
                    yield chunk;
                }
            };

            Ok(Box::pin(stream))
        }

        async fn get_embedding(&self, _text: &str) -> Result<Vec<f32>, ProviderError> {
            // Mock — возвращаем фиксированный 4-мерный вектор
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
    }

    // -----------------------------------------------------------------------
    // StreamAccumulator tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_accumulator_text_only() {
        let mut acc = StreamAccumulator::new();
        acc.push(&ChatChunk {
            delta_content: Some("Hello ".into()),
            delta_tool_calls: None,
        });
        acc.push(&ChatChunk {
            delta_content: Some("world!".into()),
            delta_tool_calls: None,
        });

        let msg = acc.into_message();
        assert_eq!(msg.role, Role::Assistant);
        assert_eq!(msg.content.as_deref(), Some("Hello world!"));
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn test_accumulator_tool_calls() {
        let mut acc = StreamAccumulator::new();

        // Первый фрагмент: id + name
        acc.push(&ChatChunk {
            delta_content: None,
            delta_tool_calls: Some(vec![ToolCallChunk {
                index: Some(0),
                id: Some("call_abc".into()),
                r#type: Some("function".into()),
                function: Some(FunctionCallChunk {
                    name: Some("dummy".into()),
                    arguments: None,
                }),
            }]),
        });

        // Второй фрагмент: аргументы частями
        acc.push(&ChatChunk {
            delta_content: None,
            delta_tool_calls: Some(vec![ToolCallChunk {
                index: Some(0),
                id: None,
                r#type: None,
                function: Some(FunctionCallChunk {
                    name: None,
                    arguments: Some(r#"{"input":"#.into()),
                }),
            }]),
        });

        acc.push(&ChatChunk {
            delta_content: None,
            delta_tool_calls: Some(vec![ToolCallChunk {
                index: Some(0),
                id: None,
                r#type: None,
                function: Some(FunctionCallChunk {
                    name: None,
                    arguments: Some(r#""hello"}"#.into()),
                }),
            }]),
        });

        let msg = acc.into_message();
        assert_eq!(msg.role, Role::Assistant);
        assert!(msg.content.is_none());
        let calls = msg.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_abc");
        assert_eq!(calls[0].function.name, "dummy");
        assert_eq!(calls[0].function.arguments, r#"{"input":"hello"}"#);
    }

    // -----------------------------------------------------------------------
    // Agent integration test
    // -----------------------------------------------------------------------

    fn make_tool_call_chunk(index: usize, id: &str, name: &str, args: &str) -> ChatChunk {
        ChatChunk {
            delta_content: None,
            delta_tool_calls: Some(vec![ToolCallChunk {
                index: Some(index),
                id: Some(id.into()),
                r#type: Some("function".into()),
                function: Some(FunctionCallChunk {
                    name: Some(name.into()),
                    arguments: Some(args.into()),
                }),
            }]),
        }
    }

    #[tokio::test]
    async fn test_agent_full_cycle() {
        // Первый вызов stream_chat: модель решает вызвать dummy-тул
        let tool_chunks = vec![
            make_tool_call_chunk(0, "call_d1", "dummy", r#"{"input":"test"}"#),
        ];

        // Второй вызов stream_chat: модель выдаёт финальный текст
        let response_chunks = vec![ChatChunk {
            delta_content: Some("Result: dummy executed successfully".into()),
            delta_tool_calls: None,
        }];

        let provider = MockProvider::new(vec![tool_chunks, response_chunks]);

        let mut agent = Agent::new(provider);
        // Регистрируем dummy-тул для теста (по умолчанию Agent регистрирует
        // read_file/write_file/glob/grep, но нам нужен предсказуемый тул)
        agent.router.register(Box::new(DummyTool::new("dummy")));

        // --- Первый шаг: модель вызывает тул ---
        let result = agent.run_step("test-model").await.unwrap();
        assert!(result.is_none(), "expected None (tool calls pending)");

        // Проверяем, что в контексте появились: assistant (c tool_call), tool_result
        let msgs = agent.context.current_messages();
        // Индекс 0 — assistant с тулом, 1 — результат выполнения тула
        assert_eq!(msgs.len(), 2, "expected 2 messages (assistant + tool_result)");

        // assistant должен содержать tool_call
        assert_eq!(msgs[0].role, Role::Assistant);
        assert!(msgs[0].tool_calls.is_some());
        let calls = msgs[0].tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "dummy");
        assert_eq!(calls[0].function.arguments, r#"{"input":"test"}"#);

        // tool_result
        assert_eq!(msgs[1].role, Role::Tool);
        let content = msgs[1].content.as_deref().unwrap();
        assert!(content.contains("dummy"));
        assert!(content.contains(r#"{"input":"test"}"#));

        // --- Второй шаг: модель даёт финальный ответ ---
        let result = agent.run_step("test-model").await.unwrap();
        assert!(result.is_some(), "expected Some (final answer)");
        assert_eq!(result.unwrap(), "Result: dummy executed successfully");
    }

    #[tokio::test]
    async fn test_agent_empty_context() {
        let provider = MockProvider::new(vec![vec![ChatChunk {
            delta_content: Some("Hello!".into()),
            delta_tool_calls: None,
        }]]);

        let mut agent = Agent::new(provider);
        let result = agent.run_step("test-model").await.unwrap();
        assert_eq!(result.as_deref(), Some("Hello!"));
    }
}
