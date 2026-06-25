"""
Multi-Agent System — Individual Agent Process
Each agent runs as a FastAPI server with:
- REST API for programmatic access
- WebSocket endpoint for real-time chat
- Tool execution in isolated sandbox
- Shared storage access for inter-agent communication
"""

import asyncio
import json
import os
import subprocess
import time
import uuid
from pathlib import Path
from typing import Optional

import httpx
import yaml
from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel


class ChatMessage(BaseModel):
    role: str  # user | assistant | system | tool
    content: str
    tool_call: Optional[dict] = None
    timestamp: Optional[float] = None


class ChatRequest(BaseModel):
    message: str
    session_id: Optional[str] = None


class TaskMessage(BaseModel):
    from_agent: str
    to_agent: str
    task: str
    context: Optional[dict] = None


class AgentServer:
    def __init__(self, config: dict, shared_config: dict):
        self.name = config["name"]
        self.description = config["description"]
        self.port = config["port"]
        self.sandbox_dir = Path(config["sandbox_dir"]).resolve()
        self.llm_config = config["llm"]
        self.system_prompt = config["system_prompt"]
        self.tools_list = config.get("tools", [])
        self.shared = shared_config

        # Ensure directories exist
        self.sandbox_dir.mkdir(parents=True, exist_ok=True)
        for d in ["tasks_dir", "results_dir", "mailbox_dir", "context_dir"]:
            Path(self.shared[d]).mkdir(parents=True, exist_ok=True)

        # Session storage: session_id -> list of messages
        self.sessions: dict[str, list[dict]] = {}

        # Build FastAPI app
        self.app = FastAPI(
            title=f"Agent: {self.name}",
            description=self.description,
        )
        self.app.add_middleware(
            CORSMiddleware,
            allow_origins=["*"],
            allow_methods=["*"],
            allow_headers=["*"],
        )
        self._setup_routes()

    def _setup_routes(self):
        app = self.app

        @app.get("/health")
        async def health():
            return {"agent": self.name, "status": "ok", "port": self.port}

        @app.get("/info")
        async def info():
            return {
                "name": self.name,
                "description": self.description,
                "model": self.llm_config["model"],
                "tools": self.tools_list,
            }

        @app.post("/chat")
        async def chat(req: ChatRequest):
            """Synchronous chat — send message, get full response."""
            session_id = req.session_id or str(uuid.uuid4())
            if session_id not in self.sessions:
                self.sessions[session_id] = []

            self.sessions[session_id].append(
                {"role": "user", "content": req.message, "timestamp": time.time()}
            )

            response = await self._llm_chat(self.sessions[session_id])

            self.sessions[session_id].append(
                {"role": "assistant", "content": response, "timestamp": time.time()}
            )

            return {
                "session_id": session_id,
                "response": response,
                "agent": self.name,
            }

        @app.websocket("/ws/{session_id}")
        async def websocket_chat(websocket: WebSocket, session_id: str):
            """WebSocket streaming chat for real-time interaction."""
            await websocket.accept()

            if session_id not in self.sessions:
                self.sessions[session_id] = []

            try:
                while True:
                    data = await websocket.receive_text()
                    msg = json.loads(data)

                    self.sessions[session_id].append(
                        {"role": "user", "content": msg["content"], "timestamp": time.time()}
                    )

                    # Stream response
                    full_response = ""
                    async for chunk in self._llm_chat_stream(self.sessions[session_id]):
                        full_response += chunk
                        await websocket.send_text(
                            json.dumps({"type": "chunk", "content": chunk})
                        )

                    # Check for tool calls in response
                    tool_result = await self._handle_tool_calls(full_response)
                    if tool_result:
                        full_response += f"\n\n**Tool result:**\n```\n{tool_result}\n```"
                        await websocket.send_text(
                            json.dumps({"type": "tool_result", "content": tool_result})
                        )

                    self.sessions[session_id].append(
                        {"role": "assistant", "content": full_response, "timestamp": time.time()}
                    )

                    await websocket.send_text(
                        json.dumps({"type": "done", "content": full_response})
                    )

            except WebSocketDisconnect:
                pass

        @app.post("/task")
        async def receive_task(task: TaskMessage):
            """Receive a task from another agent via shared mailbox."""
            task_file = Path(self.shared["mailbox_dir"]) / f"{self.name}" / f"{uuid.uuid4()}.json"
            task_file.parent.mkdir(parents=True, exist_ok=True)
            task_file.write_text(json.dumps(task.model_dump(), ensure_ascii=False, indent=2))
            return {"status": "queued", "task_id": task_file.stem}

        @app.get("/tasks")
        async def list_pending_tasks():
            """List pending tasks in this agent's mailbox."""
            mailbox = Path(self.shared["mailbox_dir"]) / self.name
            if not mailbox.exists():
                return {"tasks": []}
            tasks = []
            for f in sorted(mailbox.glob("*.json")):
                tasks.append(json.loads(f.read_text()))
            return {"tasks": tasks}

        @app.post("/delegate")
        async def delegate_task(task: TaskMessage):
            """Delegate a task to another agent."""
            target_port = self._get_agent_port(task.to_agent)
            if not target_port:
                raise HTTPException(404, f"Agent '{task.to_agent}' not found")
            async with httpx.AsyncClient() as client:
                resp = await client.post(
                    f"http://localhost:{target_port}/task",
                    json=task.model_dump(),
                    timeout=10,
                )
            return resp.json()

        @app.get("/shared/files")
        async def list_shared_files():
            """List files in shared public storage."""
            shared_dir = Path(self.shared["public_dir"])
            files = []
            for f in shared_dir.rglob("*"):
                if f.is_file():
                    files.append(str(f.relative_to(shared_dir)))
            return {"files": files}

    def _get_agent_port(self, agent_name: str) -> Optional[int]:
        """Look up agent port from config."""
        # Read config to find other agents
        config_path = Path(__file__).parent / "config.yaml"
        if config_path.exists():
            cfg = yaml.safe_load(config_path.read_text())
            for agent in cfg.get("agents", []):
                if agent["name"] == agent_name:
                    return agent["port"]
        return None

    async def _llm_chat(self, messages: list[dict]) -> str:
        """Call LLM API (OpenAI-compatible) for a complete response."""
        api_messages = [{"role": "system", "content": self.system_prompt}]
        for m in messages:
            api_messages.append({"role": m["role"], "content": m["content"]})

        models_to_try = [self.llm_config["model"]]
        fallback = self.llm_config.get("fallback_model")
        if fallback:
            models_to_try.append(fallback)

        last_error = None
        for model in models_to_try:
            for attempt in range(2):
                try:
                    async with httpx.AsyncClient(timeout=60) as client:
                        resp = await client.post(
                            f"{self.llm_config['base_url']}/chat/completions",
                            headers={"Authorization": f"Bearer {self.llm_config['api_key']}"},
                            json={
                                "model": model,
                                "messages": api_messages,
                                "temperature": 0.7,
                                "max_tokens": 4096,
                            },
                        )
                        if resp.status_code == 429:
                            print(f"[{self.name}] 429 on {model}, switching...")
                            break  # Immediately try next model
                        if resp.status_code >= 500:
                            print(f"[{self.name}] {resp.status_code} on {model}, retry...")
                            await asyncio.sleep(2)
                            continue
                        resp.raise_for_status()
                        data = resp.json()
                        return data["choices"][0]["message"]["content"]
                except httpx.ReadTimeout:
                    print(f"[{self.name}] Timeout on {model}, trying next...")
                    last_error = Exception(f"Timeout on {model}")
                    break  # Try next model
                except Exception as e:
                    last_error = e
                    break  # Try next model

        raise Exception(f"All models failed: {last_error}")

    async def _llm_chat_stream(self, messages: list[dict]):
        """Stream LLM response token by token."""
        api_messages = [{"role": "system", "content": self.system_prompt}]
        for m in messages:
            api_messages.append({"role": m["role"], "content": m["content"]})

        async with httpx.AsyncClient(timeout=300) as client:
            async with client.stream(
                "POST",
                f"{self.llm_config['base_url']}/chat/completions",
                headers={"Authorization": f"Bearer {self.llm_config['api_key']}"},
                json={
                    "model": self.llm_config["model"],
                    "messages": api_messages,
                    "temperature": 0.7,
                    "max_tokens": 4096,
                    "stream": True,
                },
            ) as resp:
                async for line in resp.aiter_lines():
                    if line.startswith("data: "):
                        payload = line[6:]
                        if payload.strip() == "[DONE]":
                            break
                        try:
                            chunk = json.loads(payload)
                            delta = chunk["choices"][0].get("delta", {})
                            content = delta.get("content", "")
                            if content:
                                yield content
                        except (json.JSONDecodeError, KeyError, IndexError):
                            continue

    async def _handle_tool_calls(self, response: str) -> Optional[str]:
        """Parse and execute tool calls from LLM response."""
        # Simple pattern: ```tool:shell\ncommand\n```
        import re
        tool_match = re.search(r"```tool:(\w+)\n(.*?)```", response, re.DOTALL)
        if not tool_match:
            return None

        tool_name = tool_match.group(1)
        tool_input = tool_match.group(2).strip()

        if tool_name == "shell" and "shell" in self.tools_list:
            return self._execute_shell(tool_input)
        elif tool_name == "filesystem" and "filesystem" in self.tools_list:
            return self._execute_filesystem(tool_input)
        return None

    def _execute_shell(self, command: str) -> str:
        """Execute shell command in agent's sandbox."""
        try:
            result = subprocess.run(
                ["bash", "-c", command],
                cwd=str(self.sandbox_dir),
                capture_output=True,
                text=True,
                timeout=30,
                env={
                    **os.environ,
                    "HOME": str(self.sandbox_dir),
                    "AGENT_NAME": self.name,
                    "SHARED_DIR": str(Path(self.shared["public_dir"]).resolve()),
                },
            )
            output = result.stdout
            if result.stderr:
                output += f"\nSTDERR: {result.stderr}"
            return output[:4096]  # Truncate
        except subprocess.TimeoutExpired:
            return "ERROR: Command timed out (30s)"
        except Exception as e:
            return f"ERROR: {e}"

    def _execute_filesystem(self, instruction: str) -> str:
        """Execute filesystem operation in sandbox."""
        lines = instruction.strip().split("\n", 1)
        op = lines[0].strip().lower()
        arg = lines[1] if len(lines) > 1 else ""

        sandbox = self.sandbox_dir
        shared = Path(self.shared["public_dir"]).resolve()

        if op == "read":
            target = (sandbox / arg.strip()).resolve()
            # Allow reading from sandbox or shared
            if not (str(target).startswith(str(sandbox)) or str(target).startswith(str(shared))):
                return "ERROR: Access denied — outside sandbox/shared"
            if target.exists():
                return target.read_text()[:8192]
            return "ERROR: File not found"
        elif op == "write":
            parts = arg.split("\n", 1)
            filepath = (sandbox / parts[0].strip()).resolve()
            if not str(filepath).startswith(str(sandbox)):
                return "ERROR: Can only write to sandbox"
            filepath.parent.mkdir(parents=True, exist_ok=True)
            filepath.write_text(parts[1] if len(parts) > 1 else "")
            return f"Written: {filepath.relative_to(sandbox)}"
        elif op == "ls":
            target = (sandbox / arg.strip()).resolve() if arg.strip() else sandbox
            if target.exists():
                return "\n".join(str(f.relative_to(target)) for f in target.iterdir())
            return "ERROR: Directory not found"

        return f"ERROR: Unknown filesystem op: {op}"


def load_agent_config(agent_name: str) -> tuple[dict, dict]:
    """Load agent and shared config from config.yaml."""
    config_path = Path(__file__).parent / "config.yaml"
    cfg = yaml.safe_load(config_path.read_text())

    shared = cfg["shared"]
    llm_defaults = cfg.get("llm_defaults", {})

    for agent in cfg["agents"]:
        if agent["name"] == agent_name:
            # Merge llm_defaults with agent-specific llm (agent overrides)
            agent_llm = agent.get("llm", {})
            agent["llm"] = {**llm_defaults, **agent_llm}
            return agent, shared

    raise ValueError(f"Agent '{agent_name}' not found in config.yaml")


def create_app(agent_name: str) -> FastAPI:
    """Factory function for uvicorn."""
    agent_cfg, shared_cfg = load_agent_config(agent_name)
    server = AgentServer(agent_cfg, shared_cfg)
    return server.app


if __name__ == "__main__":
    import sys
    import uvicorn

    if len(sys.argv) < 2:
        print("Usage: python agent_server.py <agent_name>")
        print("Available agents: planner, coder, researcher, reviewer")
        sys.exit(1)

    agent_name = sys.argv[1]
    agent_cfg, shared_cfg = load_agent_config(agent_name)

    # Bind only to internal interfaces:
    # - AGENT_BIND_HOST env var (e.g. Tailscale IP)
    # - Default: 127.0.0.1 (localhost only)
    bind_host = os.environ.get("AGENT_BIND_HOST", "127.0.0.1")

    print(f"Starting agent '{agent_name}' on {bind_host}:{agent_cfg['port']}...")
    server = AgentServer(agent_cfg, shared_cfg)

    uvicorn.run(server.app, host=bind_host, port=agent_cfg["port"])
