#!/usr/bin/env bash
# qwen-session.sh — запуск изолированной сессии qwen-code CLI для параллельной работы.
#
# Идея: на одной рабочке поднимаем несколько CLI qwen-code над ОДНИМ проектом так,
# чтобы они НЕ мешали друг другу:
#   1) свой QWEN_HOME на сессию  → раздельные конфиг/история/сессии (нет гонок состояния);
#   2) свой git worktree + ветка → файлы физически разделены, мёрж потом безопасный;
#   3) своя модель/эндпоинт       → выбирается по -m из modelProviders (baseUrl + ключ).
#
# Использование:
#   ./qwen-session.sh <session-name> [model-id] [-- доп.аргументы qwen...]
#
# Примеры:
#   ./qwen-session.sh web-a Qwen/Qwen3-235B-A22B-Instruct-2507-FP8
#   ./qwen-session.sh web-b deepseek-v4-flash
#   ./qwen-session.sh web-a              # модель по умолчанию из базового settings.json
#   ./qwen-session.sh web-a <model> -- --approval-mode auto-edit
#
set -euo pipefail

# ---------- настройки ----------
BASE_QWEN_HOME="${BASE_QWEN_HOME:-$HOME/.qwen}"          # эталонный конфиг (settings.json, .env, skills...)
SESSIONS_ROOT="${SESSIONS_ROOT:-$HOME/.qwen-sessions}"   # сюда складываем изолированные QWEN_HOME
APPROVAL_MODE="${APPROVAL_MODE:-default}"               # plan|default|auto-edit|auto|yolo

# ---------- разбор аргументов ----------
if [[ $# -lt 1 ]]; then
  echo "Использование: $0 <session-name> [model-id] [-- доп.аргументы qwen...]" >&2
  exit 1
fi

SESSION="$1"; shift
MODEL=""
if [[ $# -gt 0 && "$1" != "--" ]]; then
  MODEL="$1"; shift
fi
# всё после "--" уходит как есть в qwen
EXTRA_ARGS=()
if [[ $# -gt 0 && "$1" == "--" ]]; then
  shift
  EXTRA_ARGS=("$@")
fi

# slug: только [a-z0-9-], чтобы безопасно использовать как имя ветки/каталога
if ! [[ "$SESSION" =~ ^[a-z0-9][a-z0-9-]*$ ]]; then
  echo "Ошибка: имя сессии '$SESSION' должно быть из [a-z0-9-] (например web-a)." >&2
  exit 1
fi

# ---------- проверка git-репозитория проекта ----------
if ! git rev-parse --show-toplevel >/dev/null 2>&1; then
  cat >&2 <<EOF
Ошибка: текущая папка не git-репозиторий.
qwen-code создаёт worktree/ветку внутри репозитория проекта.
Инициализируй один раз:
    git init && git add -A && git commit -m "init multi-agent project"
затем запусти скрипт снова.
EOF
  exit 1
fi
REPO_ROOT="$(git rev-parse --show-toplevel)"

# ---------- изолированный QWEN_HOME ----------
SESSION_HOME="$SESSIONS_ROOT/$SESSION"
if [[ ! -d "$SESSION_HOME" ]]; then
  echo ">> Создаю изолированный QWEN_HOME: $SESSION_HOME"
  mkdir -p "$SESSION_HOME"

  # копируем конфиг (у каждой сессии своя правка settings/история)
  [[ -f "$BASE_QWEN_HOME/settings.json"        ]] && cp "$BASE_QWEN_HOME/settings.json"        "$SESSION_HOME/"
  [[ -f "$BASE_QWEN_HOME/output-language.md"   ]] && cp "$BASE_QWEN_HOME/output-language.md"   "$SESSION_HOME/"
  [[ -f "$BASE_QWEN_HOME/source.json"          ]] && cp "$BASE_QWEN_HOME/source.json"          "$SESSION_HOME/"

  # .env (API-ключи) и skills — общие, поэтому симлинком, чтобы не плодить копии секретов
  [[ -e "$BASE_QWEN_HOME/.env"    ]] && ln -sf "$BASE_QWEN_HOME/.env"    "$SESSION_HOME/.env"
  [[ -d "$BASE_QWEN_HOME/skills"  ]] && ln -sf "$BASE_QWEN_HOME/skills"  "$SESSION_HOME/skills"
else
  echo ">> Использую существующий QWEN_HOME: $SESSION_HOME"
fi

# ---------- запуск ----------
echo ">> session : $SESSION"
echo ">> repo    : $REPO_ROOT"
echo ">> worktree: $REPO_ROOT/.qwen/worktrees/$SESSION  (ветка создаётся qwen-code)"
echo ">> model   : ${MODEL:-<из settings.json>}"
echo ">> approval: $APPROVAL_MODE"
echo

QWEN_CMD=( qwen
  --worktree "$SESSION"
  --chat-recording           # нужно для --continue/--resume
  --approval-mode "$APPROVAL_MODE"
)
[[ -n "$MODEL" ]] && QWEN_CMD+=( -m "$MODEL" )
QWEN_CMD+=( "${EXTRA_ARGS[@]}" )

# QWEN_HOME изолирует конфиг/историю/сессии этого CLI от остальных
exec env QWEN_HOME="$SESSION_HOME" "${QWEN_CMD[@]}"
