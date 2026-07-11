// ---------------------------------------------------------------------------
// Tool Routing — распределение ToolCall по трём категориям:
//   Platform   → встроенные функции внутри бинарника
//   Frontend   → прокси на UI (WebSocket/gRPC)
//   MCP        → JSON-RPC в Docker-контейнер (Model Context Protocol)
// ---------------------------------------------------------------------------

use std::collections::HashMap;

use async_trait::async_trait;

use crate::types::{Message, ToolCall, ToolDefinition};

pub mod frontend;
pub mod mcp_transport;
pub mod platform;

// ---------------------------------------------------------------------------
// ToolKind
// ---------------------------------------------------------------------------

/// Категория тула — определяет, куда направляется вызов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolKind {
    /// Встроенный тул, исполняется внутри процесса.
    Platform,
    /// Прокси на фронтенд (WebSocket / gRPC).
    Frontend,
    /// MCP-тул, живущий в Docker-контейнере.
    Mcp {
        container_id: String,
    },
}

impl ToolKind {
    pub fn is_platform(&self) -> bool {
        matches!(self, Self::Platform)
    }

    pub fn is_frontend(&self) -> bool {
        matches!(self, Self::Frontend)
    }

    pub fn is_mcp(&self) -> bool {
        matches!(self, Self::Mcp { .. })
    }
}

// ---------------------------------------------------------------------------
// AsyncTool trait
// ---------------------------------------------------------------------------

#[async_trait]
pub trait AsyncTool: Send + Sync {
    /// Мета-описание тула (имя, описание, JSON Schema параметров).
    fn definition(&self) -> ToolDefinition;

    /// Категория, к которой относится тул.
    fn kind(&self) -> ToolKind;

    /// Исполнить тул. `arguments` — валидный JSON согласно `definition().parameters`.
    /// Возвращает строковый результат (чаще всего JSON) для передачи обратно модели.
    async fn execute(&self, arguments: &str) -> Result<String, String>;
}

// ---------------------------------------------------------------------------
// ToolRouter
// ---------------------------------------------------------------------------

/// Реестр инструментов и единая точка входа для исполнения ToolCall.
pub struct ToolRouter {
    tools: HashMap<String, Box<dyn AsyncTool>>,
}

impl ToolRouter {
    /// Пустой роутер.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Зарегистрировать инструмент.
    /// Если инструмент с таким именем уже существует — он будет заменён.
    pub fn register(&mut self, tool: Box<dyn AsyncTool>) {
        let name = tool.definition().name.clone();
        tracing::info!(tool = %name, kind = ?tool.kind(), "registering tool");
        self.tools.insert(name, tool);
    }

    /// Зарегистрировать несколько инструментов за раз.
    pub fn register_all(&mut self, tools: impl IntoIterator<Item = Box<dyn AsyncTool>>) {
        for tool in tools {
            self.register(tool);
        }
    }

    /// Найти зарегистрированный инструмент по имени.
    pub fn get(&self, name: &str) -> Option<&dyn AsyncTool> {
        self.tools.get(name).map(|b| b.as_ref())
    }

    /// Количество зарегистрированных тулов.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Имена всех зарегистрированных тулов.
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(|k| k.as_str()).collect()
    }

    /// Определения всех тулов — для передачи модели (tools parameter).
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| t.definition())
            .collect()
    }

    /// Исполнить один ToolCall.
    ///
    /// Возвращает `Message::tool_result(...)` — готовый к добавлению в контекст.
    pub async fn route(&self, call: &ToolCall) -> Result<Message, String> {
        let tool = self
            .tools
            .get(&call.function.name)
            .ok_or_else(|| format!("unknown tool: '{}'", call.function.name))?;

        let result = tool.execute(&call.function.arguments).await?;

        Ok(Message::tool_result(&call.id, result))
    }

    /// Исполнить пачку ToolCall (параллельно, через `tokio::join_all` или
    /// последовательно — в зависимости от семантики).
    pub async fn route_all(&self, calls: &[ToolCall]) -> Vec<Result<Message, String>> {
        use futures_util::future::join_all;
        join_all(calls.iter().map(|call| self.route(call))).await
    }
}

impl Default for ToolRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake platform tool for testing the router.
    struct EchoTool;

    #[async_trait]
    impl AsyncTool for EchoTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "echo".into(),
                description: "Echoes the input back".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" }
                    },
                    "required": ["message"]
                }),
            }
        }

        fn kind(&self) -> ToolKind {
            ToolKind::Platform
        }

        async fn execute(&self, arguments: &str) -> Result<String, String> {
            // naive echo — just return the raw argument json
            Ok(arguments.to_string())
        }
    }

    #[tokio::test]
    async fn test_register_and_route() {
        let mut router = ToolRouter::new();
        router.register(Box::new(EchoTool));

        assert_eq!(router.len(), 1);
        assert_eq!(router.tool_names(), vec!["echo"]);

        let call = ToolCall {
            id: "call_1".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "echo".into(),
                arguments: r#"{"message":"hello"}"#.into(),
            },
        };

        let msg = router.route(&call).await.unwrap();
        assert_eq!(msg.role, crate::types::Role::Tool);
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_1"));
        assert!(msg.content.as_deref().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn test_route_unknown_tool() {
        let router = ToolRouter::new();
        let call = ToolCall {
            id: "call_x".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "nonexistent".into(),
                arguments: "{}".into(),
            },
        };

        let err = router.route(&call).await.unwrap_err();
        assert!(err.contains("unknown tool"));
    }

    #[tokio::test]
    async fn test_definitions() {
        let mut router = ToolRouter::new();
        router.register(Box::new(EchoTool));

        let defs = router.definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "echo");
    }
}
