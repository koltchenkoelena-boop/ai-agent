"""
Session Database — SQLite persistent storage for multi-agent sessions.
Stores session metadata + full message history.
Uses aiosqlite for async access, WAL mode for concurrent reads.
"""

import json
import time
import uuid
from pathlib import Path
from typing import Optional

import aiosqlite

_instance: Optional["SessionDB"] = None


class SessionDB:
    def __init__(self, db_path: str):
        self.db_path = db_path
        self._db: Optional[aiosqlite.Connection] = None

    async def init(self):
        """Initialize database connection and create tables."""
        Path(self.db_path).parent.mkdir(parents=True, exist_ok=True)
        self._db = await aiosqlite.connect(self.db_path)
        self._db.row_factory = aiosqlite.Row
        await self._db.execute("PRAGMA journal_mode=WAL")
        await self._db.execute("PRAGMA busy_timeout=5000")
        await self._create_tables()

    async def _create_tables(self):
        await self._db.executescript("""
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                agent_name TEXT NOT NULL,
                model TEXT,
                started_at REAL NOT NULL,
                ended_at REAL,
                status TEXT NOT NULL DEFAULT 'active',
                message_count INTEGER NOT NULL DEFAULT 0,
                token_usage INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_agent ON sessions(agent_name);
            CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
            CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at);

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT,
                tool_name TEXT,
                tool_args TEXT,
                timestamp REAL NOT NULL,
                tokens INTEGER,
                FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
            CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp);
        """)
        await self._db.commit()

    async def close(self):
        if self._db:
            await self._db.close()
            self._db = None

    # --- Session CRUD ---

    async def create_session(
        self, session_id: str, user_id: str, agent_name: str, model: str = None
    ) -> dict:
        """Create a new session or reactivate existing one."""
        now = time.time()
        # Check if session already exists (reconnect scenario)
        async with self._db.execute(
            "SELECT id, status FROM sessions WHERE id = ? AND user_id = ?",
            (session_id, user_id),
        ) as cursor:
            row = await cursor.fetchone()

        if row:
            # Reactivate existing session
            await self._db.execute(
                "UPDATE sessions SET status = 'active', agent_name = ?, model = ? WHERE id = ?",
                (agent_name, model, session_id),
            )
            await self._db.commit()
            return {"id": session_id, "user_id": user_id, "agent_name": agent_name, "reactivated": True}

        await self._db.execute(
            """INSERT INTO sessions (id, user_id, agent_name, model, started_at, status)
               VALUES (?, ?, ?, ?, ?, 'active')""",
            (session_id, user_id, agent_name, model, now),
        )
        await self._db.commit()
        return {"id": session_id, "user_id": user_id, "agent_name": agent_name, "started_at": now}

    async def end_session(self, session_id: str, status: str = "completed"):
        """Mark session as ended."""
        now = time.time()
        await self._db.execute(
            "UPDATE sessions SET ended_at = ?, status = ? WHERE id = ? AND status = 'active'",
            (now, status, session_id),
        )
        await self._db.commit()

    async def get_session(self, session_id: str) -> Optional[dict]:
        """Get session metadata."""
        async with self._db.execute(
            "SELECT * FROM sessions WHERE id = ?", (session_id,)
        ) as cursor:
            row = await cursor.fetchone()
            return dict(row) if row else None

    async def list_sessions(
        self,
        user_id: str = None,
        agent_name: str = None,
        status: str = None,
        date_from: float = None,
        date_to: float = None,
        limit: int = 50,
        offset: int = 0,
    ) -> list[dict]:
        """List sessions with filters."""
        conditions = []
        params = []

        if user_id:
            conditions.append("user_id = ?")
            params.append(user_id)
        if agent_name:
            conditions.append("agent_name = ?")
            params.append(agent_name)
        if status:
            conditions.append("status = ?")
            params.append(status)
        if date_from:
            conditions.append("started_at >= ?")
            params.append(date_from)
        if date_to:
            conditions.append("started_at <= ?")
            params.append(date_to)

        where = "WHERE " + " AND ".join(conditions) if conditions else ""
        query = f"SELECT * FROM sessions {where} ORDER BY started_at DESC LIMIT ? OFFSET ?"
        params.extend([limit, offset])

        async with self._db.execute(query, params) as cursor:
            rows = await cursor.fetchall()
            return [dict(r) for r in rows]

    async def count_sessions(
        self,
        user_id: str = None,
        agent_name: str = None,
        status: str = None,
    ) -> int:
        """Count sessions matching filters."""
        conditions = []
        params = []
        if user_id:
            conditions.append("user_id = ?")
            params.append(user_id)
        if agent_name:
            conditions.append("agent_name = ?")
            params.append(agent_name)
        if status:
            conditions.append("status = ?")
            params.append(status)

        where = "WHERE " + " AND ".join(conditions) if conditions else ""
        async with self._db.execute(f"SELECT COUNT(*) FROM sessions {where}", params) as cursor:
            row = await cursor.fetchone()
            return row[0]

    async def delete_session(self, session_id: str) -> bool:
        """Delete session and its messages."""
        await self._db.execute("DELETE FROM messages WHERE session_id = ?", (session_id,))
        cursor = await self._db.execute("DELETE FROM sessions WHERE id = ?", (session_id,))
        await self._db.commit()
        return cursor.rowcount > 0

    async def delete_user_sessions(self, user_id: str) -> int:
        """Delete all sessions for a user. Returns count deleted."""
        # Get session ids first for cascading message delete
        async with self._db.execute(
            "SELECT id FROM sessions WHERE user_id = ?", (user_id,)
        ) as cursor:
            rows = await cursor.fetchall()
            session_ids = [r[0] for r in rows]

        if session_ids:
            placeholders = ",".join("?" * len(session_ids))
            await self._db.execute(
                f"DELETE FROM messages WHERE session_id IN ({placeholders})", session_ids
            )
            await self._db.execute("DELETE FROM sessions WHERE user_id = ?", (user_id,))
            await self._db.commit()
        return len(session_ids)

    # --- Messages ---

    async def add_message(
        self,
        session_id: str,
        role: str,
        content: str = None,
        tool_name: str = None,
        tool_args: str = None,
        tokens: int = None,
    ):
        """Add a message to session history."""
        now = time.time()
        await self._db.execute(
            """INSERT INTO messages (session_id, role, content, tool_name, tool_args, timestamp, tokens)
               VALUES (?, ?, ?, ?, ?, ?, ?)""",
            (session_id, role, content, tool_name, tool_args, now, tokens),
        )
        await self._db.execute(
            "UPDATE sessions SET message_count = message_count + 1 WHERE id = ?",
            (session_id,),
        )
        await self._db.commit()

    async def get_messages(
        self, session_id: str, limit: int = 500, offset: int = 0
    ) -> list[dict]:
        """Get messages for a session."""
        async with self._db.execute(
            """SELECT * FROM messages WHERE session_id = ?
               ORDER BY timestamp ASC LIMIT ? OFFSET ?""",
            (session_id, limit, offset),
        ) as cursor:
            rows = await cursor.fetchall()
            return [dict(r) for r in rows]

    async def update_model(self, session_id: str, model: str):
        """Update the model used in a session (e.g. after fallback switch)."""
        await self._db.execute(
            "UPDATE sessions SET model = ? WHERE id = ?", (model, session_id)
        )
        await self._db.commit()

    async def update_tokens(self, session_id: str, tokens: int):
        """Increment total token usage for session."""
        await self._db.execute(
            "UPDATE sessions SET token_usage = COALESCE(token_usage, 0) + ? WHERE id = ?",
            (tokens, session_id),
        )
        await self._db.commit()

    # --- Statistics ---

    async def get_stats_summary(self) -> dict:
        """Get overall statistics."""
        stats = {}
        async with self._db.execute("SELECT COUNT(*) FROM sessions") as c:
            stats["total_sessions"] = (await c.fetchone())[0]
        async with self._db.execute("SELECT COUNT(*) FROM sessions WHERE status = 'active'") as c:
            stats["active_sessions"] = (await c.fetchone())[0]
        async with self._db.execute("SELECT COUNT(DISTINCT user_id) FROM sessions") as c:
            stats["unique_users"] = (await c.fetchone())[0]
        async with self._db.execute("SELECT COUNT(*) FROM messages") as c:
            stats["total_messages"] = (await c.fetchone())[0]
        async with self._db.execute("SELECT COALESCE(SUM(token_usage), 0) FROM sessions") as c:
            stats["total_tokens"] = (await c.fetchone())[0]
        return stats

    async def get_stats_by_agent(self) -> list[dict]:
        """Get session/message counts per agent."""
        async with self._db.execute("""
            SELECT agent_name,
                   COUNT(*) as sessions,
                   SUM(message_count) as messages,
                   COALESCE(SUM(token_usage), 0) as tokens
            FROM sessions GROUP BY agent_name ORDER BY sessions DESC
        """) as cursor:
            return [dict(r) for r in await cursor.fetchall()]

    async def get_stats_by_model(self) -> list[dict]:
        """Get usage distribution by model."""
        async with self._db.execute("""
            SELECT model,
                   COUNT(*) as sessions,
                   SUM(message_count) as messages,
                   COALESCE(SUM(token_usage), 0) as tokens
            FROM sessions WHERE model IS NOT NULL GROUP BY model ORDER BY sessions DESC
        """) as cursor:
            return [dict(r) for r in await cursor.fetchall()]

    async def get_stats_by_user(self) -> list[dict]:
        """Get usage stats per user."""
        async with self._db.execute("""
            SELECT user_id,
                   COUNT(*) as sessions,
                   SUM(message_count) as messages,
                   COALESCE(SUM(token_usage), 0) as tokens,
                   MAX(started_at) as last_active
            FROM sessions GROUP BY user_id ORDER BY last_active DESC
        """) as cursor:
            return [dict(r) for r in await cursor.fetchall()]

    async def get_stats_daily(self, days: int = 30) -> list[dict]:
        """Get daily usage stats."""
        cutoff = time.time() - days * 86400
        async with self._db.execute("""
            SELECT date(started_at, 'unixepoch') as day,
                   COUNT(*) as sessions,
                   SUM(message_count) as messages,
                   COUNT(DISTINCT user_id) as users
            FROM sessions WHERE started_at >= ?
            GROUP BY day ORDER BY day DESC
        """, (cutoff,)) as cursor:
            return [dict(r) for r in await cursor.fetchall()]


async def get_db(db_path: str = None) -> SessionDB:
    """Get or create singleton SessionDB instance."""
    global _instance
    if _instance is None:
        if db_path is None:
            db_path = str(Path(__file__).parent / "shared" / "sessions.db")
        _instance = SessionDB(db_path)
        await _instance.init()
    return _instance
