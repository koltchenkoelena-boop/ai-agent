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

/// Запускает WebSocket-сервер + статику на `0.0.0.0:8080`.
///
/// Возвращает:
/// - `broadcast::Sender<FrontendEvent>` — публикация событий
/// - `watch::Sender<bool>` — graceful shutdown
/// - `mpsc::Receiver<ClientCommand>` — команды задач (StartTask, SwitchBranch) от фронтенда
/// - `mpsc::Receiver<ClientCommand>` — safety-ответы (SafetyResponse) от фронтенда
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

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .fallback_service(get_service(ServeDir::new("static")))
        .layer(CorsLayer::permissive())
        .with_state(state);

    tokio::spawn(async move {
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
