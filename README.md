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
  | ContextBranched | ModelInfo | PlanProgress
  | AgentActivity | Ping (heartbeat, 30s)
  ClientCommand: StartTask | SwitchBranch
  | StartPlan | SafetyResponse | Abort
═══════════════════════════════════════════

═══════════════════════════════════════════
  🔁  Context Overflow (413) Auto-Retry
  stream_chat → match ApiError(413)
  → trim_context_for_retry() (удаляет пару
  tool_call + tool_result) → retry 1x
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
| 12 | **413 Auto-Retry** | ✅ | Перехват `ProviderError::ApiError(413)` — удаление старейшей tool_call/tool_result пары + retry |
| 13 | **TUI** | ✅ | `ratatui`-интерфейс (`--tui`): слэш-команды, живой статус, markdown, Luck-планы (см. раздел ниже) |
| 14 | **Luck-планы** | ✅ | `luck_compile` → `luck_plan` (валидация DAG/контрактов VERIFY/POLICY) → `luck_scheduler` (исполнение с прогрессом) |
| 15 | **Memory (RAG)** | ✅ | `memory::vector_db` — векторное хранилище на `sled` для семантического поиска по контексту |

## Быстрый старт

```bash
# Сборка
cargo build --release

# Запуск (см. run.sh — выбор провайдера)
./run.sh local        # локальный Ollama (по умолчанию, http://localhost:11434)
./run.sh tailscale    # сервер niceguy через Tailscale
./run.sh openrouter   # облачные модели через OpenRouter
./run.sh cloud        # Ollama Cloud (ollama.com, нужен OLLAMA_CLOUD_API_KEY)

# Короткий алиас (указан в ~/.bashrc)
alias aa='~/workspace/ai-agent/run.sh'

# Тесты
cargo test --lib
```

## Режимы провайдеров

| Режим | Команда | Провайдер | Аутентификация |
|-------|---------|-----------|----------------|
| Локальный | `./run.sh local` | `http://localhost:11434` (Ollama) | — |
| Tailscale | `./run.sh tailscale` | `https://niceguy-1.tail349e77.ts.net/ollama` | `OLLAMA_API_KEY=ollama` |
| OpenRouter | `./run.sh openrouter` | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |
| Ollama Cloud | `./run.sh cloud` | `https://ollama.com` | `OLLAMA_CLOUD_API_KEY` |

При запуске через `run.sh` переменные окружения выставляются автоматически.
Без `run.sh` можно задать пул провайдеров вручную через `AGENT_PROVIDER_POOL` (см. Credential Rotator).

### Ollama Cloud (Native)

Режим `./run.sh cloud` встраивает прокси с API-ключом прямо в бинарник — внешний Python-прокси не нужен.

- Отправляет запрос напрямую на `https://ollama.com/api/chat` с Bearer-аутентификацией
- Парсит NDJSON-ответ Ollama (в отличие от OpenAI SSE)
- Модель по умолчанию: `nemotron-3-super:cloud`
- Ключ задаётся через `OLLAMA_CLOUD_API_KEY`:

```bash
export OLLAMA_CLOUD_API_KEY="<ваш ключ Ollama Cloud>"
./run.sh cloud
```

Внутренняя реализация: `ProviderKind::OllamaChat` — новый вариант в `ProviderConfig`, который
маршрутизируется в `FallbackProvider::stream_chat()` на `/api/chat` + NDJSON-парсер вместо
стандартного OpenAI `/v1/chat/completions` + SSE-парсера.

## Chat Logging (NDJSON)

При каждом запуске агент создаёт файл лога в директории `chat_logs/`:

```
chat_logs/2026-07-14_00-27-58.jsonl
```

Формат — NDJSON (JSON Lines), одна строка = одно событие пайплайна.
Все стадии жизненного цикла записываются с метриками:

| Событие | stage | Описание |
|---------|-------|----------|
| `run_iteration` | — | Начало итерации агента |
| `llm_call` | — | Отправка запроса провайдеру (кол-во сообщений, оценка токенов) |
| `llm_response` | — | Получен ответ от LLM (TTFB, latency, кол-во вызовов тулов) |
| `safety` | security/egress/adversary/permission/repetition | Результат проверки safety-эшелона |
| `pre_tool_hook` | — | Вызов pre-tool хука |
| `tool_exec` | — | Начало выполнения тула (имя, аргументы) |
| `tool_result` | — | Результат выполнения тула (latency, успех/ошибка) |
| `context_push` | — | Добавление сообщения в контекст (роль, оценка токенов) |
| `decision` | — | Решение агента (action: continue / wait_for_confirmation / max_steps_reached) |
| `compaction_check` | — | Проверка необходимости авто-компакции |
| `compaction` | — | Выполнена авто-компакция контекста |
| `run_complete` | — | Завершение цикла агента (шагов, ошибок, длительность) |

Пример строки лога:

```json
{"timestamp":"2026-07-14T00:27:58.123456+03:00","level":"INFO","fields":{"stage":"llm_call","step":1,"msg_count":5,"token_estimate":3840},"target":"ai_agent::agent","span":{"name":"run_step","step":1}}
```

Логи пишутся через `tracing` с кастомным `MakeWriter` в два слоя:
- **stderr** — human-readable формат (цветной, с уровнями)
- **JSON Lines** — машинно-читаемый файл для анализа и отладки

## TUI (полноэкранный интерфейс)

```bash
cargo run -- --tui
# или через run.sh:
./run.sh local --tui
```

Полноэкранный интерфейс на `ratatui` в стиле grok-build. Особенности:

- **Слэш-команды**: `/help`, `/tools`, `/branch [name]`, `/clear`, `/plan <file.luck>`, `/quit`
- **Горячие клавиши**: Enter — отправить, Esc / одиночный Ctrl+C — прервать текущий запрос, двойной Ctrl+C (< 1.5с) — выход, ↑/↓ — история ввода, PageUp/PageDown — скролл ленты
- **Живой статус агента** в статус-баре: `◆ думает… Ns · шаг N: …` (текущий тул, ожидание LLM, компакция контекста) — обновляется в реальном времени через `FrontendEvent::AgentActivity`
- **Markdown-рендер** ответов (заголовки, `**bold**`, `*italic*`, `` `code` ``, ```` ``` ```` fences) с переносом строк по display-width (корректно для кириллицы/emoji)
- **Luck-планы**: `/plan <file>` компилирует и исполняет `.luck`-план с прогрессом по узлам (см. [docs/luck-integration.md](docs/luck-integration.md))
- Ошибка тула не роняет сессию — попадает в контекст как результат тула, агент продолжает работу

### Автоматизированное тестирование TUI (tmux-драйвер)

Раздел для CI/само-тестирования агента — не требует ручного взаимодействия:

```bash
python3 scripts/tui_driver.py start          # поднять TUI в tmux-сессии ai-tui-test
python3 scripts/tui_driver.py ask "текст"    # ввод + ожидание ответа, печатает диалог
python3 scripts/tui_driver.py read           # прочитать текущий экран
python3 scripts/tui_driver.py stop           # Ctrl+C + kill сессии
python3 scripts/tui_driver.py status         # жива ли сессия

# Батч-прогон с рестартом сессии каждые N вопросов (для длинных серий)
python3 scripts/batch_ask.py --questions "q1|q2|q3" --restart-every 10

# Живой просмотр
tmux attach -t ai-tui-test   # отсоединиться: Ctrl+B, D
```

### Цикл самосовершенствования

`scripts/self_improve.py` — прогоняет сценарий вопросов через TUI-драйвер, анализирует ответы
(пустой ответ / ошибка провайдера / повтор / слишком долго / отсутствие ключевого слова) и
пишет отчёт в `out/self_improve_<ts>.json` (exit 0 — ок, exit 1 — есть проблемы):

```bash
python3 scripts/self_improve.py
python3 scripts/self_improve.py --questions "привет|найди TODO через grep" --target "привет|TODO"

# Многократный прогон со сбором статистики (для регрессионного тестирования):
scripts/loop_stats.sh 100   # пишет out/loop_stats_<ts>.jsonl + .log
```

Подробный цикл «прогон → анализ → правка → повтор» описан в `CLAUDE.md`.

## Интерактивный CLI

При запуске `ai-agent` (без `--tui`) открывается диалоговый цикл в терминале — те же команды,
что и в TUI, плюс запуск на порту 8080 веб-фронтенда (см. ниже). Все сообщения отправляются LLM.
Встроенные команды:

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

**413 Context Overflow**: при ответе провайдера `API Error (Status 413)` агент автоматически удаляет старейшую пару (tool_call + tool_result) из контекста и повторяет запрос. Однократный retry.

## Веб-фронтенд

Тот же CLI-цикл (без `--tui`) поднимает на порту 8080 WebSocket-сервер (`axum`) и отдаёт статику
из `static/index.html` — функционал приближён к TUI:

- **Живой статус агента** в шапке — тот же `AgentActivity`, что и в TUI ("шаг N: модель думает…")
- **Markdown-рендер** ответов ассистента (заголовки, `**bold**`, `` `code` ``, fences)
- **Слэш-команды** в поле ввода: `/help`, `/tools`, `/clear` (только в браузере), `/branch <name>`
  (переключение веток контекста прямо из веб-UI)
- **Кнопка Stop** — прерывает текущий запрос к агенту (`ClientCommand::Abort`), аналог Ctrl+C в TUI;
  запуск агента выполняется в отменяемом tokio-таске
- Бейдж текущей ветки контекста в шапке, дерево веток и safety-review модалка в сайдбаре
- Ошибка провайдера не роняет сервер — показывается в UI, сессия продолжает работать

Не перенесено из TUI: `/swarm` (параллельные суб-агенты) — требует отдельных WS-событий для
отображения нескольких параллельных агентов, пока доступно только в терминальном CLI.

## Переменные окружения

| Переменная | По умолчанию | Описание |
|-----------|-------------|----------|
| `AI_AGENT_MODEL` | `qwen2.5:3b` | Модель Ollama для использования |
| `AGENT_PROVIDER_POOL` | — | URL-ы эндпоинтов через запятую для round-robin ротации (например, `http://host.docker.internal:11434,http://10.0.0.2:11434`) |
| `OLLAMA_API_KEY` | — | API-ключ для Bearer-аутентификации (Ollama Cloud / OpenAI-совместимые эндпоинты) |
| `OLLAMA_CLOUD_API_KEY` | — | API-ключ Ollama Cloud (режим `cloud`, Bearer auth на `https://ollama.com/api/chat`) |
| `OLLAMA_CLOUD_BASE_URL` | `https://ollama.com` | Базовый URL для Ollama Cloud API |
| `RUST_LOG` | `info` | Уровень логирования (debug, info, warn, error) |
| `OPENROUTER_API_KEY` | — | API-ключ OpenRouter (требуется в режиме openrouter) |

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
# 104 теста: context (23), tool_routing::platform (18), safety (14), luck_plan (10),
# luck_compile (8), tui (7), memory::vector_db (7), luck_scheduler (5), agent (4),
# tool_routing (3), provider (2), hooks (2), orchestrator (1)
```

## Документация

- [docs/luck-integration.md](docs/luck-integration.md) — интеграция Luck-планов (compile → schedule → execute)
- [docs/provider-evolution.md](docs/provider-evolution.md) — история эволюции провайдер-слоя (CredentialRotator, Bearer-auth, run.sh)
- `CLAUDE.md` — инструкции для Claude Code: цикл самосовершенствования, команды драйвера TUI, правила правок

## Лицензия

MIT
