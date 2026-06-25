#!/usr/bin/env bash
# qwen-session.sh — запуск изолированной сессии qwen-code для параллельной работы.
#
# Изоляция:
#   - QWEN_HOME=~/.qwen-sessions/<name> — отдельный конфиг/история/todos на сессию
#     (засеивается из базового ~/.qwen: settings.json + .env с ключами провайдеров).
#   - --worktree <name> — qwen сам создаёт git worktree в <repoRoot>/.qwen/worktrees/<name>,
#     поэтому правки файлов разных сессий не конфликтуют (свой рабочий дерево + ветка).
#   - -m <model> — своя модель на сессию.
#
# Использование:
#   ./qwen-session.sh <session-name> [model]
#
# Примеры:
#   ./qwen-session.sh web-a Qwen/Qwen3-235B-A22B-Instruct-2507-FP8
#   ./qwen-session.sh web-b deepseek-v4-flash
#
# Доступные модели (из ~/.qwen/settings.json modelProviders.openai):
#   - Qwen/Qwen3-235B-A22B-Instruct-2507-FP8   (Gonka, ключ GONKA_API_KEY)
#   - deepseek-v4-flash                        (DeepSeek, ключ DEEPSEEK_API_KEY)
set -euo pipefail

NAME="${1:?usage: qwen-session.sh <session-name> [model]}"
MODEL="${2:-Qwen/Qwen3-235B-A22B-Instruct-2507-FP8}"

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE_QWEN="${QWEN_HOME_BASE:-$HOME/.qwen}"
SESSION_HOME="$HOME/.qwen-sessions/$NAME"

# 1. Засеять изолированный конфиг из базового (только при первом запуске).
mkdir -p "$SESSION_HOME"
for f in settings.json .env; do
  if [[ -f "$BASE_QWEN/$f" && ! -f "$SESSION_HOME/$f" ]]; then
    cp "$BASE_QWEN/$f" "$SESSION_HOME/$f"
  fi
done

# 2. Репозиторий обязателен для --worktree.
if ! git -C "$PROJECT_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
  echo "error: $PROJECT_ROOT не под git — сначала: git init && git add -A && git commit" >&2
  exit 1
fi

echo ">> session=$NAME  model=$MODEL"
echo ">> QWEN_HOME=$SESSION_HOME"
echo ">> worktree=$PROJECT_ROOT/.qwen/worktrees/$NAME"

# 3. Запуск изолированной сессии в собственном git worktree.
cd "$PROJECT_ROOT"
exec env QWEN_HOME="$SESSION_HOME" qwen -m "$MODEL" --worktree "$NAME"
