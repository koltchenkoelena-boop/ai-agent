# Эволюция провайдера в ai-agent

**Дата:** 2026-07-11  
**Проект:** `ai-agent/`  
**Источник:** сессия создания агента

---

Хронология эволюции `ModelProvider` от простого локального клиента до мульти-эндпоинтной системы с Bearer-аутентификацией, round-robin'ом и авто-retry.

---

## Этап 1. Initial commit (09:58 UTC+7)

```txt
fc4aa6b — Initial commit: modular Rust AI Agent with 7 components
```

`OllamaProvider` с единственным `base_url`, читаемым из `OLLAMA_HOST` или дефолт `http://localhost:11434`. Один эндпоинт, без аутентификации, без ротации.

---

## Этап 2. CredentialRotator + AGENT_PROVIDER_POOL (17:52)

```txt
cd2ed8b — feat: credential rotator, AGENT_PROVIDER_POOL, Docker deploy readiness
```

Добавлен `CredentialRotator`:

```rust
pub struct CredentialRotator {
    endpoints: Vec<String>,
    counter: Arc<AtomicUsize>,
}
```

- Thread-safe round-robin через `AtomicUsize` (Relaxed ordering)
- `get_next()` вызывается перед каждым `stream_chat`
- Инициализация из `AGENT_PROVIDER_POOL` (через запятую)
- Fallback: пул пуст → `OLLAMA_HOST` → `http://localhost:11434`

**Мотивация:** несколько инстансов Ollama в Docker Swarm — балансировка между ними.

---

## Этап 3. OLLAMA_API_KEY (19:34)

```txt
c40d024 — Add OLLAMA_API_KEY Bearer auth for Ollama Cloud / Nemotron-3-Super
```

Добавлен Bearer-токен во все исходящие запросы:

```rust
let mut headers = HeaderMap::new();
if let Some(ref key) = self.api_key {
    headers.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", key)).unwrap(),
    );
}
```

При каждом запросе к `/v1/chat/completions`.

**Мотивация:** Ollama Cloud и OpenRouter требуют Bearer-аутентификацию.

---

## Этап 4. run.sh — три режима запуска (21:10)

```txt
fd02457 — fix: 413 context overflow auto-retry + run.sh launcher + compact tool descriptions
```

Скрипт `run.sh`:

| Режим | Команда | `AGENT_PROVIDER_POOL` |
|-------|---------|-----------------------|
| local | `./run.sh local` | не задан → `localhost:11434` |
| tailscale | `./run.sh tailscale` | `niceguy-1.tail349e77.ts.net/ollama` |
| openrouter | `./run.sh openrouter` | `openrouter.ai/api/v1` |

Режим `local` намеренно **сбрасывает** `AGENT_PROVIDER_POOL` (unset).

**Мотивация:** перешли от Docker Swarm (этап 2) к трём фиксированным режимам — локальный demon, удалёнка через Tailscale, OpenRouter.

---

## Архитектурный вывод

1. **Начальный use case** (несколько Ollama в Docker Swarm) не реализовался — перешли на локальный demon + Tailscale + OpenRouter
2. `CredentialRotator` остаётся в коде, но используется только при >1 endpoint в `AGENT_PROVIDER_POOL`
3. Bearer-аутентификация (`OLLAMA_API_KEY`) требуется для Ollama Cloud и OpenRouter
4. 413 Context Overflow (fd02457) добавил логику retry в `Agent::run_step`, что тоже часть провайдер-слоя

## Связанные статьи

- [[concepts/credential-rotator]]
- [[concepts/413-context-overflow]]
