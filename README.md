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
| 4.1 | **Platform Tools** | ✅ | read_file, write_file, glob, grep — нативные async инструменты для работы с ФС |
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

Safety-пайплайн логируется через `tracing` (stderr): `[SAFETY] Tool execution APPROVED / DENIED`.

Авто-компакция контекста: при превышении лимита сообщений (по умолчанию 15) агент вызывает LLM для суммаризации старых сообщений, сохраняя последние 4 нетронутыми.

## Переменные окружения

| Переменная | По умолчанию | Описание |
|-----------|-------------|----------|
| `AI_AGENT_MODEL` | `qwen2.5:3b` | Модель Ollama для использования |
| `RUST_LOG` | `info` | Уровень логирования (debug, info, warn, error) |

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
# 53 теста: context (19), safety (14), agent (3), tool_routing (3), hooks (2), platform (12)
```

## Лицензия

MIT
