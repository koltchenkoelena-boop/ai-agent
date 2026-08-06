# Интеграция Luck → ai-agent (вариант А: Luck-компилятор → JSON-план → Rust-исполнитель)

Статус: план. Дата: 2026-08-06.

## Идея

Luck (Python) работает как ОФЛАЙН-КОМПИЛЯТОР: из графа намерения (.luck-файл) генерирует
JSON-план — DAG узлов с контрактами. ai-agent (Rust) исполняет план через свой runtime
(провайдеры, тулы, safety, память). Никакого Python в рантайме.

```
Luck-граф (text) ──compile──▶ plan.json (DAG) ──▶ ai-agent Rust Scheduler ──▶ результат
                                ▲                    │
                                └──── контракты: REJECT/VERIFY/POLICY, рёбра, слоты
```

## Формат плана (plan.json)

```json
{
  "plan_version": 1,
  "nodes": [
    {"id": "role",        "kind": "ROLE",      "slots": {"as": "senior incident engineer"}, "into": "ctx"},
    {"id": "severity",    "kind": "CLASSIFY",  "input": "ctx", "labels": ["critical","warning","info"], "into": "level"},
    {"id": "fork",        "kind": "BRANCH",    "on": "level", "branches": {"critical": ["runbook"], "warning": ["probe"]}},
    {"id": "runbook",     "kind": "TOOL",      "tool": "mcp:runbook", "args": {"query": "from:ctx"}, "policy": {"require": "confirmed"}, "into": "doc"},
    {"id": "verify_doc",  "kind": "VERIFY",    "predicate": "grep", "subject": "doc", "on_fail": "REJECT"},
    {"id": "probe",       "kind": "TOOL",      "tool": "shell", "args": {"cmd": "kubectl get pods"}, "into": "probe_out"},
    {"id": "merge",       "kind": "MERGE",     "inputs": ["doc","probe_out"], "into": "final"},
    {"id": "report",      "kind": "STEP",      "do": "synthesize final report from final", "verify": {"grep": "evidence"}, "into": "result"}
  ],
  "edges": [
    {"from": "role", "to": "severity", "type": "SEQ"},
    {"from": "severity", "to": "fork", "type": "SEQ"},
    {"from": "fork", "to": "runbook", "type": "BRANCH", "label": "critical"},
    {"from": "fork", "to": "probe", "type": "BRANCH", "label": "warning"},
    {"from": "runbook", "to": "verify_doc", "type": "SEQ"},
    {"from": "verify_doc", "to": "merge", "type": "SEQ"},
    {"from": "probe", "to": "merge", "type": "SEQ"},
    {"from": "merge", "to": "report", "type": "SEQ"}
  ],
  "limits": {"max_nodes": 32, "max_depth": 6, "max_tokens_per_node": 4000}
}
```

Ключевое: контракты уже в формате — VERIFY как узел (ground-значение + зарегистрированный
предикат), POLICY как декларация (ALLOW+REQUIRE, без deny_effect — учтено жюри), REJECT
как состояние (reason: SYNTAX/TYPE/BUDGET/VERIFY/POLICY).

## Что делает Rust-исполнитель (новый модуль src/luck_plan.rs)

1. Валидатор плана: типы узлов, рёбра (нет циклов, BRANCH-метки покрыты), контракты
   проверяются на компиляции плана (граф с непроверяемым VERIFY — не компилируется).
2. Scheduler: обход DAG, исполнение узлов:
   - ROLE/CLASSIFY/STEP → существующий агентский цикл (FallbackProvider, stream_chat)
   - TOOL → существующий tool_routing (включая MCP-транспорт)
   - BRANCH/MERGE → контекст-ветвление (переиспользовать механику веток ai-agent)
   - VERIFY → детерминированный предикат (grep/файл:строка) — registry предикатов
   - REJECT → состояние графа, не исключение; сообщение в событийную шину
3. Прогресс → FrontendEvent (существующая шина WebSocket): NodeStarted/NodeDone/ToolExecuting.
4. Память: результаты узлов в sled (уже есть) — кэш по id узла (аналог cache_key Luck).

## Этапы

1. Формат plan.json + валидатор (Rust, serde) — чистый, без Luck.
2. Компилятор: порт парсера Luck на минимум (граф → plan.json) — Python-скрипт
   (src/luck_compile.py рядом с ai-agent или отдельный бинарь), либо быстрый парсер
   Luck-синтаксиса на Rust, если хочется без Python.
3. Rust Scheduler (src/luck_plan.rs): DAG-обход, узлы, BRANCH/MERGE, REJECT.
4. Интеграция: команда `/plan <file.luck>` в CLI (и через WebSocket-команду).
5. UI-прогресс: NodeStarted/NodeDone в существующий TUI/веб-фронт.

## Границы (что НЕ делаем)

- Не встраиваем Luck в каждый ход агентского цикла — план запускается явно (/plan).
- Память (vector_db), safety-пайплайн, провайдеры — остаются в ai-agent; Luck не дублирует.
- Канонический кэш Luck в Python не тащим — кэш результатов по id узла в sled (свой).
- POLICY/VERIFY — сразу в формате по рекомендациям жюри (compile-time проверки).

## Открытые вопросы

- Парсер Luck: порт на Rust или Python-компилятор? (для v1 — Python, быстрее)
- Узлы SPAWN в v1: исполняем как вложенный план (рекурсия) или откладываем?
- Связь VERIFY-предикатов с реестром тулов (ToolRegistry) — где регистрировать предикаты.

## Референсы

- Luck: /home/avk/projects/luck (граф, реестр, Scheduler, ValidatingBackend/REJECT_MARK)
- Жюри POLICY/VERIFY: /home/avk/projects/luck/notes/jury-policy-verify.md
- Исследования Claude/Codex: /home/avk/workspace/claude-imp/REPORT.md,
  /home/avk/workspace/notes/codex-overview.md
