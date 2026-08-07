// ---------------------------------------------------------------------------
// Frontend — WebSocket-сервер для трансляции событий агента на UI
//
//   FrontendServer     → axum WebSocket на 127.0.0.1:8080/ws
//   FrontendEvent      → типы событий (AgentMessage, ToolExecuting, …)
//   FrontendNotifierHook → PostToolHook, пушащий события в broadcast
// ---------------------------------------------------------------------------

use async_trait::async_trait;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, watch};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use axum::routing::get_service;

use crate::types::{Message, ToolCall};

// ---------------------------------------------------------------------------
// FrontendEvent
// ---------------------------------------------------------------------------

/// События, транслируемые на фронтенд через WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FrontendEvent {
    /// Текстовый ответ агента (финальный или промежуточный).
    AgentMessage {
        content: String,
    },
    /// Heartbeat — клиент игнорирует, держит соединение живым.
    Ping,
    /// Агент начал выполнение инструмента.
    ToolExecuting {
        tool_name: String,
        arguments: String,
    },
    /// Результат выполнения инструмента.
    ToolResult {
        tool_name: String,
        result: String,
    },
    /// Safety-пайплайн запросил подтверждение пользователя.
    SafetyReviewRequired {
        tool_name: String,
        reason: String,
    },
    /// Контекст разветвлён (создана новая ветка).
    ContextBranched {
        branch_name: String,
        source_branch: String,
    },
    /// Информация о модели при запуске.
    ModelInfo {
        model_name: String,
    },
    /// Прогресс исполнения Luck-плана (узел или фаза).
    /// status: "start" | "ok" | "fail" | "reject" | "done"
    PlanProgress {
        node: String,
        status: String,
    },
    /// Текущая активность агента (живой статус: модель/тул/компакция).
    AgentActivity {
        text: String,
    },
}

// ---------------------------------------------------------------------------
// ClientCommand — команды от фронтенда к агенту
// ---------------------------------------------------------------------------

/// Команды, полученные от фронтенда через WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientCommand {
    /// Ответ пользователя на запрос safety-подтверждения.
    SafetyResponse {
        approved: bool,
    },
    /// Запустить новую задачу.
    StartTask {
        prompt: String,
    },
    /// Переключиться на ветку контекста.
    SwitchBranch {
        name: String,
    },
    /// Запустить Luck-план из файла (путь относительно рабочей директории).
    StartPlan {
        path: String,
    },
    /// Прервать текущий запрос к агенту (аналог Ctrl+C в TUI).
    Abort,
}

// ---------------------------------------------------------------------------
// FrontendNotifierHook — PostToolHook, отправляющий события в broadcast
// ---------------------------------------------------------------------------

/// PostToolHook, который пушит события выполнения тулов в broadcast-канал
/// для последующей рассылки через WebSocket.
pub struct FrontendNotifierHook {
    tx: broadcast::Sender<FrontendEvent>,
}

impl FrontendNotifierHook {
    pub fn new(tx: broadcast::Sender<FrontendEvent>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl super::super::hooks::PostToolHook for FrontendNotifierHook {
    async fn on_post_use(
        &self,
        call: &ToolCall,
        result: &Result<String, String>,
        _context: &[Message],
    ) {
        let event = match result {
            Ok(text) => FrontendEvent::ToolResult {
                tool_name: call.function.name.clone(),
                result: text.clone(),
            },
            Err(e) => FrontendEvent::ToolResult {
                tool_name: call.function.name.clone(),
                result: format!("Error: {e}"),
            },
        };
        let _ = self.tx.send(event);
    }
}

// ---------------------------------------------------------------------------
// WebSocket handler
// ---------------------------------------------------------------------------

/// Состояние сервера.
#[derive(Clone)]
struct AppState {
    tx: broadcast::Sender<FrontendEvent>,
    cmd_tx: mpsc::Sender<ClientCommand>,
    safety_tx: mpsc::Sender<ClientCommand>,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Обслуживает одно WebSocket-соединение: двунаправленный обмен.
///
/// - broadcast → клиент: все `FrontendEvent` отправляются как JSON.
/// - клиент → mpsc: `SafetyResponse` → safety_tx, остальное → cmd_tx.
async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.tx.subscribe();
    let cmd_tx = state.cmd_tx;
    let safety_tx = state.safety_tx;

    // Heartbeat: держит WebSocket живым во время долгих LLM-инференсов.
    let heartbeat_tx = state.tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.tick().await; // skip first immediate tick
        loop {
            interval.tick().await;
            if heartbeat_tx.send(FrontendEvent::Ping).is_err() {
                break;
            }
        }
    });

    loop {
        tokio::select! {
            // broadcast → клиент
            event = rx.recv() => {
                match event {
                    Ok(event) => {
                        let json = match serde_json::to_string(&event) {
                            Ok(j) => j,
                            Err(_) => continue,
                        };
                        if sender.send(WsMessage::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Frontend WS lagged by {n} events");
                        continue;
                    }
                }
            }
            // клиент → mpsc (раздельная маршрутизация)
            msg = receiver.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        if let Ok(cmd) = serde_json::from_str::<ClientCommand>(&text) {
                            match &cmd {
                                ClientCommand::SafetyResponse { .. } => {
                                    let _ = safety_tx.send(cmd).await;
                                }
                                _ => {
                                    let _ = cmd_tx.send(cmd).await;
                                }
                            }
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Err(e)) => {
                        tracing::warn!("Frontend WS receive error: {e}");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Запуск сервера
// ---------------------------------------------------------------------------

/// Пытается освободить порт, убивая процесс, который его слушает.
///
/// Сначала SIGTERM (graceful shutdown), затем SIGKILL через 500 мс, если процесс жив.
/// Поддерживает Linux (fuser → lsof → ss). На других платформах — no-op.
/// Возвращает true, если порт удалось освободить (или не был занят).
fn free_port_local(port: u16) -> bool {
    use std::process::Command;
    use std::time::Duration;

    /// Найти PID процесса, слушающего TCP-порт.
    fn find_pid(port: u16) -> Option<i32> {
        // fuser - самый быстрый и точный (есть в psmisc, почти везде)
        let out = Command::new("fuser")
            .arg(format!("{port}/tcp"))
            .output()
            .ok()?;
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            return stdout
                .split_whitespace()
                .last()
                .and_then(|s| s.parse::<i32>().ok());
        }

        // lsof -ti :PORT (fallback 1)
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("lsof -ti :{port} 2>/dev/null"))
            .output()
            .ok()?;
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            return stdout.trim().split('\n').last().and_then(|s| s.parse::<i32>().ok());
        }

        // ss -tlnp (fallback 2)
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("ss -tlnp sport = :{port} 2>/dev/null | grep -oP 'pid=\\K\\d+'"))
            .output()
            .ok()?;
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            return stdout.trim().split('\n').last().and_then(|s| s.parse::<i32>().ok());
        }

        None
    }

    fn is_alive(pid: i32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    let pid = match find_pid(port) {
        Some(pid) => pid,
        None => return true, // порт свободен
    };

    tracing::warn!("Port {port} is occupied by PID {pid}, sending SIGTERM...");

    let _ = Command::new("kill")
        .arg(pid.to_string())
        .status();

    // Даём время на graceful shutdown
    std::thread::sleep(Duration::from_millis(500));

    if is_alive(pid) {
        tracing::warn!("PID {pid} still alive, sending SIGKILL");
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status();
        std::thread::sleep(Duration::from_millis(100));
    }

    !is_alive(pid)
}

/// Запускает WebSocket-сервер + статику на `0.0.0.0:8080`.
///
/// Возвращает:
/// - `broadcast::Sender<FrontendEvent>` — публикация событий
/// - `watch::Sender<bool>` — graceful shutdown
/// - `mpsc::Receiver<ClientCommand>` — команды задач (StartTask, SwitchBranch) от фронтенда
/// - `mpsc::Receiver<ClientCommand>` — safety-ответы (SafetyResponse) от фронтенда
/// Только каналы событий/команд, без HTTP-сервера — для TUI/headless-режимов.
/// Не занимает порт, не убивает процессы.
pub fn frontend_channels() -> (
    broadcast::Sender<FrontendEvent>,
    watch::Sender<bool>,
    mpsc::Receiver<ClientCommand>,
    mpsc::Receiver<ClientCommand>,
) {
    let (tx, _rx) = broadcast::channel(256);
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let (_cmd_tx, cmd_rx) = mpsc::channel(32);
    let (_safety_tx, safety_rx) = mpsc::channel(32);
    (tx, shutdown_tx, cmd_rx, safety_rx)
}

pub fn start_frontend_server() -> (
    broadcast::Sender<FrontendEvent>,
    watch::Sender<bool>,
    mpsc::Receiver<ClientCommand>,
    mpsc::Receiver<ClientCommand>,
) {
    let (tx, _rx) = broadcast::channel(256);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (cmd_tx, cmd_rx) = mpsc::channel(32);
    let (safety_tx, safety_rx) = mpsc::channel(32);

    let state = AppState {
        tx: tx.clone(),
        cmd_tx,
        safety_tx,
    };

    // Путь к статике: env var AI_AGENT_STATIC_DIR, иначе — compile-time Cargo manifest dir.
    // В Docker задаётся через ENV AI_AGENT_STATIC_DIR=/app/static
    let static_dir = std::env::var("AI_AGENT_STATIC_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static")
        });
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .fallback_service(get_service(
            ServeDir::new(static_dir).append_index_html_on_directories(true),
        ))
        .layer(CorsLayer::permissive())
        .with_state(state);

    tokio::spawn(async move {
        // Освободить порт, если его занял зависший процесс предыдущего запуска
        free_port_local(8080);

        let listener = match tokio::net::TcpListener::bind("0.0.0.0:8080").await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Failed to bind frontend server: {e}");
                return;
            }
        };

        tracing::info!("Frontend server listening on http://127.0.0.1:8080");

        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let mut rx = shutdown_rx;
                rx.changed().await.ok();
            })
            .await
        {
            tracing::error!("Frontend server error: {e}");
        }
    });

    (tx, shutdown_tx, cmd_rx, safety_rx)
}
