# Задача субагента Nemotron-3-Super: Реализация MCP Transport

## Системная роль
Ты — Expert System Programmer in Rust & Async Systems Architecture. Реализуешь `mcp_transport.rs` для нативного AI Agent CLI инструмента по протоколу Model Context Protocol (MCP) через Docker-контейнеры.

## 1. Архитектурный контекст
Мы пишем нативный Rust CLI-агент. Кодовая база уже содержит:
- `types.rs` — `Message`, `ToolCall`, `ToolDefinition`
- `provider.rs` — Ollama-подключение
- `tool_routing/mod.rs` — оркестратор роутинга (трейт `AsyncTool`, enum `ToolKind`, структура `ToolRouter`)

Твоя задача — переписать `src/tool_routing/mcp_transport.rs` из заглушки в production-grade MCP-мост над изолированными Docker-контейнерами.

**Путь к проекту:** `/home/avk/workspace/ai-agent/`

## 2. Повторно используемые типы (НЕ переопределять, импортировать из `crate::types` и `crate::tool_routing`)
- `ToolDefinition` — схема тула, отправляемая LLM
- `AsyncTool` — трейт для реализации. Метод: `async fn execute(&self, arguments: &str) -> Result<String, String>`
- `ToolKind` — enum: `ToolKind::Mcp { container_id: String }`. Другие варианты: `Platform`, `Frontend`

## 3. Техническая спецификация и жизненный цикл

Каждое выполнение MCP-тула или инициализация проксируется через Docker-контейнер с помощью `tokio::process::Command`:

```
docker exec -i <container_id> <mcp_server_command_and_args>
```

Транспорт взаимодействует через **stdin/stdout** по протоколу **JSON-RPC 2.0**.

### Шаг 1: Инициализация (`McpToolProvider::discover`)
Перед маршрутизацией вызовов необходимо опросить инструменты, которые предоставляет Docker-контейнер с MCP-сервером:

1. Отправить JSON-RPC `initialize` запрос в stdin контейнера. Прочитать ответ из stdout.
2. Отправить JSON-RPC `notifications/initialized` в stdin.
3. Отправить JSON-RPC `tools/list` запрос в stdin. Распарсить ответ в список тулов, маппируя их в стандартные `ToolDefinition` объекты.

### Шаг 2: Вызов инструмента во время выполнения (`McpTool::execute`)
Когда LLM инициирует вызов MCP-тула:

1. Отправить `tools/call` JSON-RPC запрос, содержащий имя инструмента (`name`) и распарсенные аргументы (`arguments`).
2. Прочитать JSON-RPC ответ. Извлечь из блока `content[]` текстовые строки и вернуть как плоский `Result<String, String>`.

## 4. Требования к коду и безопасности

1. **JSON-RPC 2.0 Compliance:** Безопасно моделировать JSON-RPC Request, Response и Error фреймы через `serde` или `serde_json::Value`. ID обрабатывать как integers или строки.
2. **Robust I/O Processing:** Использовать `tokio::io::AsyncBufReadExt::read_line` для построчного чтения stdout. MCP-потоки разделяются символами новой строки (`\n`).
3. **Нет висящих процессов:** Обеспечить корректную обработку процесса или закрытие stdin при сбросе ридеров (drop).
4. **Устойчивость к ошибкам:** Маппить JSON-RPC протокольные ошибки напрямую в описательные `Err(String)` структуры Rust.

## 5. Скелет кода для генерации

Реализовать следующий публичный API в `src/tool_routing/mcp_transport.rs`:

```rust
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use crate::types::ToolDefinition;
use crate::tool_routing::{AsyncTool, ToolKind};

// Определить JSON-RPC 2.0 фреймы здесь...

// ---------------------------------------------------------------------------
// McpDiscoveredTool — результат discovery от контейнера
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct McpDiscoveredTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// McpContainerConfig — конфигурация одного контейнера
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize)]
pub struct McpContainerConfig {
    pub container_id: String,
    pub command: Vec<String>,
}

// ---------------------------------------------------------------------------
// McpToolProvider
// ---------------------------------------------------------------------------

pub struct McpToolProvider;

impl McpToolProvider {
    /// Подключиться к контейнеру через docker exec -i, выполнить
    /// MCP initialize → notifications/initialized → tools/list,
    /// вернуть список обнаруженных тулов.
    ///
    /// `command` — полная команда: например ["docker", "exec", "-i", "my-container", "mcp"]
    pub async fn discover(container_id: &str, command: &[String]) -> Result<Vec<McpDiscoveredTool>, String> {
        // TODO: Реализовать docker exec -i, JSON-RPC lifecycle
    }
}

// ---------------------------------------------------------------------------
// McpTool — асинхронный тул для MCP-контейнера
// ---------------------------------------------------------------------------

pub struct McpTool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub container_id: String,
    pub command: Vec<String>,
}

impl McpTool {
    pub fn new(name: String, container_id: String, description: String, parameters: serde_json::Value, command: Vec<String>) -> Self {
        Self { name, description, parameters, container_id, command }
    }
}

#[async_trait]
impl AsyncTool for McpTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mcp { container_id: self.container_id.clone() }
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        // TODO: Выполнить docker exec -i, отправить `tools/call` запрос в stdin, прочитать stdout
    }
}
```

## 6. Проверка

После замены файла:
```bash
cd /home/avk/workspace/ai-agent && cargo check 2>&1
cd /home/avk/workspace/ai-agent && cargo test 2>&1
```

## Формат ответа
Только код, без объяснений. Выдай полный текст файла `mcp_transport.rs`, готовый к замене.
