# AI Agent

Модульный Rust CLI AI Agent с архитектурой на трейтах. Асинхронный цикл агента с поддержкой тулов, safety-пайплайна, ветвления контекста и MCP-контейнеров.

## Архитектура

```
 LLM (stream_chat)
      │
      ▼
 ┌──────────────────────┐
 │  CredentialRotator   │ ← round-robin по пулу эндпоинтов (AGENT_PROVIDER_POOL)
 └──────────────────────┘
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
  ws://0.0.0.0:8080/ws
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
| 11 | **Credential Rotator** | ✅ | `CredentialRotator` — thread-safe round-robin по пулу эндпоинтов; `AGENT_PROVIDER_POOL` env для конфигурации |

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
| `/swarm` | Запуск параллельных суб-агентов (researcher + summarizer) |
| `/exit` | Выход |
| `Ctrl+C` | Graceful shutdown (snapshot + выход) |

Safety-пайплайн логируется через `tracing` (stderr): `[SAFETY] Tool execution APPROVED / DENIED`.

Авто-компакция контекста: при превышении лимита сообщений (по умолчанию 15) агент вызывает LLM для суммаризации старых сообщений, сохраняя последние 4 нетронутыми.

## Переменные окружения

| Переменная | По умолчанию | Описание |
|-----------|-------------|----------|
| `AI_AGENT_MODEL` | `qwen2.5:3b` | Модель Ollama для использования |
| `AGENT_PROVIDER_POOL` | — | URL-ы эндпоинтов через запятую для round-robin ротации (например, `http://host.docker.internal:11434,http://10.0.0.2:11434`) |
| `OLLAMA_API_KEY` | — | API-ключ для Bearer-аутентификации (Ollama Cloud / OpenAI-совместимые эндпоинты) |
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

Single-stage: копирует локально собранный бинарник (`cargo build --release` → `target/release/ai-agent`) в `debian:bookworm-slim`.

```bash
# Сборка бинарника
cargo build --release

# Сборка образа
docker build -t native-ai-agent .

# Запуск (daemon, проброс docker.sock для MCP)
docker run -d \
  --name ai-agent-core \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -p 8080:8080 \
  -e AGENT_PROVIDER_POOL="http://host.docker.internal:11434" \
  native-ai-agent
```

При связи с внешним Ollama из контейнера — используй `host.docker.internal` (на Linux добавить `--add-host host.docker.internal:host-gateway`) или прямой IP хоста.

## Тесты

```
cargo test --lib
# 54 теста: context (19), safety (14), agent (3), tool_routing (3), hooks (2), platform (12), orchestrator (1)
```

## Лицензия

MIT
