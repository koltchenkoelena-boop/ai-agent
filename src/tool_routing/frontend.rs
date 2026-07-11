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
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use tower_http::cors::CorsLayer;

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

/// Состояние сервера — broadcast-отправитель для рассылки событий.
#[derive(Clone)]
struct AppState {
    tx: broadcast::Sender<FrontendEvent>,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state.tx))
}

/// Обслуживает одно WebSocket-соединение: подписывается на broadcast
/// и пересылает все события клиенту в виде JSON.
async fn handle_socket(socket: WebSocket, tx: broadcast::Sender<FrontendEvent>) {
    let (mut sender, _receiver) = socket.split();
    let mut rx = Arc::new(tx).subscribe();

    loop {
        match rx.recv().await {
            Ok(event) => {
                let json = match serde_json::to_string(&event) {
                    Ok(j) => j,
                    Err(_) => continue,
                };
                if sender.send(WsMessage::Text(json.into())).await.is_err() {
                    // Клиент отключился
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
}

// ---------------------------------------------------------------------------
// Запуск сервера
// ---------------------------------------------------------------------------

/// Запускает WebSocket-сервер на `127.0.0.1:8080`.
///
/// Возвращает:
/// - `broadcast::Sender<FrontendEvent>` — для публикации событий
/// - `watch::Sender<bool>` — отправка `true` для graceful shutdown сервера
pub fn start_frontend_server(
) -> (broadcast::Sender<FrontendEvent>, watch::Sender<bool>) {
    let (tx, _rx) = broadcast::channel(256);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let state = AppState { tx: tx.clone() };

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:8080").await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Failed to bind frontend server: {e}");
                return;
            }
        };

        tracing::info!("Frontend WebSocket server listening on ws://127.0.0.1:8080/ws");

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

    (tx, shutdown_tx)
}
