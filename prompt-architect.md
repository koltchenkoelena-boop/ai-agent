# Архитектурный контекст: Rust CLI AI Agent

## Репозиторий
`https://github.com/tester-bcs/ai-agent`

## Язык и инструменты
- Rust 2024 edition, async (tokio)
- Ollama через SSE streaming
- Docker exec + JSON-RPC 2.0 для MCP контейнеров
- 40 unit-тестов, все проходят

---

## 1. Полная структура модулей

```
src/
├── lib.rs              # re-exports: types, provider, context, tool_routing, safety, agent
├── types.rs            # Базовые типы сообщений и тулов
├── provider.rs         # ModelProvider trait, OllamaProvider
├── context.rs          # ContextManager, Branch, MergeStrategy, CompactionConfig
├── agent.rs            # Agent<P>, StreamAccumulator, AgentError
├── safety/mod.rs       # SafetyPipeline, SafetyStage, SafetyDecision
├── tool_routing/
│   ├── mod.rs          # ToolRouter, AsyncTool trait, ToolKind
│   ├── platform.rs     # stub
│   ├── frontend.rs     # stub
│   └── mcp_transport.rs # MCP transport (docker exec + JSON-RPC 2.0)
└── main.rs             # Demo loop
```

## 2. Ключевые типажи (traits)

### `ModelProvider` — провайдер LLM
```rust
#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn stream_chat(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<ProviderStream, ProviderError>;
}
```

### `AsyncTool` — инструмент для LLM
```rust
#[async_trait]
pub trait AsyncTool: Send + Sync {
    fn definition(&self) -> ToolDefinition;  // имя, описание, JSON Schema параметров
    fn kind(&self) -> ToolKind;              // Platform | Frontend | Mcp { container_id }
    async fn execute(&self, arguments: &str) -> Result<String, String>;
}
```

### `SafetyStage` — стадия пайплайна безопасности
```rust
#[async_trait]
pub trait SafetyStage: Send + Sync {
    async fn verify(&self, call: &ToolCall, history: &[Message]) -> SafetyDecision;
}
```

## 3. Ключевые структуры

### `Message` — единица диалога
```rust
pub struct Message {
    pub role: Role,              // System | User | Assistant | Tool
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,  // только для Assistant
    pub tool_call_id: Option<String>,       // только для Tool
}
```

### `ContextManager` — Git-like branching контекст
```rust
pub struct ContextManager { /* HashMap<id, Branch> + current_id */ }

// Методы:
pub fn new() -> Self;
pub fn current_messages(&self) -> &[Message];
pub fn push(&mut self, msg: Message);
pub fn create_branch(&mut self, name: &str) -> &Branch;     // git checkout -b
pub fn switch(&mut self, id: String) -> Result<(), String>;
pub fn switch_by_name(&mut self, name: &str) -> Result<(), String>;
pub fn merge(&mut self, source_id: &str, strategy: MergeStrategy) -> Result<(), String>;
pub fn snapshot(&self) -> HashMap<String, (String, Vec<Message>)>;
```

### `MergeStrategy` — стратегии слияния
- `Overwrite` — полная замена
- `FastForward` — добавление новых (после fork_point)
- `Union` — longest-common-prefix + оба хвоста (dedup)

### `CompactionConfig` — авто-компакция
```rust
pub struct CompactionConfig {
    pub max_messages: usize,    // default 15
    pub reserve_recent: usize,  // default 4
}
// needs_compaction(&config) -> bool
// compaction_range(&config) -> Option<(usize, usize)>
// compact(summary, start, end) -> splice of System msg
```

### `ToolRouter` — реестр и роутинг тулов
```rust
pub struct ToolRouter { /* HashMap<String, Box<dyn AsyncTool>> */ }
// register(Box<dyn AsyncTool>), get(name) -> Option<&dyn AsyncTool>
// route(&ToolCall) -> Result<Message, String>
// route_all(&[ToolCall]) -> Vec<Result<Message, String>>
// definitions() -> Vec<ToolDefinition>
```

### `Agent<P: ModelProvider>` — главный цикл
```rust
pub struct Agent<P: ModelProvider> {
    pub provider: P,
    pub context: ContextManager,
    pub router: ToolRouter,
    pub safety: SafetyPipeline,
}

impl<P: ModelProvider> Agent<P> {
    pub fn new(provider: P) -> Self;    // регистрирует DummyTool + default pipeline
    pub async fn run_step(&mut self, model: &str) -> Result<Option<String>, AgentError>;
    pub async fn run(&mut self, model: &str) -> Result<String, AgentError>;
    pub async fn register_mcp_tools(&mut self, containers: &[McpContainerConfig]);
    async fn maybe_compact_context(&mut self, model: &str);
}
```

**`run_step()` lifecycle:**
1. LLM запрос (stream_chat с контекстом + тулами)
2. StreamAccumulator собирает чанки → Assistant Message
3. Push assistant msg в контекст
4. Если tool_calls нет → return Some(content)
5. Для каждого tool_call: Safety → (Allow/Deny/RequiresApproval) → execute → push result
6. maybe_compact_context() — скрытый LLM вызов для суммаризации
7. return Ok(None) (continue loop)

### `SafetyPipeline` — пайплайн из 5 стадий
```rust
pub struct SafetyPipeline { stages: Vec<Box<dyn SafetyStage>> }

// Порядок:
// 1. SecurityStage — блокирует инъекции в DANGEROUS_TOOLS (execute_bash, eval, sh)
// 2. EgressStage — заглушка (всегда Allow)
// 3. AdversaryStage — заглушка
// 4. PermissionStage — RequiresApproval для execute_bash
// 5. RepetitionStage — 3 одинаковых последовательных вызова → Deny
```

### `AgentError` — ошибки агента
```rust
pub enum AgentError {
    Provider(ProviderError),
    SafetyViolation(String),
    ToolExecution(String),
    Io(std::io::Error),
    UserAbort,
}
```

## 4. MCP Transport (реализован)

**JSON-RPC 2.0** через `tokio::process::Command` + `docker exec -i`:

```rust
// Lifecycle:
McpToolProvider::discover(container_id, &[String]) -> Result<Vec<McpDiscoveredTool>>
//   1. spawn: docker exec -i <container> <command>
//   2. send: {"jsonrpc":"2.0","id":1,"method":"initialize","params":{...}}
//   3. send: {"jsonrpc":"2.0","method":"notifications/initialized"}
//   4. send: {"jsonrpc":"2.0","id":2,"method":"tools/list"}
//   5. parse response.tools[] -> Vec<McpDiscoveredTool>

McpTool::execute(arguments) -> Result<String, String>
//   1. spawn + initialize (как выше)
//   2. send: {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"...","arguments":{...}}}
//   3. parse response.content[].text -> String
```

Конфиг контейнеров (`mcp_containers.json`):
```json
[
  { "container_id": "my-mcp-server", "command": ["docker", "exec", "-i", "my-mcp-server", "mcp"] }
]
```

Загрузка в `main.rs`:
```rust
if let Ok(containers) = load_mcp_config("mcp_containers.json") {
    agent.register_mcp_tools(&containers).await;
}
```

## 5. Что НЕ реализовано

### Component 6: Hooks
Требуется добавить в `Agent<P>`:
- `PreToolUse` — блокирующий хук до execute (может модифицировать/отменить вызов)
- `PostToolUse` — фоновый хук после execute (логирование, метрики, уведомления)

### Platform Tools
В `platform.rs` только заглушка. Реальные тулы:
- `read_file` — чтение файлов
- `shell` — shell команды (с Safety)
- `glob` — поиск файлов
- `grep` — поиск в файлах

### Frontend Transport
В `frontend.rs` только заглушка. Нужен WebSocket/gRPC сервер для UI.

### Тесты
- e2e тест с реальным Ollama
- Тест для MCP с mock Docker контейнером

## 6. Зависимости (Cargo.toml)
```toml
tokio, async-trait, futures-util, tokio-util,
reqwest, async-stream, serde, serde_json,
thiserror, uuid, chrono, tracing, tracing-subscriber, serde_ignored
```

## 7. Тесты (40 шт)
- `context::tests` — 19 тестов (ветки, мерж, компакция)
- `agent::tests` — 3 теста (StreamAccumulator, full cycle, empty context)
- `safety::tests` — 14 тестов (Security, Permission, Repetition, Pipeline)
- `tool_routing::tests` — 3 теста (register, route, definitions)

Запуск: `cargo test --lib`
