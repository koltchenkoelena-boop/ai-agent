# Создание ai-agent — полный лог сессии

**Дата:** 2026-07-11  
**Проект:** `~/workspace/ai-agent/`  
**Участники:** Андрей (user) / Qwen Code (assistant)

---

## 1. Запрос: создать AI-агента с нуля

**Андрей:** Нужен модульный Rust CLI AI Agent. Архитектура на трейтах. Асинхронный цикл с поддержкой тулов, safety-пайплайна, ветвления контекста и MCP-контейнеров.

**Qwen Code:** Создаю проект `cargo new ai-agent`. Архитектура:

```
LLM (stream_chat) → StreamAccumulator → Safety Pipeline (5 эшелонов)
→ PreToolHook[] → ToolRouter (Platform/Frontend/MCP) → PostToolHook[]
→ ContextManager (git-like branching) → Graceful Shutdown
```

Компоненты:
1. Agent Loop — `Agent<P>::run_step()`
2. Context Manager — ветвление, merge (Overwrite/FastForward/Union), авто-компакция
3. MCP Transport — Docker exec + JSON-RPC 2.0
4. Tool Routing — ToolRouter, AsyncTool trait
5. Safety Pipeline — 5 stages (Security → Egress → Adversary → Permission → Repetition)
6. Hooks — PreToolUse (блокирующий) + PostToolUse (fire-and-forget)
7. Auto-compaction — CompactionConfig, needs_compaction, compact
8. Frontend WS Server — axum на 127.0.0.1:8080/ws
9. Orchestrator — AgentCluster с parallel join_all
10. Graceful Shutdown — Ctrl+C → snapshot → history_dump.json

---

## 2. Запрос: CredentialRotator с ротацией URL

**Андрей:** Нужна поддержка нескольких эндпоинтов, round-robin между ними. Если переменная окружения `AGENT_PROVIDER_POOL` не задана — fallback на `OLLAMA_HOST` или localhost:11434.

**Qwen Code:** Реализовал `CredentialRotator`:

```rust
pub struct CredentialRotator {
    endpoints: Vec<String>,
    counter: Arc<AtomicUsize>,
}
```

- Thread-safe round-robin через `AtomicUsize` (Relaxed ordering)
- `get_next()` на каждый `stream_chat`
- Инициализация из `AGENT_PROVIDER_POOL` (через запятую)
- В `main.rs` три ветки: пул пуст → local(), 1 endpoint → без ротатора, N endpoints → с ротатором

---

## 3. Запрос: Ollama API Key для облачных эндпоинтов

**Андрей:** Добавь поддержку `OLLAMA_API_KEY` для Bearer-аутентификации.

**Qwen Code:** Добавлено:

```rust
let mut headers = HeaderMap::new();
if let Some(ref key) = self.api_key {
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", key)).unwrap(),
    );
}
```

Bearer-токен передаётся в каждом запросе к `/v1/chat/completions`.

---

## 4. Запрос: 413 Context Overflow — авто-retry

**Андрей:** Провайдер иногда отвечает 413 (контекст переполнен). Нужен авто-retry: удалить старейшую пару tool_call + tool_result и повторить запрос.

**Qwen Code:** Реализовал `trim_context_for_retry()`:

```rust
fn trim_context_for_retry(&mut self) {
    // Ищем первую Tool-сообщение и предшествующий Assistant с tool_calls
    let tool_result_idx = msgs.iter().position(|m| m.role == Role::Tool);
    // Удаляем пару [Assistant(tool_calls), Tool(result)]
    self.context.remove_range(remove_start, remove_end);
}
```

В `run_step` перехват 413:

```rust
Err(ProviderError::ApiError { status: 413, .. }) => {
    tracing::warn!("Context overflow (413) — trimming and retrying");
    self.trim_context_for_retry();
    // повтор stream_chat
}
```

Однократный retry. Если второй раз 413 — пробрасывается наверх.

---

## 5. Запрос: run.sh — три режима запуска

**Андрей:** Сделай скрипт запуска с выбором провайдера.

**Qwen Code:** Создал `run.sh`:

```bash
./run.sh local       # unset AGENT_PROVIDER_POOL → localhost:11434
./run.sh tailscale   # AGENT_PROVIDER_POOL=niceguy-1.tail349e77.ts.net/ollama
./run.sh openrouter  # AGENT_PROVIDER_POOL=openrouter.ai/api/v1
```

Режим `local` — дефолтный, намеренно сбрасывает `AGENT_PROVIDER_POOL`.

---

## 6. Запрос: 413 — трёхшаговая стратегия

**Андрей:** Системный архитектор выдал спецификацию: три шага против 413.

**Qwen Code:** Реализовал:

- **Step A** — GlobTool/GrepTool обрезают вывод до 8KB с маркером `[Output truncated at 8192 bytes]`
- **Step B** — `run_step` передаёт тулы условно: только когда последнее сообщение не assistant без tool_calls
- **Step C** — `ContextManager::estimate_tokens()` (сумма длин контента / 4) + proactive compaction при >6000 токенов до `stream_chat`

65 тестов (было 54), все проходят. Закоммичено и запушено.

---

## 7. Запрос: пояснительная записка про эволюцию провайдера

**Андрей:** Составь заметку — как работал CredentialRotator в начальном варианте и почему ушли на локальный демон.

**Qwen Code:** Исследовал git-историю и код. Результаты:

- `09:58` — initial commit, `OllamaProvider` с `base_url`
- `17:52` — `CredentialRotator` + `AGENT_PROVIDER_POOL` (multi-endpoint в Docker)
- `19:34` — `OLLAMA_API_KEY` (Bearer для Ollama Cloud)
- `21:10` — `run.sh` с тремя режимами

Причина: изначальный use case (несколько Ollama в Docker Swarm) не реализовался, перешли на локальный demon + удалёнка Tailscale + OpenRouter.

---

## 8. Текущий запрос: сохранить этот лог

**Андрей:** Сохрани весь текст этой сессии в файл.

**Qwen Code:** Создаю `~/workspace/chat-create_ai-agent__begin.md`.

---

## Итоги сессии

- Создан проект `ai-agent` на Rust (cargo)
- Реализованы все 10 компонентов архитектуры
- Реализован `CredentialRotator` (round-robin URL pool)
- Реализован авто-retry 413 (контекст оверфлоу)
- Реализован `run.sh` с тремя режимами
- Реализована трёхшаговая стратегия против 413 (обрезка тулов, условные схемы, proactive compaction)
- **65 тестов, все проходят**
- Закоммичено в `main` на github.com/koltchenkoelena-boop/ai-agent.git

**Ключевые файлы:**
- `src/provider.rs` — `ModelProvider` trait, `OllamaProvider`, `CredentialRotator`
- `src/agent.rs` — `Agent<P>`, `StreamAccumulator`, 413 retry, proactive compaction
- `src/context.rs` — `ContextManager`, branching, merge, `estimate_tokens()`
- `src/tool_routing/platform.rs` — `ReadFileTool`, `WriteFileTool`, `GlobTool`, `GrepTool` (с обрезкой 8KB)
- `src/safety.rs` — 5-stage safety pipeline
- `src/types.rs` — `Message`, `ChatChunk`, `ToolCall`, `ToolDefinition`
- `run.sh` — launcher local/tailscale/openrouter
