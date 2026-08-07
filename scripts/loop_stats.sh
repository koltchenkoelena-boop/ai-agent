#!/usr/bin/env bash
# Прогоняет self_improve.py N раз подряд, копит статистику по отчётам out/self_improve_*.json.
# Использование: scripts/loop_stats.sh [N]
set -uo pipefail
cd "$(dirname "$0")/.."

N="${1:-100}"
STAMP="$(date +%Y%m%d_%H%M%S)"
SUMMARY="out/loop_stats_${STAMP}.jsonl"
LOG="out/loop_stats_${STAMP}.log"

echo "loop_stats: N=$N, summary=$SUMMARY, log=$LOG"

for i in $(seq 1 "$N"); do
    before=$(ls out/self_improve_*.json 2>/dev/null | sort)
    ts0=$(date +%s)
    python3 scripts/self_improve.py >> "$LOG" 2>&1
    ec=$?
    dur=$(( $(date +%s) - ts0 ))
    after=$(ls out/self_improve_*.json 2>/dev/null | sort)
    report=$(comm -13 <(echo "$before") <(echo "$after") | head -1)
    echo "{\"iter\": $i, \"exit\": $ec, \"dur_s\": $dur, \"report\": \"${report:-}\"}" >> "$SUMMARY"
    echo "[loop $i/$N] exit=$ec dur=${dur}s report=${report:-none}"
done

echo "loop_stats: done -> $SUMMARY"
