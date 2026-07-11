// ---------------------------------------------------------------------------
// MCP Transport — Model Context Protocol для изолированных Docker-контейнеров
//
// JSON-RPC 2.0 коммуникация через docker exec -i stdin/stdout.
// Жизненный цикл: initialize → notifications/initialized → tools/list → tools/call
// ---------------------------------------------------------------------------

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::tool_routing::{AsyncTool, ToolKind};
use crate::types::ToolDefinition;

// ===========================================================================
// JSON-RPC 2.0 типы
// ===========================================================================

#[derive(Serialize)]
struct Request<'a> {
    jsonrpc: &'a str,
    id: Id,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Serialize)]
struct Notification<'a> {
    jsonrpc: &'a str,
    method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct Response {
    jsonrpc: String,
    id: Id,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorObject>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ErrorObject {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Id {
    Num(u64),
    #[allow(dead_code)]
    String(String),
}

// ===========================================================================
// Helpers для JSON-RPC коммуникации
// ===========================================================================

/// Отправить JSON-RPC запрос и прочитать ответ.
async fn send_request<S, R>(
    stdin: &mut S,
    stdout: &mut R,
    id: u64,
    method: &str,
    params: Option<Value>,
) -> Result<Value, String>
where
    S: AsyncWriteExt + Unpin,
    R: AsyncBufReadExt + Unpin,
{
    let request = Request {
        jsonrpc: "2.0",
        id: Id::Num(id),
        method,
        params,
    };
    let mut buf = serde_json::to_string(&request).map_err(|e| e.to_string())?;
    buf.push('\n');
    stdin
        .write_all(buf.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.flush().await.map_err(|e| e.to_string())?;

    let mut line = String::new();
    stdout
        .read_line(&mut line)
        .await
        .map_err(|e| format!("stdout read error: {e}"))?;
    let response: Response =
        serde_json::from_str(&line).map_err(|e| format!("JSON-RPC parse error: {e}"))?;

    if let Some(error) = response.error {
        return Err(format!("JSON-RPC error {}: {}", error.code, error.message));
    }
    response
        .result
        .ok_or_else(|| "Missing result in JSON-RPC response".to_string())
}

/// Отправить JSON-RPC нотификацию (без ответа).
#[allow(dead_code)]
async fn send_notification<S>(
    stdin: &mut S,
    method: &str,
    params: Option<Value>,
) -> Result<(), String>
where
    S: AsyncWriteExt + Unpin,
{
    let notification = Notification {
        jsonrpc: "2.0",
        method,
        params,
    };
    let mut buf = serde_json::to_string(&notification).map_err(|e| e.to_string())?;
    buf.push('\n');
    stdin
        .write_all(buf.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

// ===========================================================================
// McpDiscoveredTool
// ===========================================================================

/// Описание тула, полученное от Docker-контейнера через MCP tools/list.
#[derive(Debug, Clone)]
pub struct McpDiscoveredTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

// ===========================================================================
// McpContainerConfig
// ===========================================================================

/// Конфигурация MCP-контейнера для автоматического discovery.
#[derive(Debug, Clone, Deserialize)]
pub struct McpContainerConfig {
    /// Имя или ID Docker-контейнера.
    pub container_id: String,
    /// Команда для запуска (docker exec ...).
    pub command: Vec<String>,
}

/// Загрузить список контейнеров из JSON-файла.
pub fn load_mcp_config(path: &str) -> Result<Vec<McpContainerConfig>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("Cannot read MCP config '{path}': {e}"))?;
    serde_json::from_str(&content).map_err(|e| format!("Invalid MCP config '{path}': {e}"))
}

// ===========================================================================
// McpToolProvider
// ===========================================================================

/// Провайдер обнаружения MCP-тулов в Docker-контейнерах.
pub struct McpToolProvider;

impl McpToolProvider {
    /// Подключиться к контейнеру, выполнить MCP initialize + tools/list,
    /// вернуть список обнаруженных тулов.
    #[allow(unused_variables)]
    pub async fn discover(
        container_id: &str,
        command: &[String],
    ) -> Result<Vec<McpDiscoveredTool>, String> {
        let (mut child, mut stdin, mut stdout) = Self::spawn_and_run(command).await?;

        // tools/list
        let tools_result = send_request(&mut stdin, &mut stdout, 2, "tools/list", None).await?;

        let tools = tools_result
            .get("tools")
            .ok_or_else(|| "Missing 'tools' field in response".to_string())?
            .as_array()
            .ok_or_else(|| "'tools' field is not an array".to_string())?;

        let mut discovered = Vec::new();
        for tool in tools {
            let name = tool
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Tool missing 'name' string".to_string())?;
            let description = tool
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let parameters = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({}));

            discovered.push(McpDiscoveredTool {
                name: name.to_string(),
                description,
                parameters,
            });
        }

        let _ = child.kill().await;
        let _ = child.wait().await;
        Ok(discovered)
    }

    /// Запустить docker exec -i, выполнить MCP initialize handshake,
    /// вернуть (child, stdin, stdout).
    async fn spawn_and_run(
        command: &[String],
    ) -> Result<
        (
            Child,
            tokio::process::ChildStdin,
            tokio::io::BufReader<tokio::process::ChildStdout>,
        ),
        String,
    > {
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn process: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to take stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to take stdout".to_string())?;

        // Register stdin/stdout for async I/O
        let mut stdin = stdin;
        let mut stdout = BufReader::new(stdout);

        // Initialize
        let init_params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "ai-agent",
                "version": "0.1.0"
            }
        });
        send_request(&mut stdin, &mut stdout, 1, "initialize", Some(init_params)).await?;

        // Notify initialized
        send_notification(&mut stdin, "notifications/initialized", None).await?;

        Ok((child, stdin, stdout))
    }
}

// ===========================================================================
// McpTool — асинхронный тул для MCP-контейнера
// ===========================================================================

/// Реальный MCP-тул, вызывающий tools/call через Docker-контейнер.
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub container_id: String,
    pub command: Vec<String>,
}

impl McpTool {
    pub fn new(
        name: String,
        container_id: String,
        description: String,
        parameters: Value,
        command: Vec<String>,
    ) -> Self {
        Self {
            name,
            description,
            parameters,
            container_id,
            command,
        }
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
        ToolKind::Mcp {
            container_id: self.container_id.clone(),
        }
    }

    async fn execute(&self, arguments: &str) -> Result<String, String> {
        let (mut child, mut stdin, mut stdout) =
            McpToolProvider::spawn_and_run(&self.command).await?;

        let args_value: Value = if arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(arguments)
                .map_err(|e| format!("Failed to parse arguments as JSON: {e}"))?
        };

        let call_params = json!({
            "name": self.name,
            "arguments": args_value
        });

        let result = send_request(&mut stdin, &mut stdout, 3, "tools/call", Some(call_params))
            .await?;

        let content = result
            .get("content")
            .ok_or_else(|| "Missing 'content' field in response".to_string())?
            .as_array()
            .ok_or_else(|| "'content' field is not an array".to_string())?;

        let mut texts = Vec::new();
        for item in content {
            if let Some(item_obj) = item.as_object() {
                if let Some(item_type) = item_obj.get("type").and_then(|v| v.as_str()) {
                    if item_type == "text" {
                        if let Some(text) = item_obj.get("text").and_then(|v| v.as_str()) {
                            texts.push(text.to_string());
                        }
                    }
                }
            }
        }

        let output = texts.join("\n");

        let _ = child.kill().await;
        let _ = child.wait().await;

        Ok(output)
    }
}
