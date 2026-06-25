#!/bin/bash
# Quick start without Docker — runs all agents as local processes
# Usage: ./run.sh [start|stop|status]

set -e
cd "$(dirname "$0")"

PIDS_DIR="./pids"
LOGS_DIR="./logs"
mkdir -p "$PIDS_DIR" "$LOGS_DIR"

start() {
    echo "=== Multi-Agent System ==="
    echo ""

    # Load bind address from .env
    BIND_HOST="${AGENT_BIND_HOST:-100.88.113.50}"
    if [ -f .env ]; then
        eval "$(grep -v '^#' .env | grep AGENT_BIND_HOST)"
        BIND_HOST="${AGENT_BIND_HOST:-$BIND_HOST}"
    fi
    export AGENT_BIND_HOST="$BIND_HOST"

    echo "Bind address: $BIND_HOST (internal only)"
    echo ""

    # Check dependencies
    if ! python3 -c "import fastapi, uvicorn, httpx, yaml" 2>/dev/null; then
        echo "Installing dependencies..."
        pip install -r requirements.txt -q
    fi

    # Start agents
    for agent in planner coder researcher reviewer; do
        if [ -f "$PIDS_DIR/$agent.pid" ] && kill -0 "$(cat "$PIDS_DIR/$agent.pid")" 2>/dev/null; then
            echo "  [$agent] already running (pid=$(cat "$PIDS_DIR/$agent.pid"))"
        else
            nohup python3 agent_server.py "$agent" >> "$LOGS_DIR/$agent.log" 2>&1 &
            echo $! > "$PIDS_DIR/$agent.pid"
            disown $!
            echo "  [$agent] started (pid=$!)"
        fi
    done

    # Start web server (bound to internal address)
    if [ -f "$PIDS_DIR/web.pid" ] && kill -0 "$(cat "$PIDS_DIR/web.pid")" 2>/dev/null; then
        echo "  [web] already running"
    else
        nohup python3 -m http.server 8000 --bind "$BIND_HOST" --directory web >> "$LOGS_DIR/web.log" 2>&1 &
        echo $! > "$PIDS_DIR/web.pid"
        disown $!
        echo "  [web] started (pid=$!, port=8000)"
    fi

    echo ""
    echo "Web Chat:    http://$BIND_HOST:8000"
    echo "Planner API: http://$BIND_HOST:8001"
    echo "Coder API:   http://$BIND_HOST:8002"
    echo "Researcher:  http://$BIND_HOST:8003"
    echo "Reviewer:    http://$BIND_HOST:8004"
    echo ""
    echo "Access: only from Tailscale network (not exposed to internet)"
    echo "Logs: $LOGS_DIR/"
    echo "Stop: $0 stop"
}

stop() {
    echo "Stopping agents..."
    for pid_file in "$PIDS_DIR"/*.pid; do
        if [ -f "$pid_file" ]; then
            name=$(basename "$pid_file" .pid)
            pid=$(cat "$pid_file")
            if kill -0 "$pid" 2>/dev/null; then
                kill "$pid"
                echo "  [$name] stopped (pid=$pid)"
            fi
            rm "$pid_file"
        fi
    done
    echo "Done."
}

status() {
    echo "Agent Status:"
    for agent in planner coder researcher reviewer web; do
        if [ -f "$PIDS_DIR/$agent.pid" ] && kill -0 "$(cat "$PIDS_DIR/$agent.pid")" 2>/dev/null; then
            echo "  [$agent] running (pid=$(cat "$PIDS_DIR/$agent.pid"))"
        else
            echo "  [$agent] stopped"
        fi
    done
}

case "${1:-start}" in
    start)  start ;;
    stop)   stop ;;
    status) status ;;
    restart) stop; sleep 1; start ;;
    *)      echo "Usage: $0 {start|stop|status|restart}"; exit 1 ;;
esac
