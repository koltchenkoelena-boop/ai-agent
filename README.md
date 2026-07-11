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
 │  ToolRouter      │ → Platform (встроенные) / Frontend (UI) / MCP (Docker контейнеры)
 └─────────────────┘
      │
 ┌──────────────────┐
 │  PostToolHook[]   │ ← fire-and-forget (логирование, метрики)
 └──────────────────┘
      │
      ▼
 ContextManager ← ветвление (git-like branching) + авто-компакция
```

## Компоненты

| # | Компонент | Статус | Описание |
|---|-----------|--------|----------|
| 1 | **Agent Loop** | ✅ | `Agent<P>::run_step()` — LLM → Safety → Hooks → Tool Router → Context |
| 2 | **Context Manager** | ✅ | Git-like branching: create, switch, merge (Overwrite/FastForward/Union), snapshot, авто-компакция |
| 3 | **MCP Transport** | ✅ | Docker exec + JSON-RPC 2.0: initialize → tools/list → tools/call |
| 4 | **Tool Routing** | ✅ | ToolRouter, AsyncTool trait, ToolKind (Platform/Frontend/Mcp) |
| 5 | **Safety Pipeline** | ✅ | 5 stages: Security → Egress → Adversary → Permission → Repetition |
| 6 | **Hooks** | ✅ | PreToolUse (блокирующий) + PostToolUse (fire-and-forget через tokio::spawn) |
| 7 | **Auto-compaction** | ✅ | CompactionConfig, needs_compaction, compact, скрытый LLM вызов |

## Быстрый старт

```bash
# Сборка
cargo build --release

# Запуск (требуется локальный Ollama)
./target/release/ai-agent

# Тесты
cargo test --lib
```

## Зависимости

tokio, async-trait, futures-util, tokio-util, reqwest, async-stream, serde, serde_json, thiserror, uuid, chrono, tracing, tracing-subscriber

## MCP Контейнеры

Опционально: создать `mcp_containers.json` в корне проекта:

```json
[
  { "container_id": "my-mcp-server", "command": ["docker", "exec", "-i", "my-mcp-server", "mcp"] }
]
```

## Тесты

```
cargo test --lib
# 42 теста: context (19), safety (14), agent (3), tool_routing (3), hooks (2)
```

## Лицензия

MIT
