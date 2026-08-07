#!/usr/bin/env python3
"""
Прогон вопросов агенту БАТЧАМИ: рестарт сессии каждые N вопросов (чистый контекст —
защита от деградации deepseek на длинных сессиях) + анализ ответов + отчёт JSON.

Использование:
  python3 scripts/batch_ask.py \
      --questions "вопрос1|вопрос2|..." \
      --target "слово1|слово2|..."   (опционально, по одному на вопрос)
      --restart-every 10
      --max-time 120

Выход: out/batch_ask_<ts>.json + краткий отчёт в stdout.
"""

import argparse
import datetime
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__))))
import tui_driver as td

OUT_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "out")


def analyze(q, answer, target, dur, prev_answers, max_time):
    issues = []
    text = " ".join(answer.split())
    if len(text) < 10:
        issues.append("пустой ответ")
    if "Provider error" in text or "Execution error" in text:
        issues.append("ошибка провайдера")
    if dur > max_time:
        issues.append(f"слишком долго ({dur:.0f}s > {max_time}s)")
    for prev in prev_answers[-3:]:
        if prev and text[:80] and text[:80] == prev[:80]:
            issues.append("повтор предыдущего ответа")
            break
    if target:
        if not any(w.lower() in text.lower() for w in target.lower().split()):
            issues.append(f"нет ключевого слова ({target})")
    return issues


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--questions", default="привет|найди все TODO в src через grep и скажи где|прочитай файл src/main.rs и скажи, что он делает")
    ap.add_argument("--target", default="")
    ap.add_argument("--restart-every", type=int, default=10)
    ap.add_argument("--max-time", type=int, default=120)
    args = ap.parse_args()

    questions = [q.strip() for q in args.questions.split("|") if q.strip()]
    targets = [t.strip() for t in args.target.split("|") if t.strip()] if args.target else []
    if targets and len(targets) != len(questions):
        targets = [targets[0]] * len(questions)

    ts = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
    os.makedirs(OUT_DIR, exist_ok=True)

    r = subprocess.run(["tmux", "has-session", "-t", td.SESSION], capture_output=True)
    if r.returncode != 0:
        td.start()
        time.sleep(2)

    results = []
    prev_answers = []
    for i, q in enumerate(questions, 1):
        if args.restart_every and i > 1 and (i - 1) % args.restart_every == 0:
            print(f"\n--- рестарт сессии (чистый контекст) ---", flush=True)
            td.stop()
            time.sleep(1)
            td.start()
            time.sleep(2)
            prev_answers = []
        t0 = time.time()
        try:
            out = td.ask(q, timeout=args.max_time + 30)
            dur = time.time() - t0
            ans = out
            if "🤖" in out:
                ans = out.rsplit("🤖", 1)[1]
            ans = " ".join(ans.split())
        except Exception as e:
            dur = time.time() - t0
            ans = ""
            issues = [f"ask error: {e}"]
            results.append({"q": q, "ok": False, "dur_s": round(dur, 1), "issues": issues, "answer": ""})
            print(f"[{i}/{len(questions)}] FAIL {dur:.0f}s | {q[:60]} | {', '.join(issues)}", flush=True)
            continue
        target = targets[i - 1] if targets else ""
        issues = analyze(q, ans, target, dur, prev_answers, args.max_time)
        prev_answers.append(ans)
        results.append({"q": q, "ok": not issues, "dur_s": round(dur, 1), "issues": issues, "answer": ans[:200]})
        status = "OK " if not issues else "FAIL"
        print(f"[{i}/{len(questions)}] {status} {dur:.0f}s | {q[:60]} | {ans[:60]}", flush=True)
        if issues:
            print(f"       проблемы: {', '.join(issues)}", flush=True)
        time.sleep(1)

    td.stop()

    ok_n = sum(1 for x in results if x["ok"])
    report = {
        "ts": ts,
        "total": len(results),
        "ok": ok_n,
        "fail": len(results) - ok_n,
        "restart_every": args.restart_every,
        "results": results,
    }
    path = os.path.join(OUT_DIR, f"batch_ask_{ts}.json")
    with open(path, "w", encoding="utf-8") as f:
        json.dump(report, f, ensure_ascii=False, indent=2)

    print("\n=== ОТЧЁТ ===")
    print(f"вопросов: {len(results)}, ок: {ok_n}, проблем: {len(results) - ok_n}")
    for x in results:
        if not x["ok"]:
            print(f"  FAIL: {x['q'][:60]} -> {', '.join(x['issues'])}")
    print(f"файл отчёта: {path}")
    sys.exit(0 if ok_n == len(results) else 1)


if __name__ == "__main__":
    main()
