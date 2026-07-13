#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# AI Agent — короткий запуск с выбором провайдера
#   ./run.sh local       (локальный Ollama, по умолчанию)
#   ./run.sh tailscale   (сервер niceguy через Tailscale)
#   ./run.sh openrouter  (облачные модели через OpenRouter)
#   ./run.sh cloud       (Ollama Cloud — api.ollama.com, ключ из OLLAMA_CLOUD_API_KEY)
# ─────────────────────────────────────────────────────────────

set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"

mode="${1:-local}"

case "$mode" in
  local|1)
    echo "→ Local Ollama (${OLLAMA_BASE_URL:-http://localhost:11434}, model: ${AI_AGENT_MODEL:-qwen2.5:3b})"
    unset AGENT_PROVIDER_POOL
    unset OLLAMA_API_KEY
    unset OLLAMA_CLOUD_API_KEY
    export OLLAMA_BASE_URL="${OLLAMA_BASE_URL:-http://localhost:11434}"
    export AI_AGENT_MODEL="${AI_AGENT_MODEL:-qwen2.5:3b}"
    ;;

  tailscale|2)
    echo "→ Tailscale proxy (niceguy-1)"
    unset AGENT_PROVIDER_POOL
    unset OLLAMA_CLOUD_API_KEY
    export AGENT_PROVIDER_POOL="https://niceguy-1.tail349e77.ts.net/ollama"
    export OLLAMA_API_KEY="${OLLAMA_API_KEY:-ollama}"
    export AI_AGENT_MODEL="${AI_AGENT_MODEL:-qwen3.6:latest}"
    ;;

  openrouter|3)
    echo "→ OpenRouter"
    unset AGENT_PROVIDER_POOL
    unset OLLAMA_CLOUD_API_KEY
    export AGENT_PROVIDER_POOL="https://openrouter.ai/api/v1"
    : "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY not set}"
    unset OLLAMA_API_KEY
    export AI_AGENT_MODEL="${AI_AGENT_MODEL:-nvidia/nemotron-3-super-120b-a12b:free}"
    ;;

  cloud|4)
    echo "→ Ollama Cloud (ollama.com)"
    unset AGENT_PROVIDER_POOL
    unset OLLAMA_API_KEY
    : "${OLLAMA_CLOUD_API_KEY:?OLLAMA_CLOUD_API_KEY not set — нужен API-ключ Ollama Cloud}"
    export AI_AGENT_MODEL="${AI_AGENT_MODEL:-nemotron-3-super:cloud}"
    ;;

  *)
    echo "Usage: $0 {local|tailscale|openrouter|cloud}" >&2
    exit 1
    ;;
esac

exec "$DIR/target/release/ai-agent" "$@"
