#!/usr/bin/env python3
"""
TUI-драйвер для тестов ai-agent (tmux-хак).
Имитирует ввод/чтение в окне терминала с TUI-режимом (ratatui):

  build                 — собрать бинарник один раз (cargo build); вызывать перед start()
  start                 — запустить собранный бинарник (target/debug/ai-agent --tui)
                           в tmux-сессии ai-tui-test. Бинарник НЕ пересобирается —
                           собрать заранее: `cargo build` (см. build_binary() /
                           scripts/loop_stats.sh, который делает это один раз перед циклом)
  ask "<текст>"         — отправить запрос, дождаться ответа (ждёт исчезновения «думает»),
                           печатает текст экрана (скроллбек) в stdout
  send "<текст>"        — только отправить (без ожидания)
  read [--raw]          — прочитать текущий экран (--raw: без очистки)
  stop                  — Ctrl+C + убить tmux-сессию
  status                — состояние: жива ли сессия, последняя строка (статус-бар)

Цикл «ввод → чтение → анализ → правка → перезапуск» — внешний (Hermes/CI):
  start; ask "…"; read; stop; правим src; start; …
"""

import subprocess
import sys
import time
import re
import shlex
import json
import datetime

SESSION = "ai-tui-test"
WORKDIR = "/home/avk/ws1/ai-agent"
BINARY = WORKDIR + "/target/debug/ai-agent"
# Запускаем готовый бинарник напрямую (не `cargo run`) — иначе каждый старт
# TUI заново гоняет cargo-проверку/пересборку и грузит все ядра CPU, что на
# длинных сериях (self_improve/loop_stats) подвешивает систему (см. отчёт
# пользователя: "мышь перестала работать после веб-UI пайпа" — виновата была
# параллельная пересборка, а не сам агент). Бинарник собирается один раз
# заранее (build_binary() / вызывающий скрипт), тут — только запуск.
RUN_CMD = f"{BINARY} --tui"
LOG_PATH = WORKDIR + "/tests/dialog_log.jsonl"


def build_binary():
    """Собрать бинарник один раз (debug). Вызывать перед серией start()."""
    print("Собираю бинарник (cargo build)...", file=sys.stderr)
    r = subprocess.run(
        ["cargo", "build"], cwd=WORKDIR, capture_output=True, text=True, timeout=600,
    )
    if r.returncode != 0:
        print(r.stdout[-2000:], file=sys.stderr)
        print(r.stderr[-2000:], file=sys.stderr)
        raise RuntimeError("cargo build failed")
    print("Бинарник собран.", file=sys.stderr)


def log_record(kind, text):
    """Протоколировать событие диалога (вопрос/ответ/прерывание/автономия)."""
    import os
    os.makedirs(os.path.dirname(LOG_PATH), exist_ok=True)
    rec = {
        "ts": datetime.datetime.now().isoformat(timespec="seconds"),
        "kind": kind,
        "text": text,
    }
    with open(LOG_PATH, "a", encoding="utf-8") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    return rec["ts"]


def tmux(*args, timeout=30):
    return subprocess.run(
        ["tmux", *args],
        capture_output=True, text=True, timeout=timeout,
    )


def start():
    import os
    if not os.path.isfile(BINARY):
        raise RuntimeError(
            f"Бинарник не найден: {BINARY}. Собери его заранее: "
            f"`cargo build` (или вызови build_binary())."
        )
    tmux("kill-session", "-t", SESSION)  # на всякий случай убрать старую
    subprocess.run(
        ["tmux", "new-session", "-d", "-s", SESSION, "-x", "160", "-y", "45",
         f"cd {WORKDIR} && {RUN_CMD}"],
        check=False, timeout=30,
    )
    # Ждём инициализацию TUI (появление рамки/приглашения).
    for _ in range(60):
        pane = read()
        if "AI Agent TUI" in pane or "ввод" in pane:
            print("TUI запущен", file=sys.stderr)
            return True
        time.sleep(1)
    print("FAIL: TUI не поднялся за 60с", file=sys.stderr)
    print(read(clean=False), file=sys.stderr)
    return False


def send(text, wait=0.8):
    tmux("send-keys", "-t", SESSION, text, "Enter")
    time.sleep(wait)


def read(clean=True, last_lines=None, history=False):
    # -S -3000: включить скроллбэк, иначе на длинных ответах маркер "🤖"
    # текущего хода может уехать выше видимой области раньше, чем
    # дорисуется весь текст, и ask() подхватит старый маркер с экрана.
    args = ["capture-pane", "-t", SESSION, "-p"]
    if history:
        args += ["-S", "-3000"]
    r = tmux(*args)
    pane = r.stdout
    if last_lines:
        pane = "\n".join(pane.splitlines()[-last_lines:])
    if clean:
        # Убрать ANSI-мусор.
        pane = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", pane)
        pane = re.sub(r"\x1b\][^\x07]*\x07", "", pane)
    return pane


def waiting(pane):
    """Агент ещё думает? (по статус-бару)."""
    for line in pane.splitlines():
        if "думает" in line:
            return True
    return False


def dialog(pane):
    """Извлечь значимые строки диалога (без рамок TUI и пустоты)."""
    out = []
    for line in pane.splitlines():
        s = line.strip(" │┌└┐┘─")
        if s.strip():
            out.append(s)
    return out


def alive():
    """Жива ли tmux-сессия? (проверка перед ask — иначе send уходит в никуда)."""
    return tmux("has-session", "-t", SESSION).returncode == 0


def ask(text, timeout=180):
    if not alive():
        raise RuntimeError("tmux сессия мертва — вызови start() перед ask()")
    send(text, wait=0.3)
    log_record("question", text)
    t0 = time.time()
    last = ""
    while time.time() - t0 < timeout:
        time.sleep(1)
        if not alive():
            raise RuntimeError("tmux сессия умерла во время ожидания ответа")
        pane = read()
        if pane == last and not waiting(pane):
            # Экран стабилен и агент не думает — ответ готов.
            break
        last = pane
    out = "\n".join(dialog(read(history=True)))
    log_record("answer", out)
    # Пустой ответ (нет ответа модели) — считаем ошибкой, а не успехом.
    text_after = ""
    if "🤖" in out:
        text_after = out.rsplit("🤖", 1)[1]
    if not " ".join(text_after.split()).strip():
        raise RuntimeError("пустой ответ (агент не ответил на запрос)")
    return out


def stop():
    tmux("send-keys", "-t", SESSION, "C-c")
    time.sleep(0.5)
    tmux("kill-session", "-t", SESSION)


def status():
    r = tmux("has-session", "-t", SESSION)
    alive = r.returncode == 0
    print(f"сессия {SESSION}: {'жива' if alive else 'мертва'}")
    if alive:
        pane = read(clean=False)
        lines = pane.splitlines()
        print("последняя строка:", repr(lines[-1] if lines else ""))


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        return
    cmd = args[0]
    if cmd == "build":
        build_binary()
    elif cmd == "start":
        start()
    elif cmd == "ask":
        text = " ".join(args[1:])
        out = ask(text)
        print(out)
    elif cmd == "send":
        send(" ".join(args[1:]))
    elif cmd == "read":
        raw = "--raw" in args
        print(read(clean=not raw))
    elif cmd == "stop":
        stop()
    elif cmd == "attach":
        # Присоединиться к живой сессии (видеть диалог; Ctrl+B, D — отсоединиться).
        subprocess.run(["tmux", "attach", "-t", SESSION], check=False)
    elif cmd == "status":
        status()
    elif cmd == "log":
        # log "<kind>" "<текст>" — записать событие в tests/dialog_log.jsonl
        kind = args[1] if len(args) > 1 else "note"
        text = " ".join(args[2:])
        ts = log_record(kind, text)
        print(ts, kind, text[:80])
    else:
        print(f"неизвестная команда: {cmd}")
        print(__doc__)
        sys.exit(1)


if __name__ == "__main__":
    main()
