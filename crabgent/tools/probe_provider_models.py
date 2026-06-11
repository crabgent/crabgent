#!/usr/bin/env python3
"""Probe provider model-discovery endpoints from local .env credentials."""

from __future__ import annotations

import json
import os
import ssl
import sys
import urllib.parse
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

TIMEOUT_SECS = 30
SNIPPET_CHARS = 360


def load_dotenv(path: Path) -> None:
    if not path.exists():
        return
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key = key.strip()
        value = value.strip().strip("\"'")
        os.environ.setdefault(key, value)


def secret_values() -> list[str]:
    values: list[str] = []
    for key, value in os.environ.items():
        if key.endswith(("KEY", "TOKEN")) or "TOKEN" in key or "SECRET" in key:
            if value:
                values.append(value)
    return sorted(values, key=len, reverse=True)


def redact(text: str) -> str:
    redacted = text
    for value in secret_values():
        if len(value) >= 6:
            redacted = redacted.replace(value, "<redacted>")
    return redacted


def request_json(
    *,
    url: str,
    display_url: str,
    headers: dict[str, str],
) -> tuple[int | None, Any | None, str | None, str]:
    req = urllib.request.Request(url, headers=headers, method="GET")
    try:
        with urllib.request.urlopen(
            req,
            timeout=TIMEOUT_SECS,
            context=ssl.create_default_context(),
        ) as response:
            body = response.read()
            status = response.status
    except urllib.error.HTTPError as exc:
        body = exc.read()
        status = exc.code
    except Exception as exc:  # noqa: BLE001
        return None, None, None, f"{type(exc).__name__}: {exc}"

    content = body.decode("utf-8", errors="replace")
    try:
        return status, json.loads(content), None, ""
    except json.JSONDecodeError:
        snippet = redact(content[:SNIPPET_CHARS]).replace("\n", "\\n")
        return status, None, snippet, ""


def with_query(url: str, params: dict[str, str | int]) -> str:
    parsed = urllib.parse.urlsplit(url)
    query = dict(urllib.parse.parse_qsl(parsed.query, keep_blank_values=True))
    query.update({key: str(value) for key, value in params.items()})
    return urllib.parse.urlunsplit(
        parsed._replace(query=urllib.parse.urlencode(query))
    )


def fetch_pages(
    *,
    url: str,
    headers: dict[str, str],
    provider: str,
) -> tuple[int | None, Any | None, str | None, str]:
    if provider == "anthropic":
        return fetch_anthropic_pages(url=url, headers=headers)
    if provider == "google":
        return fetch_google_pages(url=url, headers=headers)
    return request_json(url=url, display_url=url, headers=headers)


def fetch_anthropic_pages(
    *,
    url: str,
    headers: dict[str, str],
) -> tuple[int | None, Any | None, str | None, str]:
    combined: dict[str, Any] = {"data": []}
    after_id: str | None = None
    last_status: int | None = None
    for _ in range(20):
        page_url = with_query(url, {"limit": 100, **({"after_id": after_id} if after_id else {})})
        status, payload, snippet, error = request_json(
            url=page_url,
            display_url=page_url,
            headers=headers,
        )
        last_status = status
        if error or payload is None or not isinstance(payload, dict):
            return status, payload, snippet, error
        data = payload.get("data")
        if isinstance(data, list):
            combined["data"].extend(data)
        combined["first_id"] = payload.get("first_id", combined.get("first_id"))
        combined["last_id"] = payload.get("last_id")
        combined["has_more"] = payload.get("has_more", False)
        if not payload.get("has_more"):
            return last_status, combined, None, ""
        next_after_id = payload.get("last_id")
        if not isinstance(next_after_id, str) or next_after_id == after_id:
            return last_status, combined, None, "anthropic pagination did not advance"
        after_id = next_after_id
    return last_status, combined, None, "anthropic pagination exceeded 20 pages"


def fetch_google_pages(
    *,
    url: str,
    headers: dict[str, str],
) -> tuple[int | None, Any | None, str | None, str]:
    combined: dict[str, Any] = {"models": []}
    page_token: str | None = None
    last_status: int | None = None
    for _ in range(20):
        page_url = with_query(
            url,
            {"pageSize": 1000, **({"pageToken": page_token} if page_token else {})},
        )
        status, payload, snippet, error = request_json(
            url=page_url,
            display_url=page_url,
            headers=headers,
        )
        last_status = status
        if error or payload is None or not isinstance(payload, dict):
            return status, payload, snippet, error
        models = payload.get("models")
        if isinstance(models, list):
            combined["models"].extend(models)
        next_token = payload.get("nextPageToken")
        if not isinstance(next_token, str) or not next_token:
            return last_status, combined, None, ""
        if next_token == page_token:
            return last_status, combined, None, "google pagination did not advance"
        page_token = next_token
    return last_status, combined, None, "google pagination exceeded 20 pages"


def ids_from_openai(payload: Any) -> list[str]:
    if not isinstance(payload, dict):
        return []
    data = payload.get("data")
    if not isinstance(data, list):
        return []
    return sorted(
        item["id"]
        for item in data
        if isinstance(item, dict) and isinstance(item.get("id"), str)
    )


def ids_from_anthropic(payload: Any) -> list[str]:
    return ids_from_openai(payload)


def ids_from_google(payload: Any) -> list[str]:
    if not isinstance(payload, dict):
        return []
    models = payload.get("models")
    if not isinstance(models, list):
        return []
    ids: list[str] = []
    for item in models:
        if not isinstance(item, dict):
            continue
        name = item.get("name")
        if isinstance(name, str):
            ids.append(name.removeprefix("models/"))
    return sorted(ids)


def ids_from_elevenlabs(payload: Any) -> list[str]:
    raw_models: Any
    if isinstance(payload, list):
        raw_models = payload
    elif isinstance(payload, dict):
        raw_models = payload.get("models", payload.get("data", []))
    else:
        raw_models = []
    if not isinstance(raw_models, list):
        return []
    ids: list[str] = []
    for item in raw_models:
        if not isinstance(item, dict):
            continue
        for key in ("model_id", "id", "name"):
            value = item.get(key)
            if isinstance(value, str):
                ids.append(value)
                break
    return sorted(ids)


def ids_from_chatgpt(payload: Any) -> list[str]:
    seen: set[str] = set()

    def walk(value: Any) -> None:
        if isinstance(value, dict):
            for key in ("id", "slug", "model", "model_slug"):
                item = value.get(key)
                if isinstance(item, str) and looks_like_model_id(item):
                    seen.add(item)
            for child in value.values():
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)

    walk(payload)
    return sorted(seen)


def looks_like_model_id(value: str) -> bool:
    prefixes = ("gpt-", "o1", "o3", "o4", "codex", "chatgpt-", "auto")
    return value.startswith(prefixes)


def shape(payload: Any) -> str:
    if isinstance(payload, dict):
        keys = ",".join(sorted(str(key) for key in payload.keys())[:12])
        return f"object keys=[{keys}]"
    if isinstance(payload, list):
        return f"array len={len(payload)}"
    return type(payload).__name__


def print_probe(
    name: str,
    *,
    url: str,
    display_url: str,
    headers: dict[str, str],
    extractor,
    provider: str = "generic",
) -> None:
    status, payload, snippet, error = fetch_pages(
        url=url,
        headers=headers,
        provider=provider,
    )
    print(f"\n## {name}")
    print(f"endpoint: {display_url}")
    if error:
        print(f"result: transport_error {redact(error)}")
        return
    print(f"status: {status}")
    if payload is None:
        print("json: no")
        if snippet:
            print(f"body_snippet: {snippet}")
        return
    ids = extractor(payload)
    print(f"json: yes, {shape(payload)}")
    if isinstance(payload, dict) and "error" in payload:
        print(f"error: {redact(json.dumps(payload['error'], sort_keys=True))[:SNIPPET_CHARS]}")
    print(f"model_count: {len(ids)}")
    for model_id in ids:
        print(f"- {model_id}")


def bearer_headers(token: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {token}"}


def codex_headers() -> dict[str, str] | None:
    token_path = os.environ.get("OPENAI_CODEX_OAUTH_TOKEN_PATH")
    if not token_path:
        return None
    try:
        token_payload = json.loads(Path(token_path).expanduser().read_text(encoding="utf-8"))
    except Exception:
        return None
    token = token_payload.get("access_token")
    if not isinstance(token, str) or not token:
        return None
    headers = {
        "Authorization": f"Bearer {token}",
        "OpenAI-Beta": "responses=experimental",
        "originator": "codex_cli_rs",
        "User-Agent": "codex_cli_rs/0.59.0",
    }
    account_id = token_payload.get("account_id")
    if isinstance(account_id, str) and account_id:
        headers["chatgpt-account-id"] = account_id
    return headers


def main() -> int:
    load_dotenv(Path(".env"))

    probes: list[tuple[str, str, str, dict[str, str], Any, str]] = []
    if key := os.environ.get("OPENAI_API_KEY"):
        probes.append(
            (
                "OpenAI API key",
                "https://api.openai.com/v1/models",
                "https://api.openai.com/v1/models",
                bearer_headers(key),
                ids_from_openai,
                "generic",
            )
        )
    if key := os.environ.get("OPENAI_TEST_STT_KEY"):
        probes.append(
            (
                "OpenAI STT test key",
                "https://api.openai.com/v1/models",
                "https://api.openai.com/v1/models",
                bearer_headers(key),
                ids_from_openai,
                "generic",
            )
        )
    if key := os.environ.get("ANTHROPIC_API_KEY"):
        probes.append(
            (
                "Anthropic",
                "https://api.anthropic.com/v1/models",
                "https://api.anthropic.com/v1/models",
                {
                    "x-api-key": key,
                    "anthropic-version": "2023-06-01",
                },
                ids_from_anthropic,
                "anthropic",
            )
        )
    if key := os.environ.get("GEMINI_API_KEY"):
        probes.append(
            (
                "Google Gemini",
                "https://generativelanguage.googleapis.com/v1beta/models",
                "https://generativelanguage.googleapis.com/v1beta/models",
                {"x-goog-api-key": key},
                ids_from_google,
                "google",
            )
        )
    if key := os.environ.get("ELEVENLABS_API_KEY"):
        probes.append(
            (
                "ElevenLabs",
                "https://api.elevenlabs.io/v1/models",
                "https://api.elevenlabs.io/v1/models",
                {"xi-api-key": key},
                ids_from_elevenlabs,
                "generic",
            )
        )
    if key := os.environ.get("ELEVENLABS_TEST_STT_KEY"):
        probes.append(
            (
                "ElevenLabs STT test key",
                "https://api.elevenlabs.io/v1/models",
                "https://api.elevenlabs.io/v1/models",
                {"xi-api-key": key},
                ids_from_elevenlabs,
                "generic",
            )
        )
    headers = codex_headers()
    if headers is not None:
        for path in (
            "/v1/models",
            "/backend-api/models",
            "/backend-api/codex/models",
            "/backend-api/codex/models?client_version=0.59.0",
        ):
            probes.append(
                (
                    f"OpenAI Codex OAuth {path}",
                    f"https://chatgpt.com{path}",
                    f"https://chatgpt.com{path}",
                    headers,
                    ids_from_chatgpt,
                    "generic",
                )
            )

    if not probes:
        print("No provider credentials found in .env", file=sys.stderr)
        return 1
    for name, url, display_url, headers, extractor, provider in probes:
        print_probe(
            name,
            url=url,
            display_url=display_url,
            headers=headers,
            extractor=extractor,
            provider=provider,
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
