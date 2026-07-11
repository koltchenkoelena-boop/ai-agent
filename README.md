# AI Agent

Модульный Rust CLI AI Agent с архитектурой на трейтах. Асинхронный цикл агента с поддержкой тулов, safety-пайплайна, ветвления контекста и MCP-контейнеров.

## Архитектура

```
 LLM (stream_chat)
      │
      ▼
 StreamAccumulator
      │
      ▼
 ┌─────────────────┐
 │  Safety Pipeline │ ← 5 эшелонов (Security, Egress, Adversary, Permission, Repetition)
 └─────────────────┘
      │
 ┌─────────────────┐
 │  PreToolHook[]   │ ← блокирующие хуки (модификация/отмена вызова)
 └─────────────────┘
      │
 ┌─────────────────┐
 │  ToolRouter      │ → Platform (встроенные) / Frontend (WS) / MCP (Docker)
 └─────────────────┘
      │
 ┌──────────────────┐
 │  PostToolHook[]   │ ← fire-and-forget (логирование, метрики, WS-трансляция)
 └──────────────────┘
      │
      ▼
 ┌──────────────────┐
 │  ContextManager   │ ← ветвление (git-like branching) + авто-компакция
 └──────────────────┘
      │
      ▼
 ┌──────────────────────────────────────────┐
 │  Graceful Shutdown                        │
 │  Ctrl+C → snapshot → history_dump.json   │
 └──────────────────────────────────────────┘

═══════════════════════════════════════════
  ⚡ Parallel Execution (Orchestrator):
  AgentCluster::execute_parallel_tasks
    → N суб-агентов (join_all)
    → ветки task-0..N
    → MergeStrategy::Union
═══════════════════════════════════════════

═══════════════════════════════════════════
  🖥  Frontend WebSocket Server
  ws://127.0.0.1:8080/ws
  FrontendEvent: AgentMessage | ToolExecuting
  | ToolResult | SafetyReviewRequired
  | ContextBranched
═══════════════════════════════════════════
```

## Компоненты

| # | Компонент | Статус | Описание |
|---|-----------|--------|----------|
| 1 | **Agent Loop** | ✅ | `Agent<P>::run_step()` — LLM → Safety → Hooks → Tool Router → Context |
| 2 | **Context Manager** | ✅ | Git-like branching: create, switch, merge (Overwrite/FastForward/Union), snapshot, авто-компакция |
| 3 | **MCP Transport** | ✅ | Docker exec + JSON-RPC 2.0: initialize → tools/list → tools/call |
| 4 | **Tool Routing** | ✅ | ToolRouter, AsyncTool trait, ToolKind (Platform/Frontend/Mcp) |
| 4.1 | **Platform Tools** | ✅ | read_file, write_file, glob, grep — нативные async инструменты для работы с ФС |
| 5 | **Safety Pipeline** | ✅ | 5 stages: Security → Egress → Adversary → Permission → Repetition |
| 6 | **Hooks** | ✅ | PreToolUse (блокирующий) + PostToolUse (fire-and-forget через tokio::spawn) |
| 7 | **Auto-compaction** | ✅ | CompactionConfig, needs_compaction, compact, скрытый LLM вызов |
| 8 | **Frontend WS Server** | ✅ | axum WebSocket на 127.0.0.1:8080/ws, трансляция событий в JSON (FrontendEvent) |
| 9 | **Orchestrator** | ✅ | `AgentCluster::execute_parallel_tasks()` — N суб-агентов через join_all, ветвление + MergeStrategy::Union |
| 10 | **Graceful Shutdown** | ✅ | Ctrl+C → snapshot всех веток → history_dump.json → остановка фронтенд-сервера |

## Быстрый старт

```bash
# Сборка
cargo build --release

# Запуск (требуется локальный Ollama)
./target/release/ai-agent

# Тесты
cargo test --lib
```

## Интерактивный CLI

При запуске `ai-agent` открывается диалоговый цикл. Все сообщения отправляются LLM. Встроенные команды:

| Команда | Описание |
|---------|----------|
| `/help` | Список команд |
| `/branch` | Показать ветки контекста |
| `/switch <name>` | Переключиться на ветку |
| `/rename <name>` | Переименовать текущую ветку |
| `/tools` | Список зарегистрированных инструментов |
| `/snapshot` | Снапшот всех веток |
| `/exit` | Выход |
| `Ctrl+C` | Graceful shutdown (snapshot + выход) |

Safety-пайплайн логируется через `tracing` (stderr): `[SAFETY] Tool execution APPROVED / DENIED`.

Авто-компакция контекста: при превышении лимита сообщений (по умолчанию 15) агент вызывает LLM для суммаризации старых сообщений, сохраняя последние 4 нетронутыми.

## Переменные окружения

| Переменная | По умолчанию | Описание |
|-----------|-------------|----------|
| `AI_AGENT_MODEL` | `qwen2.5:3b` | Модель Ollama для использования |
| `RUST_LOG` | `info` | Уровень логирования (debug, info, warn, error) |

## Зависимости

tokio, async-trait, futures-util, tokio-util, reqwest, async-stream, serde, serde_json, axum, tower-http, thiserror, uuid, chrono, tracing, tracing-subscriber

## MCP Контейнеры

Опционально: создать `mcp_containers.json` в корне проекта:

```json
[
  { "container_id": "my-mcp-server", "command": ["docker", "exec", "-i", "my-mcp-server", "mcp"] }
]
```

## Docker

Многоступенчатая сборка: `rust:1.80-slim` (builder) → `debian:bookworm-slim` (runtime).

```bash
# Сборка образа
docker build -t ai-agent .

# Запуск (интерактивный)
docker run -it --rm \
  -e AI_AGENT_MODEL=qwen2.5:3b \
  -p 8080:8080 \
  ai-agent

# Запуск с MCP-контейнерами (требуется docker.sock)
docker run -it --rm \
  -e AI_AGENT_MODEL=qwen2.5:3b \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -p 8080:8080 \
  ai-agent
```

При связи с внешним Ollama — передай `http://host.docker.internal:11434` или IP хоста.

## Тесты

```
cargo test --lib
# 53 теста: context (19), safety (14), agent (3), tool_routing (3), hooks (2), platform (12)
```

## Лицензия

MIT
