"""
Shared Storage Manager — межагентный обмен через файловую систему.
Предоставляет:
- Task queue (producer-consumer)
- Mailbox (point-to-point)
- Shared context (broadcast)
- File locking для конкурентного доступа
"""

import json
import os
import time
import uuid
from pathlib import Path
from typing import Optional
import fcntl


class SharedStorage:
    def __init__(self, base_dir: str = "./shared"):
        self.base = Path(base_dir).resolve()
        self.tasks_dir = self.base / "tasks"
        self.results_dir = self.base / "results"
        self.mailbox_dir = self.base / "mailbox"
        self.context_dir = self.base / "context"

        for d in [self.tasks_dir, self.results_dir, self.mailbox_dir, self.context_dir]:
            d.mkdir(parents=True, exist_ok=True)

    # --- Task Queue ---

    def publish_task(self, task: dict, priority: int = 5) -> str:
        """Publish a task to the shared queue. Returns task_id."""
        task_id = str(uuid.uuid4())[:8]
        task_data = {
            "id": task_id,
            "priority": priority,
            "status": "pending",
            "created_at": time.time(),
            "assigned_to": task.get("assigned_to"),
            "from_agent": task.get("from_agent", "unknown"),
            "payload": task,
        }
        filepath = self.tasks_dir / f"{priority:02d}_{task_id}.json"
        self._atomic_write(filepath, task_data)
        return task_id

    def claim_task(self, agent_name: str) -> Optional[dict]:
        """Claim the highest-priority pending task for this agent."""
        candidates = sorted(self.tasks_dir.glob("*.json"))
        for f in candidates:
            if f.name.endswith(".locked"):
                continue
            try:
                data = json.loads(f.read_text())
            except (json.JSONDecodeError, FileNotFoundError):
                continue

            if data["status"] != "pending":
                continue
            if data.get("assigned_to") and data["assigned_to"] != agent_name:
                continue

            # Try to claim via rename (atomic on same filesystem)
            locked = f.with_suffix(".locked")
            try:
                f.rename(locked)
            except FileNotFoundError:
                continue  # Another agent grabbed it

            data["status"] = "in_progress"
            data["claimed_by"] = agent_name
            data["claimed_at"] = time.time()
            self._atomic_write(locked, data)
            return data

        return None

    def complete_task(self, task_id: str, result: dict):
        """Mark task as completed and store result."""
        # Find the locked task file
        for f in self.tasks_dir.glob(f"*{task_id}*.locked"):
            data = json.loads(f.read_text())
            data["status"] = "completed"
            data["completed_at"] = time.time()
            self._atomic_write(f, data)

            # Write result
            result_file = self.results_dir / f"{task_id}.json"
            self._atomic_write(result_file, {
                "task_id": task_id,
                "result": result,
                "completed_at": time.time(),
            })
            return

    # --- Mailbox (point-to-point) ---

    def send_message(self, from_agent: str, to_agent: str, content: str, metadata: Optional[dict] = None):
        """Send a message to another agent's mailbox."""
        mailbox = self.mailbox_dir / to_agent
        mailbox.mkdir(parents=True, exist_ok=True)

        msg = {
            "id": str(uuid.uuid4())[:8],
            "from": from_agent,
            "to": to_agent,
            "content": content,
            "metadata": metadata or {},
            "timestamp": time.time(),
            "read": False,
        }
        filepath = mailbox / f"{int(time.time())}_{msg['id']}.json"
        self._atomic_write(filepath, msg)

    def read_messages(self, agent_name: str, mark_read: bool = True) -> list[dict]:
        """Read all unread messages for an agent."""
        mailbox = self.mailbox_dir / agent_name
        if not mailbox.exists():
            return []

        messages = []
        for f in sorted(mailbox.glob("*.json")):
            try:
                msg = json.loads(f.read_text())
            except (json.JSONDecodeError, FileNotFoundError):
                continue

            if not msg.get("read"):
                messages.append(msg)
                if mark_read:
                    msg["read"] = True
                    self._atomic_write(f, msg)

        return messages

    # --- Shared Context (broadcast) ---

    def publish_context(self, key: str, data: dict, agent_name: str):
        """Publish shared context document available to all agents."""
        doc = {
            "key": key,
            "data": data,
            "published_by": agent_name,
            "updated_at": time.time(),
        }
        filepath = self.context_dir / f"{key}.json"
        self._atomic_write(filepath, doc)

    def get_context(self, key: str) -> Optional[dict]:
        """Get a shared context document."""
        filepath = self.context_dir / f"{key}.json"
        if filepath.exists():
            return json.loads(filepath.read_text())
        return None

    def list_context(self) -> list[str]:
        """List all available context keys."""
        return [f.stem for f in self.context_dir.glob("*.json")]

    # --- Utilities ---

    def _atomic_write(self, filepath: Path, data: dict):
        """Write JSON atomically (write to tmp, then rename)."""
        tmp = filepath.with_suffix(".tmp")
        tmp.write_text(json.dumps(data, ensure_ascii=False, indent=2))
        tmp.rename(filepath)
