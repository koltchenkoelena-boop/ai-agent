#!/usr/bin/env python3
"""
TUI-драйвер для тестов ai-agent (tmux-хак).
Имитирует ввод/чтение в окне терминала с TUI-режимом (ratatui):

  start                 — запустить `cargo run -- --tui` в tmux-сессии ai-tui-test
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
RUN_CMD = "cargo run -- --tui"
LOG_PATH = WORKDIR + "/tests/dialog_log.jsonl"


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


def read(clean=True, last_lines=None):
    r = tmux("capture-pane", "-t", SESSION, "-p")
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
    out = "\n".join(dialog(read()))
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
    if cmd == "start":
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
