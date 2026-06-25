#!/usr/bin/env python3
"""
Multi-Agent Launcher — запускает все 4 агента + web-сервер для фронтенда.
Управляет жизненным циклом, логами, health-checks.
"""

import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import yaml


BASE_DIR = Path(__file__).parent.resolve()
CONFIG_PATH = BASE_DIR / "config.yaml"
WEB_DIR = BASE_DIR / "web"
WEB_PORT = 8000


def load_config():
    return yaml.safe_load(CONFIG_PATH.read_text())


def start_agent(agent_cfg: dict) -> subprocess.Popen:
    """Start an agent as a subprocess."""
    name = agent_cfg["name"]
    port = agent_cfg["port"]
    log_file = BASE_DIR / "logs" / f"{name}.log"
    log_file.parent.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env["PYTHONUNBUFFERED"] = "1"

    proc = subprocess.Popen(
        [sys.executable, str(BASE_DIR / "agent_server.py"), name],
        cwd=str(BASE_DIR),
        stdout=open(log_file, "a"),
        stderr=subprocess.STDOUT,
        env=env,
    )
    print(f"  [{name}] started (pid={proc.pid}, port={port}, log={log_file})")
    return proc


def start_web_server() -> subprocess.Popen:
    """Start a simple HTTP server for the web chat frontend."""
    log_file = BASE_DIR / "logs" / "web.log"
    log_file.parent.mkdir(parents=True, exist_ok=True)

    proc = subprocess.Popen(
        [sys.executable, "-m", "http.server", str(WEB_PORT), "--directory", str(WEB_DIR)],
        cwd=str(BASE_DIR),
        stdout=open(log_file, "a"),
        stderr=subprocess.STDOUT,
    )
    print(f"  [web] started (pid={proc.pid}, port={WEB_PORT})")
    return proc


def start_dispatcher(config: dict) -> subprocess.Popen:
    """Start the dispatcher admin service."""
    dispatcher_cfg = config.get("dispatcher", {})
    port = dispatcher_cfg.get("port", 8005)
    log_file = BASE_DIR / "logs" / "dispatcher.log"
    log_file.parent.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env["PYTHONUNBUFFERED"] = "1"

    proc = subprocess.Popen(
        [sys.executable, str(BASE_DIR / "dispatcher.py")],
        cwd=str(BASE_DIR),
        stdout=open(log_file, "a"),
        stderr=subprocess.STDOUT,
        env=env,
    )
    print(f"  [dispatcher] started (pid={proc.pid}, port={port}, log={log_file})")
    return proc


def health_check(port: int, timeout: float = 5.0) -> bool:
    """Check if an agent is responding."""
    import urllib.request
    try:
        req = urllib.request.urlopen(f"http://localhost:{port}/health", timeout=timeout)
        return req.status == 200
    except Exception:
        return False


def main():
    print("=" * 60)
    print("  Multi-Agent System Launcher")
    print("=" * 60)

    config = load_config()
    agents = config["agents"]

    processes = {}
    
    # Start all agents
    print("\nStarting agents:")
    for agent_cfg in agents:
        proc = start_agent(agent_cfg)
        processes[agent_cfg["name"]] = {"proc": proc, "config": agent_cfg}

    # Start web frontend
    print("\nStarting web frontend:")
    web_proc = start_web_server()
    processes["_web"] = {"proc": web_proc, "config": {"port": WEB_PORT}}

    # Start dispatcher
    print("\nStarting dispatcher:")
    disp_proc = start_dispatcher(config)
    dispatcher_cfg = config.get("dispatcher", {})
    processes["dispatcher"] = {"proc": disp_proc, "config": dispatcher_cfg}

    # Wait a bit, then check health
    print("\nWaiting for agents to initialize...")
    time.sleep(3)

    print("\nHealth check:")
    all_ok = True
    for agent_cfg in agents:
        ok = health_check(agent_cfg["port"])
        status = "✓ online" if ok else "✗ offline"
        print(f"  [{agent_cfg['name']}] {status}")
        if not ok:
            all_ok = False

    # Check dispatcher health
    disp_port = dispatcher_cfg.get("port", 8005)
    disp_ok = health_check(disp_port)
    print(f"  [dispatcher] {'✓ online' if disp_ok else '✗ offline'}")
    if not disp_ok:
        all_ok = False

    print(f"\n{'All services online!' if all_ok else 'Some services failed to start — check logs/'}")
    print(f"\n  Web Chat:    http://localhost:{WEB_PORT}")
    for agent_cfg in agents:
        print(f"  {agent_cfg['name']:12s} API: http://localhost:{agent_cfg['port']}")
    print(f"  {'dispatcher':12s} API: http://localhost:{disp_port}")
    print(f"\n  Logs:        {BASE_DIR / 'logs/'}")
    print("\nPress Ctrl+C to stop all agents.")

    # Handle graceful shutdown
    def shutdown(signum, frame):
        print("\n\nShutting down...")
        for name, info in processes.items():
            proc = info["proc"]
            if proc.poll() is None:
                proc.terminate()
                print(f"  [{name}] terminated")
        # Wait for processes
        for name, info in processes.items():
            try:
                info["proc"].wait(timeout=5)
            except subprocess.TimeoutExpired:
                info["proc"].kill()
        print("Done.")
        sys.exit(0)

    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)

    # Monitor loop
    while True:
        time.sleep(10)
        for name, info in processes.items():
            proc = info["proc"]
            if proc.poll() is not None and name not in ("_web",):
                print(f"\n  WARNING: [{name}] died (exit={proc.returncode}), restarting...")
                if name == "dispatcher":
                    new_proc = start_dispatcher(config)
                else:
                    new_proc = start_agent(info["config"])
                processes[name]["proc"] = new_proc


if __name__ == "__main__":
    main()
