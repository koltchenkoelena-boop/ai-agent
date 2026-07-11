#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# AI Agent — короткий запуск с выбором провайдера
#   ./run.sh local       (локальный Ollama, по умолчанию)
#   ./run.sh tailscale   (сервер niceguy через Tailscale)
#   ./run.sh openrouter  (облачные модели через OpenRouter)
# ─────────────────────────────────────────────────────────────

set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"

mode="${1:-local}"

case "$mode" in
  local|1)
    echo "→ Local Ollama (nemotron-3-super:cloud)"
    unset AGENT_PROVIDER_POOL
    unset OLLAMA_API_KEY
    export AI_AGENT_MODEL="${AI_AGENT_MODEL:-nemotron-3-super:cloud}"
    ;;

  tailscale|2)
    echo "→ Tailscale proxy (niceguy-1)"
    unset AGENT_PROVIDER_POOL
    export AGENT_PROVIDER_POOL="https://niceguy-1.tail349e77.ts.net/ollama"
    export OLLAMA_API_KEY="${OLLAMA_API_KEY:-ollama}"
    export AI_AGENT_MODEL="${AI_AGENT_MODEL:-qwen3.6:latest}"
    ;;

  openrouter|3)
    echo "→ OpenRouter"
    unset AGENT_PROVIDER_POOL
    export AGENT_PROVIDER_POOL="https://openrouter.ai/api/v1"
    : "${OPENROUTER_API_KEY:?OPENROUTER_API_KEY not set}"
    unset OLLAMA_API_KEY
    export AI_AGENT_MODEL="${AI_AGENT_MODEL:-nvidia/nemotron-3-super-120b-a12b:free}"
    ;;

  *)
    echo "Usage: $0 {local|tailscale|openrouter}" >&2
    exit 1
    ;;
esac

exec "$DIR/target/release/ai-agent" "$@"
