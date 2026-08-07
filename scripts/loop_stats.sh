#!/usr/bin/env bash
# Прогоняет self_improve.py N раз подряд, копит статистику по отчётам out/self_improve_*.json.
# Использование: scripts/loop_stats.sh [N]
#
# Ресурсы: (1) бинарник собирается ОДИН раз перед циклом (tui_driver.py больше не
# гоняет `cargo run` на каждый start() — это грузило все ядра и подвешивало
# систему, включая мышь/десктоп); (2) весь прогон заворачивается в systemd-run
# --user --scope с жёстким лимитом CPU/памяти (cgroup), а не просто "по-хорошему" —
# это тонкий клиент, ему не положено класть систему пользователя.
set -uo pipefail
cd "$(dirname "$0")/.."

# ---- Жёсткий лимит ресурсов через cgroup (systemd --user scope) -----------
CPU_QUOTA="${LOOP_STATS_CPU_QUOTA:-400%}"   # 4 ядра из 16 по умолчанию
MEM_MAX="${LOOP_STATS_MEM_MAX:-4G}"
if [ -z "${LOOP_STATS_CONFINED:-}" ] && command -v systemd-run >/dev/null 2>&1; then
    export LOOP_STATS_CONFINED=1
    exec systemd-run --user --scope --quiet \
        -p CPUQuota="$CPU_QUOTA" -p MemoryMax="$MEM_MAX" \
        -- "$0" "$@"
fi

N="${1:-100}"
STAMP="$(date +%Y%m%d_%H%M%S)"
SUMMARY="out/loop_stats_${STAMP}.jsonl"
LOG="out/loop_stats_${STAMP}.log"

echo "loop_stats: N=$N, summary=$SUMMARY, log=$LOG (cgroup: CPUQuota=$CPU_QUOTA MemoryMax=$MEM_MAX)"

# ---- Собрать бинарник один раз (не на каждой итерации) --------------------
python3 scripts/tui_driver.py build

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
