"""
Auth module — JWT + bcrypt + JSON file storage.
Provides signup, login, token verification for HTTP and WebSocket.
"""

import json
import time
import secrets
from pathlib import Path
from typing import Optional

import bcrypt
import jwt

# --- Config ---

SECRET_KEY = None  # Will be loaded/generated at startup
ALGORITHM = "HS256"
TOKEN_EXPIRE_SECONDS = 86400 * 7  # 7 days

USERS_FILE: Path = None  # Set on init

# Static admin account (always available)
STATIC_ADMIN_USERNAME = "admin"
STATIC_ADMIN_PASSWORD = "avkeva321"


def init(shared_dir: str, secret: Optional[str] = None):
    """Initialize auth module. Call once at startup."""
    global SECRET_KEY, USERS_FILE

    shared = Path(shared_dir)
    shared.mkdir(parents=True, exist_ok=True)

    USERS_FILE = shared / "users.json"
    if not USERS_FILE.exists():
        USERS_FILE.write_text("[]")

    # Load or generate secret
    secret_file = shared / ".auth_secret"
    if secret:
        SECRET_KEY = secret
    elif secret_file.exists():
        SECRET_KEY = secret_file.read_text().strip()
    else:
        SECRET_KEY = secrets.token_hex(32)
        secret_file.write_text(SECRET_KEY)
        secret_file.chmod(0o600)


def _load_users() -> list[dict]:
    if not USERS_FILE or not USERS_FILE.exists():
        return []
    return json.loads(USERS_FILE.read_text())


def _save_users(users: list[dict]):
    USERS_FILE.write_text(json.dumps(users, ensure_ascii=False, indent=2))


def _hash_password(password: str) -> str:
    return bcrypt.hashpw(password.encode(), bcrypt.gensalt()).decode()


def _verify_password(password: str, hashed: str) -> bool:
    return bcrypt.checkpw(password.encode(), hashed.encode())


def signup(username: str, password: str) -> dict:
    """Register a new user. Returns user dict or raises ValueError."""
    username = username.strip().lower()
    if not username or len(username) < 2:
        raise ValueError("Имя пользователя должно быть не менее 2 символов")
    if not password or len(password) < 4:
        raise ValueError("Пароль должен быть не менее 4 символов")

    users = _load_users()
    if any(u["username"] == username for u in users):
        raise ValueError("Пользователь уже существует")

    # First user gets admin role automatically
    role = "admin" if not users else "user"

    user = {
        "username": username,
        "password_hash": _hash_password(password),
        "created_at": time.time(),
        "role": role,
    }
    users.append(user)
    _save_users(users)

    return {"username": username, "token": _create_token(username, role)}


def login(username: str, password: str) -> dict:
    """Authenticate user. Returns token or raises ValueError."""
    username = username.strip().lower()

    # Static admin — always works, no DB lookup
    if username == STATIC_ADMIN_USERNAME and password == STATIC_ADMIN_PASSWORD:
        return {"username": username, "token": _create_token(username, "admin")}

    users = _load_users()

    user = next((u for u in users if u["username"] == username), None)
    if not user or not _verify_password(password, user["password_hash"]):
        raise ValueError("Неверное имя пользователя или пароль")

    role = user.get("role", "user")
    return {"username": username, "token": _create_token(username, role)}


def _create_token(username: str, role: str = "user") -> str:
    payload = {
        "sub": username,
        "role": role,
        "iat": int(time.time()),
        "exp": int(time.time()) + TOKEN_EXPIRE_SECONDS,
    }
    return jwt.encode(payload, SECRET_KEY, algorithm=ALGORITHM)


def verify_token(token: str) -> Optional[str]:
    """Verify JWT token. Returns username or None."""
    try:
        payload = jwt.decode(token, SECRET_KEY, algorithms=[ALGORITHM])
        return payload.get("sub")
    except (jwt.ExpiredSignatureError, jwt.InvalidTokenError):
        return None


def is_admin(username: str) -> bool:
    """Check if a user has admin role."""
    if username == STATIC_ADMIN_USERNAME:
        return True
    users = _load_users()
    user = next((u for u in users if u["username"] == username), None)
    if user:
        return user.get("role") == "admin"
    return False


def set_admin(username: str, is_admin_flag: bool = True) -> bool:
    """Set or remove admin role for a user. Returns True if user found."""
    users = _load_users()
    for u in users:
        if u["username"] == username:
            u["role"] = "admin" if is_admin_flag else "user"
            _save_users(users)
            return True
    return False


def get_user(username: str) -> Optional[dict]:
    """Get user info (without password hash)."""
    users = _load_users()
    user = next((u for u in users if u["username"] == username), None)
    if user:
        return {"username": user["username"], "created_at": user["created_at"]}
    return None
