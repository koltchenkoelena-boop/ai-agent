// ---------------------------------------------------------------------------
// Platform (Built-in) tools
//
// TO DO: Реализовать конкретные Platform-тулы, используя AsyncTool trait.
//
// Примеры будущих тулов:
//   - read_file   — чтение файлов из файловой системы
//   - shell       — исполнение shell-команд
//   - search_code — grep/ripgrep по проекту
//   - glob        — поиск файлов по glob-паттерну
//
// Каждый Platform-тул регистрируется в ToolRouter:
//   router.register(Box::new(ReadFileTool));
//   router.register(Box::new(ShellTool));
// ---------------------------------------------------------------------------

use async_trait::async_trait;

use crate::tool_routing::{AsyncTool, ToolKind};
use crate::types::ToolDefinition;

/// Placeholder — будет заменён на реальную фабрику Platform-тулов.
pub struct PlatformToolRegistry;

impl PlatformToolRegistry {
    /// Вернуть список всех встроенных тулов.
    /// Пока заглушка: возвращает пустой вектор.
    pub fn builtins() -> Vec<Box<dyn AsyncTool>> {
        vec![Box::new(DummyPlatformTool)]
    }
}

// ---------------------------------------------------------------------------
// Dummy (для тестирования инфраструктуры)
// ---------------------------------------------------------------------------

struct DummyPlatformTool;

#[async_trait]
impl AsyncTool for DummyPlatformTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "_dummy_platform".into(),
            description: "Dummy platform tool (placeholder)".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        }
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Platform
    }

    async fn execute(&self, _arguments: &str) -> Result<String, String> {
        Ok(r#"{"status":"ok","message":"dummy platform tool executed"}"#.into())
    }
}
