"""Explicit protocol-2 HTTP requests and maintenance for disposable test services.

No path translation or automatic request replay: stale fixtures must be updated
at their call sites. One call represents one intended write; transport retries
must retain the returned request or pass the original idempotency key.
"""
import json
import subprocess
import urllib.parse
import urllib.request
import uuid


def management_request(url, data=None, headers=None, method=None, *, idempotency_key=None):
    headers = dict(headers or {})
    method = method or ("POST" if data is not None else "GET")
    path = urllib.parse.urlsplit(url).path
    if path.startswith("/api/v2/") and method in {"POST", "PUT", "PATCH", "DELETE"}:
        if not any(key.lower() == "idempotency-key" for key in headers):
            if idempotency_key is None and data is not None:
                try:
                    payload = json.loads(data)
                    if isinstance(payload, dict):
                        idempotency_key = payload.get("idempotency_key")
                except (ValueError, TypeError):
                    pass
            headers["Idempotency-Key"] = idempotency_key or str(uuid.uuid4())
    return urllib.request.Request(url, data=data, headers=headers, method=method)


def start_test_server(command, *, env, stdout, stderr=None, **kwargs):
    """Initialize only the caller's explicitly configured disposable database."""
    if not env.get("DATABASE_URL"):
        raise ValueError("test service requires its own DATABASE_URL")
    subprocess.run([*command, "--migrate-only"], env=env, stdout=stdout,
                   stderr=stderr, cwd=kwargs.get("cwd"), check=True)
    return subprocess.Popen(command, env=env, stdout=stdout, stderr=stderr, **kwargs)
