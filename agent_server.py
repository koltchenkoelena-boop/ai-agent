"""
Multi-Agent System — Agent Mode
Each agent runs as a FastAPI server with:
- OpenAI-compatible function calling (tool use)
- Agentic loop: LLM → tool_calls → execute → observe → repeat
- WebSocket streaming with live tool execution
- Isolated sandbox + shared storage
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
from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException, Query
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

import auth
import session_db


# --- Tool Definitions (OpenAI format) ---

TOOL_DEFINITIONS = {
    "shell": {
        "type": "function",
        "function": {
            "name": "shell",
            "description": "Execute a bash command in the agent's sandbox directory. Use for running scripts, installing packages, compiling code, checking system state.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute"
                    }
                },
                "required": ["command"]
            }
        }
    },
    "filesystem": {
        "type": "function",
        "function": {
            "name": "filesystem",
            "description": "Read, write, or list files. Operations: read (read file content), write (create/overwrite file), append (append to file), ls (list directory).",
            "parameters": {
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["read", "write", "append", "ls"],
                        "description": "The filesystem operation to perform"
                    },
                    "path": {
                        "type": "string",
                        "description": "Relative path within sandbox or shared directory"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write (for write/append operations)"
                    }
                },
                "required": ["operation", "path"]
            }
        }
    },
    "delegate_task": {
        "type": "function",
        "function": {
            "name": "delegate_task",
            "description": "Delegate a task to another agent (coder, researcher, reviewer, planner). The task will be queued in their mailbox.",
            "parameters": {
                "type": "object",
                "properties": {
                    "to_agent": {
                        "type": "string",
                        "enum": ["planner", "coder", "researcher", "reviewer"],
                        "description": "Name of the agent to delegate to"
                    },
                    "task": {
                        "type": "string",
                        "description": "Description of the task to delegate"
                    },
                    "context": {
                        "type": "string",
                        "description": "Additional context or data for the task"
                    }
                },
                "required": ["to_agent", "task"]
            }
        }
    },
    "web_search": {
        "type": "function",
        "function": {
            "name": "web_search",
            "description": "Search the web for information or images. Set type='images' to find pictures. Returns text snippets or image URLs in markdown format.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["text", "images"],
                        "description": "Search type: 'text' for web pages, 'images' for pictures. Default: text"
                    }
                },
                "required": ["query"]
            }
        }
    },
    "run_tests": {
        "type": "function",
        "function": {
            "name": "run_tests",
            "description": "Run tests or validation commands. Specify the test command to execute.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Test command to run (e.g., 'python -m pytest', 'npm test')"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Working directory relative to sandbox (default: sandbox root)"
                    }
                },
                "required": ["command"]
            }
        }
    },
    "code_execute": {
        "type": "function",
        "function": {
            "name": "code_execute",
            "description": "Execute a code snippet directly. Supports python3 and node.",
            "parameters": {
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "enum": ["python", "node", "bash"],
                        "description": "Programming language"
                    },
                    "code": {
                        "type": "string",
                        "description": "Code to execute"
                    }
                },
                "required": ["language", "code"]
            }
        }
    },
    "read_documents": {
        "type": "function",
        "function": {
            "name": "read_documents",
            "description": "Read and analyze documents from the shared context or sandbox.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the document (relative to shared/ or sandbox/)"
                    },
                    "query": {
                        "type": "string",
                        "description": "What to look for in the document (optional filter)"
                    }
                },
                "required": ["path"]
            }
        }
    },
    "user_memory": {
        "type": "function",
        "function": {
            "name": "user_memory",
            "description": "Save or recall facts about the current user. Use 'save' to remember something about the user (name, preferences, context). Use 'recall' to retrieve all saved facts. ALWAYS use save when user asks you to remember something.",
            "parameters": {
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["save", "recall"],
                        "description": "'save' to store a new fact, 'recall' to get all stored facts"
                    },
                    "fact": {
                        "type": "string",
                        "description": "The fact to save (required for 'save' operation). Write concisely: 'Имя: Ирина', 'Любит: кофе без сахара'"
                    }
                },
                "required": ["operation"]
            }
        }
    },
}

MAX_TOOL_ITERATIONS = 10

# Dispatcher notification port (fire-and-forget)
DISPATCHER_PORT = 8005


class AuthRequest(BaseModel):
    username: str
    password: str


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
        self.system_prompt = config["system_prompt"] + (
            "\n\nВАЖНО ПРО КАРТИНКИ: Если ты нашёл или хочешь показать изображение, "
            "ВСЕГДА вставляй его в финальный ответ как markdown: ![описание](ПРЯМОЙ_URL). "
            "Используй прямой URL картинки (.jpg/.png/.webp), а не ссылку на страницу. "
            "Бери URL из результатов web_search. Не пиши просто «вот картинка» без markdown-разметки."
        )
        self.tools_list = config.get("tools", [])
        self.shared = shared_config

        # Build tool definitions for this agent
        self.tools_schema = [TOOL_DEFINITIONS[t] for t in self.tools_list if t in TOOL_DEFINITIONS]

        # Ensure directories exist
        self.sandbox_dir.mkdir(parents=True, exist_ok=True)
        for d in ["tasks_dir", "results_dir", "mailbox_dir", "context_dir"]:
            Path(self.shared[d]).mkdir(parents=True, exist_ok=True)

        # Session storage: keyed by "user:session_id"
        self.sessions: dict[str, list[dict]] = {}

        # Session DB (persistent SQLite)
        self.session_db: Optional[session_db.SessionDB] = None

        # Build FastAPI app
        self.app = FastAPI(title=f"Agent: {self.name}", description=self.description)
        self.app.add_middleware(
            CORSMiddleware, allow_origins=["*"], allow_methods=["*"], allow_headers=["*"]
        )
        self._setup_routes()

        # Register startup handler via router event
        @self.app.on_event("startup")
        async def _startup():
            await self._init_session_db()

    async def _init_session_db(self):
        """Initialize persistent session database."""
        db_path = str(Path(self.shared["public_dir"]).resolve() / "sessions.db")
        self.session_db = await session_db.get_db(db_path)

    async def _notify_dispatcher(self, endpoint: str, data: dict):
        """Fire-and-forget notification to dispatcher. Failures are logged, not raised."""
        try:
            bind_host = os.environ.get("AGENT_BIND_HOST", "127.0.0.1")
            async with httpx.AsyncClient(timeout=3) as client:
                await client.post(
                    f"http://{bind_host}:{DISPATCHER_PORT}/event/{endpoint}",
                    json=data,
                )
        except Exception:
            pass  # Dispatcher unavailable — non-critical

    def _setup_routes(self):
        app = self.app

        @app.get("/health")
        async def health():
            return {"agent": self.name, "status": "ok", "port": self.port, "mode": "agent"}

        @app.get("/info")
        async def info():
            return {
                "name": self.name,
                "description": self.description,
                "model": self.llm_config["model"],
                "mode": "agent",
                "tools": [t["function"]["name"] for t in self.tools_schema],
            }

        # --- Auth endpoints ---

        @app.post("/auth/signup")
        async def signup(req: AuthRequest):
            try:
                result = auth.signup(req.username, req.password)
                return result
            except ValueError as e:
                raise HTTPException(400, str(e))

        @app.post("/auth/login")
        async def login(req: AuthRequest):
            try:
                result = auth.login(req.username, req.password)
                return result
            except ValueError as e:
                raise HTTPException(401, str(e))

        @app.get("/auth/me")
        async def me(token: str = Query(...)):
            username = auth.verify_token(token)
            if not username:
                raise HTTPException(401, "Invalid or expired token")
            user = auth.get_user(username)
            if not user:
                raise HTTPException(404, "User not found")
            return user

        @app.post("/chat")
        async def chat(req: ChatRequest, token: str = Query(...)):
            """Synchronous agentic chat — full tool-use loop."""
            username = auth.verify_token(token)
            if not username:
                raise HTTPException(401, "Invalid or expired token")

            session_id = req.session_id or str(uuid.uuid4())
            session_key = f"{username}:{session_id}"
            if session_key not in self.sessions:
                self.sessions[session_key] = []

            self.sessions[session_key].append(
                {"role": "user", "content": req.message, "timestamp": time.time()}
            )

            response, tool_log = await self._agent_loop(self.sessions[session_key], username)

            self.sessions[session_key].append(
                {"role": "assistant", "content": response, "timestamp": time.time()}
            )

            return {
                "session_id": session_id,
                "response": response,
                "agent": self.name,
                "tools_used": tool_log,
            }

        @app.websocket("/ws/{session_id}")
        async def websocket_chat(websocket: WebSocket, session_id: str, token: str = Query(None)):
            """WebSocket streaming agentic chat with live tool execution."""
            # Verify token
            username = auth.verify_token(token) if token else None
            if not username:
                await websocket.close(code=4001, reason="Unauthorized")
                return

            await websocket.accept()
            session_key = f"{username}:{session_id}"
            print(f"[{self.name}] WS connected: user={username} session={session_id}")

            if session_key not in self.sessions:
                self.sessions[session_key] = []

            # Persist session start to DB + notify dispatcher
            model_name = self.llm_config.get("model")
            if self.session_db:
                await self.session_db.create_session(
                    session_id=session_id,
                    user_id=username,
                    agent_name=self.name,
                    model=model_name,
                )
            asyncio.create_task(self._notify_dispatcher("connect", {
                "session_id": session_id,
                "user_id": username,
                "agent_name": self.name,
                "model": model_name,
            }))

            try:
                while True:
                    data = await websocket.receive_text()
                    msg = json.loads(data)
                    print(f"[{self.name}] WS message ({username}): {msg['content'][:80]}")

                    self.sessions[session_key].append(
                        {"role": "user", "content": msg["content"], "timestamp": time.time()}
                    )

                    # Persist user message
                    if self.session_db:
                        await self.session_db.add_message(
                            session_id=session_id, role="user", content=msg["content"]
                        )

                    try:
                        full_response = await self._agent_loop_streaming(
                            self.sessions[session_key], websocket, username, session_id=session_id
                        )
                    except Exception as e:
                        error_msg = f"Ошибка: {type(e).__name__}: {e}"
                        print(f"[{self.name}] Agent loop error: {error_msg}")
                        await websocket.send_text(
                            json.dumps({"type": "chunk", "content": error_msg})
                        )
                        full_response = error_msg

                    self.sessions[session_key].append(
                        {"role": "assistant", "content": full_response, "timestamp": time.time()}
                    )

                    # Persist assistant response
                    if self.session_db:
                        await self.session_db.add_message(
                            session_id=session_id, role="assistant", content=full_response
                        )

                    await websocket.send_text(
                        json.dumps({"type": "done", "content": full_response})
                    )

            except WebSocketDisconnect:
                print(f"[{self.name}] WS disconnected: user={username} session={session_id}")
                # Mark session as completed in DB + notify dispatcher
                if self.session_db:
                    await self.session_db.end_session(session_id)
                asyncio.create_task(self._notify_dispatcher("disconnect", {
                    "session_id": session_id,
                    "user_id": username,
                    "agent_name": self.name,
                }))

        @app.post("/task")
        async def receive_task(task: TaskMessage):
            task_file = Path(self.shared["mailbox_dir"]) / self.name / f"{uuid.uuid4()}.json"
            task_file.parent.mkdir(parents=True, exist_ok=True)
            task_file.write_text(json.dumps(task.model_dump(), ensure_ascii=False, indent=2))
            return {"status": "queued", "task_id": task_file.stem}

        @app.get("/tasks")
        async def list_pending_tasks():
            mailbox = Path(self.shared["mailbox_dir"]) / self.name
            if not mailbox.exists():
                return {"tasks": []}
            tasks = []
            for f in sorted(mailbox.glob("*.json")):
                tasks.append(json.loads(f.read_text()))
            return {"tasks": tasks}

        @app.post("/delegate")
        async def delegate_task(task: TaskMessage):
            target_port = self._get_agent_port(task.to_agent)
            if not target_port:
                raise HTTPException(404, f"Agent '{task.to_agent}' not found")
            bind_host = os.environ.get("AGENT_BIND_HOST", "127.0.0.1")
            async with httpx.AsyncClient() as client:
                resp = await client.post(
                    f"http://{bind_host}:{target_port}/task",
                    json=task.model_dump(),
                    timeout=10,
                )
            return resp.json()

        @app.get("/shared/files")
        async def list_shared_files():
            shared_dir = Path(self.shared["public_dir"])
            files = []
            for f in shared_dir.rglob("*"):
                if f.is_file():
                    files.append(str(f.relative_to(shared_dir)))
            return {"files": files}

    # --- Agent Loop (non-streaming) ---

    async def _agent_loop(self, messages: list[dict], username: str) -> tuple[str, list[dict]]:
        """Run agentic tool-use loop until final answer."""
        memory = self._load_user_memory(username)
        system = self.system_prompt
        if memory:
            system += f"\n\nФакты о пользователе ({username}):\n" + "\n".join(f"- {f}" for f in memory)
        api_messages = [{"role": "system", "content": system}]
        for m in messages:
            api_messages.append({"role": m["role"], "content": m["content"]})

        tool_log = []

        for iteration in range(MAX_TOOL_ITERATIONS):
            response_msg = await self._llm_call(api_messages)
            api_messages.append(response_msg)

            # Check for tool calls
            tool_calls = response_msg.get("tool_calls")
            if not tool_calls:
                return response_msg.get("content", ""), tool_log

            # Execute tools
            for tc in tool_calls:
                func_name = tc["function"]["name"]
                try:
                    args = json.loads(tc["function"]["arguments"])
                except json.JSONDecodeError:
                    args = {"raw": tc["function"]["arguments"]}

                print(f"[{self.name}] Tool: {func_name}({json.dumps(args, ensure_ascii=False)[:100]})")
                result = await self._execute_tool(func_name, args, username)
                tool_log.append({"tool": func_name, "args": args, "result": result[:500]})

                api_messages.append({
                    "role": "tool",
                    "tool_call_id": tc["id"],
                    "content": result,
                })

        return "Достигнут лимит итераций агента.", tool_log

    # --- Agent Loop (streaming) ---

    async def _agent_loop_streaming(self, messages: list[dict], ws: WebSocket, username: str, session_id: str = None) -> str:
        """Run agentic loop with WebSocket streaming."""
        memory = self._load_user_memory(username)
        system = self.system_prompt
        if memory:
            system += f"\n\nФакты о пользователе ({username}):\n" + "\n".join(f"- {f}" for f in memory)
        api_messages = [{"role": "system", "content": system}]
        for m in messages:
            api_messages.append({"role": m["role"], "content": m["content"]})

        full_text_parts = []

        for iteration in range(MAX_TOOL_ITERATIONS):
            response_msg, streamed_text = await self._llm_call_stream(api_messages, ws)
            api_messages.append(response_msg)

            if streamed_text:
                full_text_parts.append(streamed_text)

            tool_calls = response_msg.get("tool_calls")
            if not tool_calls:
                return "".join(full_text_parts)

            # Execute tools and notify client
            for tc in tool_calls:
                func_name = tc["function"]["name"]
                try:
                    args = json.loads(tc["function"]["arguments"])
                except json.JSONDecodeError:
                    args = {"raw": tc["function"]["arguments"]}

                print(f"[{self.name}] Tool: {func_name}({json.dumps(args, ensure_ascii=False)[:100]})")

                await ws.send_text(json.dumps({
                    "type": "tool_call",
                    "tool": func_name,
                    "args": args,
                }))

                result = await self._execute_tool(func_name, args, username)

                await ws.send_text(json.dumps({
                    "type": "tool_result",
                    "tool": func_name,
                    "result": result[:2000],
                }))

                # Persist tool call + result to session DB
                if session_id and self.session_db:
                    await self.session_db.add_message(
                        session_id=session_id, role="tool_call",
                        tool_name=func_name,
                        tool_args=json.dumps(args, ensure_ascii=False),
                    )
                    await self.session_db.add_message(
                        session_id=session_id, role="tool_result",
                        content=result[:2000], tool_name=func_name,
                    )

                api_messages.append({
                    "role": "tool",
                    "tool_call_id": tc["id"],
                    "content": result,
                })

        return "".join(full_text_parts) + "\n\n[Достигнут лимит итераций]"

    # --- LLM Calls ---

    async def _llm_call(self, messages: list[dict]) -> dict:
        """Single LLM call with tools. Returns assistant message dict."""
        request_body = {
            "model": self.llm_config["model"],
            "messages": messages,
            "temperature": 0.7,
            "max_tokens": 4096,
        }
        if self.tools_schema:
            request_body["tools"] = self.tools_schema
            request_body["tool_choice"] = "auto"

        models_to_try = [
            {
                "model": self.llm_config["model"],
                "base_url": self.llm_config["base_url"],
                "api_key": self.llm_config["api_key"],
            }
        ]
        fallback = self.llm_config.get("fallback_model")
        if fallback:
            models_to_try.append({
                "model": fallback,
                "base_url": self.llm_config.get("fallback_base_url", self.llm_config["base_url"]),
                "api_key": self.llm_config.get("fallback_api_key", self.llm_config["api_key"]),
            })

        last_error = None
        for m in models_to_try:
            request_body["model"] = m["model"]
            try:
                async with httpx.AsyncClient(timeout=120) as client:
                    resp = await client.post(
                        f"{m['base_url']}/chat/completions",
                        headers={"Authorization": f"Bearer {m['api_key']}"},
                        json=request_body,
                    )
                    if resp.status_code == 429:
                        print(f"[{self.name}] 429 on {m['model']}, switching...")
                        continue
                    if resp.status_code >= 500:
                        print(f"[{self.name}] {resp.status_code} on {m['model']}")
                        await asyncio.sleep(2)
                        continue
                    resp.raise_for_status()
                    data = resp.json()
                    return data["choices"][0]["message"]
            except httpx.ReadTimeout:
                last_error = Exception(f"Timeout on {m['model']}")
                continue
            except Exception as e:
                last_error = e
                continue

        raise Exception(f"All models failed: {last_error}")

    async def _llm_call_stream(self, messages: list[dict], ws: WebSocket) -> tuple[dict, str]:
        """Streaming LLM call with tool support and fallback. Returns (message_dict, streamed_text)."""
        request_body = {
            "model": self.llm_config["model"],
            "messages": messages,
            "temperature": 0.7,
            "max_tokens": 4096,
            "stream": True,
        }
        if self.tools_schema:
            request_body["tools"] = self.tools_schema
            request_body["tool_choice"] = "auto"

        models_to_try = [
            {
                "model": self.llm_config["model"],
                "base_url": self.llm_config["base_url"],
                "api_key": self.llm_config["api_key"],
            }
        ]
        fallback = self.llm_config.get("fallback_model")
        if fallback:
            models_to_try.append({
                "model": fallback,
                "base_url": self.llm_config.get("fallback_base_url", self.llm_config["base_url"]),
                "api_key": self.llm_config.get("fallback_api_key", self.llm_config["api_key"]),
            })

        last_error = None
        for m in models_to_try:
            request_body["model"] = m["model"]
            try:
                result = await self._do_stream(request_body, ws, m["base_url"], m["api_key"])
                return result
            except Exception as e:
                last_error = e
                if "429" in str(e) or "rate_limit" in str(e):
                    print(f"[{self.name}] 429 on {m['model']}, trying fallback...")
                    continue
                raise

        raise Exception(f"All models failed: {last_error}")

    async def _do_stream(self, request_body: dict, ws: WebSocket, base_url: str, api_key: str) -> tuple[dict, str]:
        """Execute a single streaming LLM request."""
        async with httpx.AsyncClient(timeout=300) as client:
            async with client.stream(
                "POST",
                f"{base_url}/chat/completions",
                headers={"Authorization": f"Bearer {api_key}"},
                json=request_body,
            ) as resp:
                if resp.status_code != 200:
                    body = await resp.aread()
                    raise Exception(f"LLM API {resp.status_code}: {body[:300].decode()}")

                content_parts = []
                tool_calls_acc: dict[int, dict] = {}

                async for line in resp.aiter_lines():
                    if not line.startswith("data: "):
                        continue
                    payload = line[6:]
                    if payload.strip() == "[DONE]":
                        break
                    try:
                        chunk = json.loads(payload)
                        delta = chunk["choices"][0].get("delta", {})

                        # Stream text content to client
                        content = delta.get("content", "")
                        if content:
                            content_parts.append(content)
                            await ws.send_text(json.dumps({"type": "chunk", "content": content}))

                        # Accumulate tool calls from stream
                        if "tool_calls" in delta:
                            for tc_delta in delta["tool_calls"]:
                                idx = tc_delta["index"]
                                if idx not in tool_calls_acc:
                                    tool_calls_acc[idx] = {
                                        "id": tc_delta.get("id", f"call_{idx}"),
                                        "type": "function",
                                        "function": {"name": "", "arguments": ""},
                                    }
                                if "id" in tc_delta and tc_delta["id"]:
                                    tool_calls_acc[idx]["id"] = tc_delta["id"]
                                func = tc_delta.get("function", {})
                                if "name" in func and func["name"]:
                                    tool_calls_acc[idx]["function"]["name"] = func["name"]
                                if "arguments" in func:
                                    tool_calls_acc[idx]["function"]["arguments"] += func["arguments"]

                    except (json.JSONDecodeError, KeyError, IndexError):
                        continue

        full_content = "".join(content_parts)
        assistant_msg: dict = {"role": "assistant", "content": full_content or None}
        if tool_calls_acc:
            assistant_msg["tool_calls"] = [tool_calls_acc[i] for i in sorted(tool_calls_acc.keys())]

        return assistant_msg, full_content

    # --- Tool Execution ---

    async def _execute_tool(self, name: str, args: dict, username: str = "") -> str:
        """Execute a tool and return result string."""
        try:
            if name == "shell":
                return self._exec_shell(args.get("command", ""))
            elif name == "filesystem":
                return self._exec_filesystem(args)
            elif name == "delegate_task":
                return await self._exec_delegate(args)
            elif name == "web_search":
                return await self._exec_web_search(args.get("query", ""), args.get("type", "text"))
            elif name == "run_tests":
                return self._exec_shell(args.get("command", ""), cwd=args.get("working_dir"))
            elif name == "code_execute":
                return self._exec_code(args)
            elif name == "read_documents":
                return self._exec_read_doc(args)
            elif name == "user_memory":
                return self._exec_user_memory(args, username)
            else:
                return f"Unknown tool: {name}"
        except Exception as e:
            return f"Tool error ({name}): {type(e).__name__}: {e}"

    # --- User Memory ---

    def _user_memory_path(self, username: str) -> Path:
        return Path(self.shared["public_dir"]).resolve() / "users" / username / "memory.json"

    def _load_user_memory(self, username: str) -> list[str]:
        """Load user memory facts from disk."""
        path = self._user_memory_path(username)
        if path.exists():
            try:
                return json.loads(path.read_text())
            except (json.JSONDecodeError, IOError):
                return []
        return []

    def _save_user_memory(self, username: str, facts: list[str]):
        """Save user memory facts to disk."""
        path = self._user_memory_path(username)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(facts, ensure_ascii=False, indent=2))

    def _exec_user_memory(self, args: dict, username: str) -> str:
        """Execute user_memory tool."""
        op = args.get("operation", "")
        if not username:
            return "ERROR: No user context"

        if op == "recall":
            facts = self._load_user_memory(username)
            if not facts:
                return f"Память пуста — нет сохранённых фактов о пользователе '{username}'."
            return f"Факты о пользователе '{username}':\n" + "\n".join(f"- {f}" for f in facts)

        elif op == "save":
            fact = args.get("fact", "").strip()
            if not fact:
                return "ERROR: No fact provided"
            facts = self._load_user_memory(username)
            # Avoid duplicates
            if fact not in facts:
                facts.append(fact)
                self._save_user_memory(username, facts)
            return f"Записано в память: '{fact}'. Всего фактов: {len(facts)}."

        return f"ERROR: Unknown operation: {op}"

    def _exec_shell(self, command: str, cwd: str = None) -> str:
        if not command:
            return "ERROR: No command provided"
        work_dir = self.sandbox_dir
        if cwd:
            work_dir = (self.sandbox_dir / cwd).resolve()
            if not str(work_dir).startswith(str(self.sandbox_dir)):
                work_dir = self.sandbox_dir
        try:
            result = subprocess.run(
                ["bash", "-c", command],
                cwd=str(work_dir),
                capture_output=True,
                text=True,
                timeout=60,
                env={
                    **os.environ,
                    "HOME": str(self.sandbox_dir),
                    "AGENT_NAME": self.name,
                    "SHARED_DIR": str(Path(self.shared["public_dir"]).resolve()),
                },
            )
            output = ""
            if result.stdout:
                output += result.stdout
            if result.stderr:
                output += f"\n[stderr]: {result.stderr}"
            if result.returncode != 0:
                output += f"\n[exit code: {result.returncode}]"
            return (output or "(no output)").strip()[:4096]
        except subprocess.TimeoutExpired:
            return "ERROR: Command timed out (60s)"

    def _exec_filesystem(self, args: dict) -> str:
        op = args.get("operation", "")
        path = args.get("path", "")
        content = args.get("content", "")

        sandbox = self.sandbox_dir
        shared = Path(self.shared["public_dir"]).resolve()

        if path.startswith("shared/") or path.startswith("../shared/"):
            target = (shared / path.removeprefix("shared/").removeprefix("../shared/")).resolve()
            allowed_roots = [shared]
        else:
            target = (sandbox / path).resolve()
            allowed_roots = [sandbox, shared]

        if not any(str(target).startswith(str(root)) for root in allowed_roots):
            return "ERROR: Access denied — path outside allowed directories"

        if op == "read":
            if not target.exists():
                return f"ERROR: File not found: {path}"
            return target.read_text()[:8192]
        elif op == "write":
            if not str(target).startswith(str(sandbox)) and not str(target).startswith(str(shared / "results")):
                return "ERROR: Can only write to sandbox or shared/results/"
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(content)
            return f"Written {len(content)} bytes to {path}"
        elif op == "append":
            if not str(target).startswith(str(sandbox)):
                return "ERROR: Can only append in sandbox"
            target.parent.mkdir(parents=True, exist_ok=True)
            with open(target, "a") as f:
                f.write(content)
            return f"Appended {len(content)} bytes to {path}"
        elif op == "ls":
            if not target.exists():
                return f"ERROR: Directory not found: {path}"
            entries = []
            for f in sorted(target.iterdir()):
                suffix = "/" if f.is_dir() else f" ({f.stat().st_size}b)"
                entries.append(f"{f.name}{suffix}")
            return "\n".join(entries) if entries else "(empty directory)"
        return f"ERROR: Unknown operation: {op}"

    async def _exec_delegate(self, args: dict) -> str:
        to_agent = args.get("to_agent", "")
        task = args.get("task", "")
        context = args.get("context", "")

        target_port = self._get_agent_port(to_agent)
        if not target_port:
            return f"ERROR: Agent '{to_agent}' not found"

        bind_host = os.environ.get("AGENT_BIND_HOST", "127.0.0.1")
        task_msg = {
            "from_agent": self.name,
            "to_agent": to_agent,
            "task": task,
            "context": {"details": context} if context else None,
        }
        try:
            async with httpx.AsyncClient() as client:
                resp = await client.post(
                    f"http://{bind_host}:{target_port}/task",
                    json=task_msg,
                    timeout=10,
                )
                return f"Task delegated to {to_agent}: {resp.json()}"
        except Exception as e:
            return f"ERROR delegating to {to_agent}: {e}"

    async def _exec_web_search(self, query: str, search_type: str = "text") -> str:
        if not query:
            return "ERROR: No query provided"

        if search_type == "images":
            return await self._search_images(query)

        try:
            async with httpx.AsyncClient(timeout=15) as client:
                resp = await client.get(
                    "https://html.duckduckgo.com/html/",
                    params={"q": query},
                    headers={"User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36"},
                )
                from html.parser import HTMLParser

                class SnippetParser(HTMLParser):
                    def __init__(self):
                        super().__init__()
                        self.in_snippet = False
                        self.snippets = []
                        self.current = ""

                    def handle_starttag(self, tag, attrs):
                        attrs_dict = dict(attrs)
                        cls = attrs_dict.get("class", "")
                        if "result__snippet" in cls:
                            self.in_snippet = True
                            self.current = ""

                    def handle_endtag(self, tag):
                        if self.in_snippet and tag in ("a", "span", "td", "div"):
                            self.in_snippet = False
                            if self.current.strip():
                                self.snippets.append(self.current.strip())

                    def handle_data(self, data):
                        if self.in_snippet:
                            self.current += data

                parser = SnippetParser()
                parser.feed(resp.text)
                if parser.snippets:
                    return f"Search results for '{query}':\n" + "\n---\n".join(parser.snippets[:5])
                return f"No results found for '{query}'"
        except Exception as e:
            return f"Search error: {e}"

    async def _search_images(self, query: str) -> str:
        """Search for images via DuckDuckGo and return markdown image links."""
        import re
        try:
            async with httpx.AsyncClient(timeout=15, follow_redirects=True) as client:
                # Get VQD token
                resp = await client.get(
                    "https://duckduckgo.com/",
                    params={"q": query},
                    headers={"User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36"},
                )
                vqd_match = re.search(r'vqd="([^"]+)"', resp.text) or re.search(r'vqd=([a-zA-Z0-9_-]+)', resp.text)
                if not vqd_match:
                    return f"ERROR: Could not get search token for '{query}'"

                vqd = vqd_match.group(1)

                # Search images
                resp2 = await client.get(
                    "https://duckduckgo.com/i.js",
                    params={"l": "us-en", "o": "json", "q": query, "vqd": vqd, "f": ",,,", "p": "1"},
                    headers={"User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36", "Referer": "https://duckduckgo.com/"},
                )
                if resp2.status_code != 200:
                    return f"Image search failed: HTTP {resp2.status_code}"

                data = resp2.json()
                results = data.get("results", [])
                if not results:
                    return f"No images found for '{query}'"

                # Return top 4 images in markdown format
                output_lines = [f"Found {len(results)} images for '{query}'. Top results:\n"]
                for i, img in enumerate(results[:4], 1):
                    url = img.get("image", "")
                    title = img.get("title", "Image")
                    source = img.get("source", "")
                    if url:
                        output_lines.append(f"{i}. ![{title}]({url})")
                        if source:
                            output_lines.append(f"   Source: {source}")
                return "\n".join(output_lines)
        except Exception as e:
            return f"Image search error: {e}"

    def _exec_code(self, args: dict) -> str:
        language = args.get("language", "python")
        code = args.get("code", "")
        if not code:
            return "ERROR: No code provided"
        cmd_map = {
            "python": ["python3", "-c", code],
            "node": ["node", "-e", code],
            "bash": ["bash", "-c", code],
        }
        cmd = cmd_map.get(language)
        if not cmd:
            return f"ERROR: Unsupported language: {language}"
        try:
            result = subprocess.run(
                cmd, cwd=str(self.sandbox_dir), capture_output=True, text=True, timeout=30,
            )
            output = result.stdout
            if result.stderr:
                output += f"\n[stderr]: {result.stderr}"
            return (output or "(no output)").strip()[:4096]
        except subprocess.TimeoutExpired:
            return "ERROR: Code execution timed out (30s)"

    def _exec_read_doc(self, args: dict) -> str:
        path = args.get("path", "")
        sandbox = self.sandbox_dir
        shared = Path(self.shared["public_dir"]).resolve()

        if path.startswith("shared/"):
            target = (shared / path.removeprefix("shared/")).resolve()
        else:
            target = (sandbox / path).resolve()

        if not target.exists():
            return f"ERROR: Document not found: {path}"
        return target.read_text()[:8192]

    def _get_agent_port(self, agent_name: str) -> Optional[int]:
        config_path = Path(__file__).parent / "config.yaml"
        if config_path.exists():
            cfg = yaml.safe_load(config_path.read_text())
            for agent in cfg.get("agents", []):
                if agent["name"] == agent_name:
                    return agent["port"]
        return None


def load_agent_config(agent_name: str) -> tuple[dict, dict]:
    config_path = Path(__file__).parent / "config.yaml"
    cfg = yaml.safe_load(config_path.read_text())
    shared = cfg["shared"]
    llm_defaults = cfg.get("llm_defaults", {})

    for agent in cfg["agents"]:
        if agent["name"] == agent_name:
            agent_llm = agent.get("llm", {})
            agent["llm"] = {**llm_defaults, **agent_llm}
            return agent, shared

    raise ValueError(f"Agent '{agent_name}' not found in config.yaml")


if __name__ == "__main__":
    import sys
    import uvicorn

    if len(sys.argv) < 2:
        print("Usage: python agent_server.py <agent_name>")
        sys.exit(1)

    agent_name = sys.argv[1]
    agent_cfg, shared_cfg = load_agent_config(agent_name)
    bind_host = os.environ.get("AGENT_BIND_HOST", "127.0.0.1")

    # Init auth module (shared users across all agents)
    auth.init(shared_cfg["public_dir"])

    print(f"Starting agent '{agent_name}' [AGENT MODE] on {bind_host}:{agent_cfg['port']}...")
    server = AgentServer(agent_cfg, shared_cfg)
    uvicorn.run(server.app, host=bind_host, port=agent_cfg["port"])
