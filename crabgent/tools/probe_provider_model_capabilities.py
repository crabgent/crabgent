#!/usr/bin/env python3
"""Probe runtime model compatibility for crabgent provider surfaces.

The script intentionally prints no credential values. It uses local .env
credentials and writes machine-readable JSON plus a compact Markdown report.
"""

from __future__ import annotations

import concurrent.futures
import json
import os
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
RESULT_DIR = ROOT / ".probe-results"
TODAY = "2026-06-01"
TIMEOUT_SECS = 45
OPENAI_CLIENT_VERSION = "0.59.0"
CODEX_SCOPE_ID = "crabgent-model-probe"
CODEX_INSTALLATION_ID = "9d3e7a2c-5f4b-4a1d-9c8e-2b1a4f6d3e5c"
OPENAI_MAX_WORKERS = 4
GOOGLE_MAX_WORKERS = 4


@dataclass(frozen=True)
class ProbeResult:
    provider: str
    surface: str
    model: str
    status: int | None
    ok: bool
    error_type: str | None
    error_code: str | None
    error_message: str | None
    response_model: str | None = None
    detail: str | None = None


def load_dotenv(path: Path) -> None:
    if not path.exists():
        return
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        os.environ.setdefault(key.strip(), value.strip().strip("\"'"))


def request(
    method: str,
    url: str,
    *,
    headers: dict[str, str] | None = None,
    body: bytes | None = None,
    timeout: int = TIMEOUT_SECS,
) -> tuple[int | None, bytes, str | None]:
    req = urllib.request.Request(url, headers=headers or {}, data=body, method=method)
    try:
        with urllib.request.urlopen(
            req,
            timeout=timeout,
            context=ssl.create_default_context(),
        ) as response:
            return response.status, response.read(), None
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(), None
    except Exception as exc:  # noqa: BLE001
        return None, b"", f"{type(exc).__name__}: {exc}"


def get_json(url: str, headers: dict[str, str]) -> Any:
    status, body, error = request("GET", url, headers=headers)
    if error is not None:
        raise RuntimeError(error)
    if status != 200:
        raise RuntimeError(f"GET {url} returned {status}")
    return json.loads(body.decode("utf-8"))


def post_json(url: str, headers: dict[str, str], payload: Any) -> tuple[int | None, Any, str | None]:
    body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    merged = {"Content-Type": "application/json", **headers}
    status, raw, error = request("POST", url, headers=merged, body=body)
    if error is not None:
        return status, None, error
    try:
        return status, json.loads(raw.decode("utf-8")), None
    except json.JSONDecodeError:
        return status, raw[:128].decode("utf-8", errors="replace"), None


def openai_api_headers() -> dict[str, str]:
    return {"Authorization": f"Bearer {required_env('OPENAI_API_KEY')}"}


def anthropic_headers() -> dict[str, str]:
    return {
        "anthropic-version": "2023-06-01",
        "x-api-key": required_env("ANTHROPIC_API_KEY"),
    }


def elevenlabs_headers(key_env: str = "ELEVENLABS_API_KEY") -> dict[str, str]:
    return {"xi-api-key": required_env(key_env)}


def codex_headers() -> dict[str, str]:
    token_path = Path(required_env("OPENAI_CODEX_OAUTH_TOKEN_PATH")).expanduser()
    payload = json.loads(token_path.read_text(encoding="utf-8"))
    token = payload["access_token"]
    headers = {
        "Authorization": f"Bearer {token}",
        "OpenAI-Beta": "responses=experimental",
        "originator": "codex_cli_rs",
        "User-Agent": f"codex_cli_rs/{OPENAI_CLIENT_VERSION}",
        "session_id": CODEX_SCOPE_ID,
        "session-id": CODEX_SCOPE_ID,
        "thread_id": CODEX_SCOPE_ID,
        "thread-id": CODEX_SCOPE_ID,
        "x-client-request-id": CODEX_SCOPE_ID,
        "x-codex-window-id": CODEX_INSTALLATION_ID,
    }
    account_id = payload.get("account_id")
    if isinstance(account_id, str) and account_id:
        headers["chatgpt-account-id"] = account_id
    return headers


def required_env(key: str) -> str:
    value = os.environ.get(key)
    if not value:
        raise RuntimeError(f"{key} is not set")
    return value


def optional_model_ids_env(key: str) -> list[str]:
    raw = os.environ.get(key, "")
    return sorted({part.strip() for part in raw.split(",") if part.strip()})


def discover_openai_models() -> list[str]:
    payload = get_json("https://api.openai.com/v1/models", openai_api_headers())
    return sorted(
        item["id"]
        for item in payload.get("data", [])
        if isinstance(item, dict) and isinstance(item.get("id"), str)
    )


def discover_anthropic_models() -> list[str]:
    payload = get_json("https://api.anthropic.com/v1/models", anthropic_headers())
    return sorted(
        item["id"]
        for item in payload.get("data", [])
        if isinstance(item, dict) and isinstance(item.get("id"), str)
    )


def discover_codex_models() -> list[str]:
    url = (
        "https://chatgpt.com/backend-api/codex/models"
        f"?client_version={urllib.parse.quote(OPENAI_CLIENT_VERSION)}"
    )
    payload = get_json(url, codex_headers())
    models = payload.get("models", [])
    ids: list[str] = []
    if isinstance(models, list):
        for item in models:
            if isinstance(item, dict):
                for key in ("id", "slug", "model", "model_slug"):
                    value = item.get(key)
                    if isinstance(value, str):
                        ids.append(value)
                        break
            elif isinstance(item, str):
                ids.append(item)
    return sorted(set(ids))


def discover_google_models() -> list[dict[str, Any]]:
    headers = {"x-goog-api-key": required_env("GEMINI_API_KEY")}
    models: list[dict[str, Any]] = []
    page_token: str | None = None
    for _ in range(20):
        params = {"pageSize": "1000"}
        if page_token:
            params["pageToken"] = page_token
        url = (
            "https://generativelanguage.googleapis.com/v1beta/models?"
            + urllib.parse.urlencode(params)
        )
        payload = get_json(url, headers)
        for item in payload.get("models", []):
            if isinstance(item, dict):
                models.append(item)
        next_token = payload.get("nextPageToken")
        if not isinstance(next_token, str) or not next_token:
            break
        page_token = next_token
    return models


def discover_elevenlabs_models() -> list[str]:
    payload = get_json("https://api.elevenlabs.io/v1/models", elevenlabs_headers())
    ids: list[str] = []
    if isinstance(payload, list):
        for item in payload:
            if isinstance(item, dict):
                value = item.get("model_id", item.get("id"))
                if isinstance(value, str):
                    ids.append(value)
    return sorted(set(ids))


def discover_elevenlabs_voice() -> str:
    payload = get_json("https://api.elevenlabs.io/v1/voices", elevenlabs_headers())
    voices = payload.get("voices", []) if isinstance(payload, dict) else []
    for item in voices:
        if isinstance(item, dict) and isinstance(item.get("voice_id"), str):
            return item["voice_id"]
    raise RuntimeError("ElevenLabs returned no voices")


def openai_llm_candidate(model: str) -> bool:
    if model.startswith(("sora-", "text-embedding-", "omni-moderation", "tts-")):
        return False
    if any(part in model for part in ("image", "transcribe", "whisper", "realtime")):
        return False
    if model in {"babbage-002", "davinci-002"}:
        return False
    return model.startswith(("gpt-", "o1", "o3", "o4", "ft:gpt", "chat-latest", "computer-use"))


def openai_stt_candidate(model: str) -> bool:
    return any(part in model for part in ("transcribe", "whisper"))


def openai_tts_candidate(model: str) -> bool:
    return model.startswith("tts-") or "tts" in model or "audio" in model


def openai_image_candidate(model: str) -> bool:
    return "image" in model


def probe_anthropic_messages(model: str) -> ProbeResult:
    payload = {
        "model": model,
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "Reply with y."}],
    }
    status, response, error = post_json(
        "https://api.anthropic.com/v1/messages",
        anthropic_headers(),
        payload,
    )
    return provider_result("anthropic", "messages", model, status, response, error)


def probe_openai_chat(model: str) -> ProbeResult:
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": "Reply with y."}],
        "max_completion_tokens": 256,
        "stream": False,
    }
    status, response, error = post_json(
        "https://api.openai.com/v1/chat/completions",
        openai_api_headers(),
        payload,
    )
    return openai_result("openai", "chat_completions", model, status, response, error)


def probe_openai_responses(model: str) -> ProbeResult:
    payload = {
        "model": model,
        "input": "Reply with y.",
        "max_output_tokens": 256,
        "store": False,
    }
    status, response, error = post_json(
        "https://api.openai.com/v1/responses",
        openai_api_headers(),
        payload,
    )
    return openai_result("openai", "responses", model, status, response, error)


def probe_codex_response(model: str) -> ProbeResult:
    payload = {
        "model": model,
        "store": False,
        "stream": True,
        "instructions": "You are a terse probe.",
        "prompt_cache_key": CODEX_SCOPE_ID,
        "client_metadata": {"x-codex-installation-id": CODEX_INSTALLATION_ID},
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "Reply with y."}]}],
    }
    status, response, error = post_json(
        "https://chatgpt.com/backend-api/codex/responses",
        codex_headers(),
        payload,
    )
    return openai_result("openai", "codex_oauth_responses", model, status, response, error)


def tiny_wav() -> bytes:
    sample_rate = 8_000
    duration_samples = 800
    data = b"\x00\x00" * duration_samples
    byte_rate = sample_rate * 2
    block_align = 2
    bits_per_sample = 16
    riff_size = 36 + len(data)
    return (
        b"RIFF"
        + riff_size.to_bytes(4, "little")
        + b"WAVEfmt "
        + (16).to_bytes(4, "little")
        + (1).to_bytes(2, "little")
        + (1).to_bytes(2, "little")
        + sample_rate.to_bytes(4, "little")
        + byte_rate.to_bytes(4, "little")
        + block_align.to_bytes(2, "little")
        + bits_per_sample.to_bytes(2, "little")
        + b"data"
        + len(data).to_bytes(4, "little")
        + data
    )


def multipart_body(model: str) -> tuple[bytes, str]:
    boundary = f"crabgentProbe{int(time.time() * 1000)}"
    audio = tiny_wav()
    parts = [
        (
            f"--{boundary}\r\n"
            'Content-Disposition: form-data; name="model"\r\n\r\n'
            f"{model}\r\n"
        ).encode(),
        (
            f"--{boundary}\r\n"
            'Content-Disposition: form-data; name="response_format"\r\n\r\n'
            "json\r\n"
        ).encode(),
        (
            f"--{boundary}\r\n"
            'Content-Disposition: form-data; name="file"; filename="probe.wav"\r\n'
            "Content-Type: audio/wav\r\n\r\n"
        ).encode()
        + audio
        + b"\r\n",
        f"--{boundary}--\r\n".encode(),
    ]
    return b"".join(parts), boundary


def elevenlabs_stt_multipart_body(model: str) -> tuple[bytes, str]:
    boundary = f"crabgentProbe{int(time.time() * 1000)}"
    audio = tiny_wav()
    parts = [
        (
            f"--{boundary}\r\n"
            'Content-Disposition: form-data; name="model_id"\r\n\r\n'
            f"{model}\r\n"
        ).encode(),
        (
            f"--{boundary}\r\n"
            'Content-Disposition: form-data; name="timestamps_granularity"\r\n\r\n'
            "word\r\n"
        ).encode(),
        (
            f"--{boundary}\r\n"
            'Content-Disposition: form-data; name="tag_audio_events"\r\n\r\n'
            "true\r\n"
        ).encode(),
        (
            f"--{boundary}\r\n"
            'Content-Disposition: form-data; name="file"; filename="probe.wav"\r\n'
            "Content-Type: audio/wav\r\n\r\n"
        ).encode()
        + audio
        + b"\r\n",
        f"--{boundary}--\r\n".encode(),
    ]
    return b"".join(parts), boundary


def probe_openai_stt(model: str) -> ProbeResult:
    body, boundary = multipart_body(model)
    headers = {
        **openai_api_headers(),
        "Content-Type": f"multipart/form-data; boundary={boundary}",
    }
    status, raw, error = request(
        "POST",
        "https://api.openai.com/v1/audio/transcriptions",
        headers=headers,
        body=body,
    )
    response = parse_possible_json(raw)
    return openai_result("openai", "stt_batch", model, status, response, error)


def probe_elevenlabs_stt(model: str) -> ProbeResult:
    body, boundary = elevenlabs_stt_multipart_body(model)
    headers = {
        **elevenlabs_headers("ELEVENLABS_TEST_STT_KEY"),
        "Content-Type": f"multipart/form-data; boundary={boundary}",
    }
    status, raw, error = request(
        "POST",
        "https://api.elevenlabs.io/v1/speech-to-text",
        headers=headers,
        body=body,
    )
    response = parse_possible_json(raw)
    return provider_result("elevenlabs", "stt_batch", model, status, response, error)


def probe_openai_tts(model: str) -> ProbeResult:
    payload = {
        "model": model,
        "input": "hi",
        "voice": "coral",
        "response_format": "mp3",
    }
    status, raw, error = request(
        "POST",
        "https://api.openai.com/v1/audio/speech",
        headers={**openai_api_headers(), "Content-Type": "application/json"},
        body=json.dumps(payload).encode("utf-8"),
    )
    if error is not None:
        return ProbeResult("openai", "tts", model, status, False, "transport", None, error)
    if status == 200 and raw:
        return ProbeResult("openai", "tts", model, status, True, None, None, None)
    response = parse_possible_json(raw)
    return openai_result("openai", "tts", model, status, response, None)


def probe_elevenlabs_tts(model: str, voice_id: str) -> ProbeResult:
    payload = {
        "text": "hi",
        "model_id": model,
    }
    url = (
        "https://api.elevenlabs.io/v1/text-to-speech/"
        f"{urllib.parse.quote(voice_id, safe='')}"
        "?output_format=mp3_44100_128"
    )
    status, raw, error = request(
        "POST",
        url,
        headers={**elevenlabs_headers(), "Content-Type": "application/json"},
        body=json.dumps(payload).encode("utf-8"),
    )
    if error is not None:
        return ProbeResult("elevenlabs", "tts", model, status, False, "transport", None, error)
    if status == 200 and raw:
        return ProbeResult("elevenlabs", "tts", model, status, True, None, None, None)
    response = parse_possible_json(raw)
    return provider_result("elevenlabs", "tts", model, status, response, None)


def probe_openai_image(model: str) -> ProbeResult:
    payload = {"model": model, "prompt": "a single black dot", "n": 1}
    status, response, error = post_json(
        "https://api.openai.com/v1/images/generations",
        openai_api_headers(),
        payload,
    )
    return openai_result("openai", "image_generation", model, status, response, error)


def provider_result(
    provider: str,
    surface: str,
    model: str,
    status: int | None,
    response: Any,
    transport_error: str | None,
) -> ProbeResult:
    if transport_error is not None:
        return ProbeResult(provider, surface, model, status, False, "transport", None, transport_error)
    if status is not None and 200 <= status < 300:
        response_model = response.get("model") if isinstance(response, dict) else None
        return ProbeResult(provider, surface, model, status, True, None, None, None, response_model)
    err = extract_error(response)
    return ProbeResult(provider, surface, model, status, False, err[0], err[1], err[2])


def openai_result(
    provider: str,
    surface: str,
    model: str,
    status: int | None,
    response: Any,
    transport_error: str | None,
) -> ProbeResult:
    return provider_result(provider, surface, model, status, response, transport_error)


def parse_possible_json(raw: bytes) -> Any:
    try:
        return json.loads(raw.decode("utf-8"))
    except Exception:  # noqa: BLE001
        return None


def extract_error(response: Any) -> tuple[str | None, str | None, str | None]:
    if isinstance(response, dict):
        error = response.get("error", response.get("detail"))
        if isinstance(error, dict):
            return (
                as_str(error.get("type")),
                as_str(error.get("code")),
                compact(as_str(error.get("message"))),
            )
        if isinstance(error, str):
            return None, None, compact(error)
        message = response.get("message")
        if isinstance(message, str):
            return None, None, compact(message)
    if isinstance(response, str):
        return None, None, compact(response)
    return None, None, None


def as_str(value: Any) -> str | None:
    return value if isinstance(value, str) else None


def compact(value: str | None) -> str | None:
    if value is None:
        return None
    return " ".join(value.split())[:240]


def google_model_id(item: dict[str, Any]) -> str:
    return str(item.get("name", "")).removeprefix("models/")


def google_methods(item: dict[str, Any]) -> list[str]:
    raw = item.get("supportedGenerationMethods", [])
    return [method for method in raw if isinstance(method, str)]


def probe_google_text(model: str) -> ProbeResult:
    payload = {
        "contents": [{"role": "user", "parts": [{"text": "Reply with y."}]}],
        "generationConfig": {"maxOutputTokens": 32},
    }
    url = (
        "https://generativelanguage.googleapis.com/v1beta/models/"
        f"{urllib.parse.quote(model)}:generateContent"
    )
    status, response, error = post_json(
        url,
        {"x-goog-api-key": required_env("GEMINI_API_KEY")},
        payload,
    )
    return google_result("text_generate_content", model, status, response, error)


def probe_google_image(model: str) -> ProbeResult:
    payload = {
        "contents": [{"role": "user", "parts": [{"text": "a single black dot"}]}],
        "generationConfig": {
            "responseModalities": ["TEXT", "IMAGE"],
            "candidateCount": 1,
        },
    }
    url = (
        "https://generativelanguage.googleapis.com/v1beta/models/"
        f"{urllib.parse.quote(model)}:generateContent"
    )
    status, response, error = post_json(
        url,
        {"x-goog-api-key": required_env("GEMINI_API_KEY")},
        payload,
    )
    if status == 200 and isinstance(response, dict) and google_response_has_image(response):
        return ProbeResult("google", "image_generation", model, status, True, None, None, None)
    result = google_result("image_generation", model, status, response, error)
    if result.ok:
        return ProbeResult(
            "google",
            "image_generation",
            model,
            status,
            False,
            "no_image_part",
            None,
            "generateContent succeeded but returned no inline image data",
        )
    return result


def google_response_has_image(response: dict[str, Any]) -> bool:
    for candidate in response.get("candidates", []):
        if not isinstance(candidate, dict):
            continue
        content = candidate.get("content", {})
        if not isinstance(content, dict):
            continue
        for part in content.get("parts", []):
            if isinstance(part, dict) and isinstance(part.get("inlineData"), dict):
                return True
    return False


def google_result(
    surface: str,
    model: str,
    status: int | None,
    response: Any,
    transport_error: str | None,
) -> ProbeResult:
    if transport_error is not None:
        return ProbeResult("google", surface, model, status, False, "transport", None, transport_error)
    if status is not None and 200 <= status < 300:
        response_model = response.get("modelVersion") if isinstance(response, dict) else None
        return ProbeResult("google", surface, model, status, True, None, None, None, response_model)
    err = extract_error(response)
    return ProbeResult("google", surface, model, status, False, err[0], err[1], err[2])


def run_pool(
    title: str,
    models: list[str],
    fn,
    *,
    workers: int,
) -> list[ProbeResult]:
    print(f"{title}: {len(models)} candidates", file=sys.stderr)
    results: list[ProbeResult] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        futures = {pool.submit(fn, model): model for model in models}
        for future in concurrent.futures.as_completed(futures):
            model = futures[future]
            try:
                result = future.result()
                if should_retry(result):
                    time.sleep(1)
                    result = fn(model)
            except Exception as exc:  # noqa: BLE001
                result = ProbeResult(
                    provider=title.split()[0].lower(),
                    surface=title,
                    model=model,
                    status=None,
                    ok=False,
                    error_type="probe_exception",
                    error_code=None,
                    error_message=f"{type(exc).__name__}: {exc}",
                )
            print(f"  {model}: {'ok' if result.ok else 'fail'}", file=sys.stderr)
            results.append(result)
    return sorted(results, key=lambda result: (result.surface, result.model))


def should_retry(result: ProbeResult) -> bool:
    if result.ok:
        return False
    if result.status in {408, 429, 500, 502, 503, 504}:
        return True
    if result.error_type == "transport":
        return True
    message = result.error_message or ""
    return "max_tokens or model output limit" in message


def image_candidate_google(model: str) -> bool:
    return "image" in model or model.startswith("nano-banana")


def write_results(
    anthropic_models: list[str],
    openai_models: list[str],
    google_models: list[dict[str, Any]],
    elevenlabs_models: list[str],
    results: list[ProbeResult],
) -> tuple[Path, Path]:
    RESULT_DIR.mkdir(exist_ok=True)
    payload = {
        "date": TODAY,
        "anthropic_discovered_count": len(anthropic_models),
        "openai_discovered_count": len(openai_models),
        "google_discovered_count": len(google_models),
        "elevenlabs_discovered_count": len(elevenlabs_models),
        "results": [result.__dict__ for result in results],
        "anthropic_discovery": anthropic_models,
        "elevenlabs_discovery": elevenlabs_models,
        "google_discovery": [
            {
                "id": google_model_id(item),
                "supported_generation_methods": google_methods(item),
            }
            for item in google_models
        ],
    }
    json_path = RESULT_DIR / f"provider-model-capability-probe-{TODAY}.json"
    json_path.write_text(json.dumps(payload, indent=2, sort_keys=True), encoding="utf-8")
    md_path = RESULT_DIR / f"provider-model-capability-probe-{TODAY}.md"
    md_path.write_text(
        render_markdown(anthropic_models, openai_models, google_models, elevenlabs_models, results),
        encoding="utf-8",
    )
    return json_path, md_path


def render_markdown(
    anthropic_models: list[str],
    openai_models: list[str],
    google_models: list[dict[str, Any]],
    elevenlabs_models: list[str],
    results: list[ProbeResult],
) -> str:
    lines = [
        f"# Provider Model Capability Probe ({TODAY})",
        "",
        "Runtime probe using local `.env` credentials. Credentials and response bodies are not stored.",
        "",
        f"- Anthropic discovery returned {len(anthropic_models)} model ids from `/v1/models`.",
        f"- OpenAI discovery returned {len(openai_models)} model ids from `/v1/models`.",
        f"- Google discovery returned {len(google_models)} model resources from `/v1beta/models`.",
        f"- ElevenLabs discovery returned {len(elevenlabs_models)} model ids from `/v1/models`.",
        "",
    ]
    for provider in ("anthropic", "openai", "google", "elevenlabs"):
        lines.extend(provider_section(provider, results))
    lines.extend(["## Google Discovery Methods", ""])
    for item in sorted(google_models, key=google_model_id):
        methods = ", ".join(google_methods(item)) or "none"
        lines.append(f"- `{google_model_id(item)}`: {methods}")
    lines.append("")
    return "\n".join(lines)


def provider_section(provider: str, results: list[ProbeResult]) -> list[str]:
    lines = [f"## {provider.capitalize()}", ""]
    surfaces = sorted({result.surface for result in results if result.provider == provider})
    for surface in surfaces:
        matching = [r for r in results if r.provider == provider and r.surface == surface]
        ok = [r for r in matching if r.ok]
        fail = [r for r in matching if not r.ok]
        lines.append(f"### `{surface}`")
        lines.append("")
        lines.append(f"- working: {len(ok)}")
        lines.append(f"- failing: {len(fail)}")
        lines.append("")
        if ok:
            lines.append("Working models:")
            for result in sorted(ok, key=lambda r: r.model):
                suffix = f" -> `{result.response_model}`" if result.response_model else ""
                lines.append(f"- `{result.model}`{suffix}")
            lines.append("")
        if fail:
            lines.append("Rejected models:")
            for result in sorted(fail, key=lambda r: r.model):
                reason = result.error_message or result.error_code or result.error_type or f"status={result.status}"
                lines.append(f"- `{result.model}`: status={result.status}, {reason}")
            lines.append("")
    return lines


def main() -> int:
    load_dotenv(ROOT / ".env")
    anthropic_models = discover_anthropic_models()
    openai_models = discover_openai_models()
    google_models = discover_google_models()
    elevenlabs_models = discover_elevenlabs_models()
    elevenlabs_voice = discover_elevenlabs_voice()
    codex_discovered = discover_codex_models()

    openai_llm = [model for model in openai_models if openai_llm_candidate(model)]
    openai_stt = [model for model in openai_models if openai_stt_candidate(model)]
    openai_tts = [model for model in openai_models if openai_tts_candidate(model)]
    openai_image = [model for model in openai_models if openai_image_candidate(model)]
    google_text = [
        google_model_id(item)
        for item in google_models
        if "generateContent" in google_methods(item)
    ]
    google_image = [
        google_model_id(item)
        for item in google_models
        if "generateContent" in google_methods(item) and image_candidate_google(google_model_id(item))
    ]
    elevenlabs_stt = ["scribe_v2", "scribe_v1", "scribe_v1_experimental", "scribe_v2_realtime"]
    codex_models = sorted(
        set(codex_discovered)
        .union(openai_llm)
        .union(optional_model_ids_env("OPENAI_CODEX_EXTRA_MODELS"))
    )

    results: list[ProbeResult] = []
    results.extend(
        run_pool("Anthropic messages", anthropic_models, probe_anthropic_messages, workers=2)
    )
    results.extend(
        run_pool("OpenAI chat_completions", openai_llm, probe_openai_chat, workers=OPENAI_MAX_WORKERS)
    )
    results.extend(
        run_pool("OpenAI responses", openai_llm, probe_openai_responses, workers=OPENAI_MAX_WORKERS)
    )
    results.extend(
        run_pool("OpenAI stt_batch", openai_stt, probe_openai_stt, workers=2)
    )
    results.extend(
        run_pool("OpenAI tts", openai_tts, probe_openai_tts, workers=2)
    )
    results.extend(
        run_pool("OpenAI image_generation", openai_image, probe_openai_image, workers=1)
    )
    results.extend(
        run_pool("OpenAI codex_oauth_responses", codex_models, probe_codex_response, workers=1)
    )
    results.extend(
        run_pool("Google text_generate_content", google_text, probe_google_text, workers=GOOGLE_MAX_WORKERS)
    )
    results.extend(
        run_pool("Google image_generation", google_image, probe_google_image, workers=1)
    )
    results.extend(
        run_pool("ElevenLabs stt_batch", elevenlabs_stt, probe_elevenlabs_stt, workers=1)
    )
    results.extend(
        run_pool(
            "ElevenLabs tts",
            elevenlabs_models,
            lambda model: probe_elevenlabs_tts(model, elevenlabs_voice),
            workers=2,
        )
    )

    json_path, md_path = write_results(
        anthropic_models,
        openai_models,
        google_models,
        elevenlabs_models,
        results,
    )
    print(f"Wrote {json_path}")
    print(f"Wrote {md_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
