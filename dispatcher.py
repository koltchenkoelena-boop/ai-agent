"""
Dispatcher — Administrative observer service for the Multi-Agent system.
Tracks active connections, provides session management, usage statistics.
Runs as a separate FastAPI service on its own port.
"""

import time
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Optional

from fastapi import FastAPI, HTTPException, Query
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

import auth
import session_db


# --- Models ---

class ConnectEvent(BaseModel):
    session_id: str
    user_id: str
    agent_name: str
    model: Optional[str] = None
    remote_addr: Optional[str] = None


class DisconnectEvent(BaseModel):
    session_id: str
    user_id: str
    agent_name: str


class MessageEvent(BaseModel):
    session_id: str
    role: str
    content: Optional[str] = None
    tool_name: Optional[str] = None
    tokens: Optional[int] = None


# --- Active Connections Registry (in-memory) ---

active_connections: dict[str, dict] = {}
# key: session_id, value: {user_id, agent_name, model, connected_at, remote_addr}


# --- App ---

@asynccontextmanager
async def lifespan(app):
    # Startup
    db_path = str(Path(__file__).parent / "shared" / "sessions.db")
    await session_db.get_db(db_path)
    shared_dir = str(Path(__file__).parent / "shared")
    auth.init(shared_dir)
    print("[dispatcher] Started — session DB and auth initialized")
    yield
    # Shutdown
    if session_db._instance:
        await session_db._instance.close()


app = FastAPI(
    title="Multi-Agent Dispatcher",
    description="Administrative observer & session manager",
    lifespan=lifespan,
)
app.add_middleware(
    CORSMiddleware, allow_origins=["*"], allow_methods=["*"], allow_headers=["*"]
)


def _require_admin(token: str):
    """Verify token and check admin role."""
    username = auth.verify_token(token)
    if not username:
        raise HTTPException(401, "Invalid or expired token")
    if not auth.is_admin(username):
        raise HTTPException(403, "Admin access required")
    return username


# --- Health ---

@app.get("/health")
async def health():
    return {"service": "dispatcher", "status": "ok", "active_connections": len(active_connections)}


# --- Event Receivers (called by agents, no auth needed — internal) ---

@app.post("/event/connect")
async def event_connect(event: ConnectEvent):
    """Agent reports a new WebSocket connection."""
    active_connections[event.session_id] = {
        "user_id": event.user_id,
        "agent_name": event.agent_name,
        "model": event.model,
        "connected_at": time.time(),
        "remote_addr": event.remote_addr,
    }
    return {"status": "registered"}


@app.post("/event/disconnect")
async def event_disconnect(event: DisconnectEvent):
    """Agent reports a WebSocket disconnection."""
    active_connections.pop(event.session_id, None)
    return {"status": "removed"}


@app.post("/event/message")
async def event_message(event: MessageEvent):
    """Agent reports a message (optional, for live tracking)."""
    conn = active_connections.get(event.session_id)
    if conn:
        conn["last_message_at"] = time.time()
        conn["last_role"] = event.role
    return {"status": "noted"}


# --- Connections (live monitoring) ---

@app.get("/connections")
async def list_connections(token: str = Query(...)):
    """List all currently active WebSocket connections."""
    _require_admin(token)
    result = []
    now = time.time()
    for sid, info in active_connections.items():
        result.append({
            "session_id": sid,
            "user_id": info["user_id"],
            "agent_name": info["agent_name"],
            "model": info.get("model"),
            "connected_at": info["connected_at"],
            "duration_sec": round(now - info["connected_at"]),
            "remote_addr": info.get("remote_addr"),
            "last_message_at": info.get("last_message_at"),
        })
    return {"connections": result, "count": len(result)}


# --- Sessions ---

@app.get("/sessions")
async def list_sessions(
    token: str = Query(...),
    user_id: str = None,
    agent_name: str = None,
    status: str = None,
    date_from: float = None,
    date_to: float = None,
    limit: int = 50,
    offset: int = 0,
):
    """List sessions with optional filters."""
    _require_admin(token)
    db = await session_db.get_db()
    sessions = await db.list_sessions(
        user_id=user_id,
        agent_name=agent_name,
        status=status,
        date_from=date_from,
        date_to=date_to,
        limit=limit,
        offset=offset,
    )
    total = await db.count_sessions(user_id=user_id, agent_name=agent_name, status=status)
    return {"sessions": sessions, "total": total, "limit": limit, "offset": offset}


@app.get("/sessions/{session_id}")
async def get_session(session_id: str, token: str = Query(...), messages: bool = True):
    """Get full session details with message history."""
    _require_admin(token)
    db = await session_db.get_db()
    session = await db.get_session(session_id)
    if not session:
        raise HTTPException(404, "Session not found")
    result = {"session": session}
    if messages:
        result["messages"] = await db.get_messages(session_id)
    return result


@app.delete("/sessions/{session_id}")
async def delete_session(session_id: str, token: str = Query(...)):
    """Delete a session and its messages from the database."""
    _require_admin(token)
    db = await session_db.get_db()
    deleted = await db.delete_session(session_id)
    if not deleted:
        raise HTTPException(404, "Session not found")
    # Also remove from active connections if present
    active_connections.pop(session_id, None)
    return {"status": "deleted", "session_id": session_id}


@app.post("/sessions/{session_id}/interrupt")
async def interrupt_session(session_id: str, token: str = Query(...)):
    """Interrupt an active session — marks it as interrupted and removes from active."""
    _require_admin(token)
    db = await session_db.get_db()
    session = await db.get_session(session_id)
    if not session:
        raise HTTPException(404, "Session not found")
    if session["status"] != "active":
        raise HTTPException(400, f"Session is not active (status: {session['status']})")
    await db.end_session(session_id, status="interrupted")
    active_connections.pop(session_id, None)
    return {"status": "interrupted", "session_id": session_id}


# --- Users ---

@app.get("/users")
async def list_users(token: str = Query(...)):
    """List all users with their usage stats."""
    _require_admin(token)
    db = await session_db.get_db()
    return {"users": await db.get_stats_by_user()}


@app.get("/users/{username}/sessions")
async def user_sessions(
    username: str, token: str = Query(...), limit: int = 50, offset: int = 0
):
    """Get sessions for a specific user."""
    _require_admin(token)
    db = await session_db.get_db()
    sessions = await db.list_sessions(user_id=username, limit=limit, offset=offset)
    total = await db.count_sessions(user_id=username)
    return {"user": username, "sessions": sessions, "total": total}


@app.delete("/users/{username}")
async def delete_user_data(username: str, token: str = Query(...)):
    """Delete all session data for a user."""
    _require_admin(token)
    db = await session_db.get_db()
    count = await db.delete_user_sessions(username)
    # Remove their active connections
    to_remove = [sid for sid, info in active_connections.items() if info["user_id"] == username]
    for sid in to_remove:
        del active_connections[sid]
    return {"status": "deleted", "user": username, "sessions_deleted": count}


# --- Statistics ---

@app.get("/stats/summary")
async def stats_summary(token: str = Query(...)):
    """Overall system statistics."""
    _require_admin(token)
    db = await session_db.get_db()
    summary = await db.get_stats_summary()
    summary["live_connections"] = len(active_connections)
    return summary


@app.get("/stats/usage")
async def stats_usage(
    token: str = Query(...),
    by: str = "daily",
    days: int = 30,
):
    """Usage statistics in different breakdowns."""
    _require_admin(token)
    db = await session_db.get_db()
    if by == "daily":
        return {"by": "daily", "data": await db.get_stats_daily(days)}
    elif by == "agent":
        return {"by": "agent", "data": await db.get_stats_by_agent()}
    elif by == "model":
        return {"by": "model", "data": await db.get_stats_by_model()}
    elif by == "user":
        return {"by": "user", "data": await db.get_stats_by_user()}
    else:
        raise HTTPException(400, f"Invalid breakdown: {by}. Use: daily, agent, model, user")


@app.get("/stats/models")
async def stats_models(token: str = Query(...)):
    """Model usage distribution."""
    _require_admin(token)
    db = await session_db.get_db()
    return {"models": await db.get_stats_by_model()}


# --- Entry point ---

if __name__ == "__main__":
    import sys
    import uvicorn
    import yaml

    config_path = Path(__file__).parent / "config.yaml"
    cfg = yaml.safe_load(config_path.read_text())
    dispatcher_cfg = cfg.get("dispatcher", {})
    port = dispatcher_cfg.get("port", 8005)
    host = dispatcher_cfg.get("host", "127.0.0.1")

    print(f"[dispatcher] Starting on {host}:{port}")
    uvicorn.run(app, host=host, port=port)
