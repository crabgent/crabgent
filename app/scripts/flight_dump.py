#!/usr/bin/env python3
"""Render a flight-recorder JSONL into editor-friendly markdown files.

Usage:
    scripts/flight_dump.py <run.jsonl> [<out_dir>]

Default out_dir: alongside the input, named <run_id>-readable/.

Layout:
    00-INDEX.md           top-level timeline with relative timestamps
    01-llm/<NNN>.md       one file per LLM call (request payload)
    02-tools.md           tool_call + tool_result pairs
    03-sink.md            outbound ChannelSink actions
    04-embeddings.md      embedding request/response events
    05-outcome.md         final Outcome
"""

import argparse
import datetime as dt
import json
import sys
from pathlib import Path


def parse_ts(s: str) -> dt.datetime:
    s = s.replace("Z", "+00:00")
    return dt.datetime.fromisoformat(s)


def rel_ms(t0: dt.datetime, t: dt.datetime) -> str:
    delta = (t - t0).total_seconds() * 1000.0
    if delta < 1000:
        return f"+{delta:>6.0f}ms"
    return f"+{delta / 1000:>5.2f}s "


def fmt_messages(messages: list[dict]) -> str:
    """One-line-per-message summary of the conversation tail.

    Crabgent's `LlmRequest.messages` uses a domain-shape (not provider
    wire-shape) where each message is one of:
      - user/system: {role, content: str | array[{type:text, text}]}
      - assistant:   {role: "assistant", text: str, tool_calls: [{id,name,args}]}
      - tool_result: {role: "tool_result", call_id, output, is_error}
    """
    out = []
    for i, msg in enumerate(messages):
        role = msg.get("role", "?")
        body = render_one_message(role, msg)
        body = body.replace("\n", " ")
        if len(body) > 240:
            body = body[:240] + "…"
        out.append(f"  [{i:>3}] {role:<12} {body}")
    return "\n".join(out)


def render_one_message(role: str, msg: dict) -> str:
    """Format one message body per its shape (user/system text vs assistant
    text+tool_calls vs tool_result)."""
    if role == "assistant":
        text = msg.get("text") or ""
        tool_calls = msg.get("tool_calls") or []
        parts = []
        if text:
            parts.append(text)
        for tc in tool_calls:
            name = tc.get("name", "?")
            args_str = json.dumps(tc.get("args", {}), ensure_ascii=False)
            if len(args_str) > 100:
                args_str = args_str[:100] + "…"
            parts.append(f"[→ {name}({args_str})]")
        return " ".join(parts) if parts else "(empty assistant)"
    if role == "tool_result":
        cid = (msg.get("call_id") or "?")[:14]
        out = msg.get("output") or ""
        if isinstance(out, dict | list):
            out = json.dumps(out, ensure_ascii=False)
        err = " FAIL" if msg.get("is_error") else ""
        return f"[← {cid}{err}] {out}"
    # user/system: content can be str or array
    content = msg.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for block in content:
            btype = block.get("type", "?")
            if btype == "text":
                parts.append(block.get("text", ""))
            else:
                parts.append(f"[{btype}]")
        return " | ".join(parts)
    return f"(unknown shape, keys={list(msg.keys())})"


def render_llm_request(idx: int, ev: dict, out_dir: Path, t0: dt.datetime) -> str:
    p = ev["payload"]
    model = p.get("model", "?")
    system_prompt = p.get("system_prompt") or ""
    messages = p.get("messages") or []
    tools = p.get("tools") or []
    ts = parse_ts(ev["ts"])

    file = out_dir / "01-llm" / f"{idx:03d}.md"
    file.parent.mkdir(parents=True, exist_ok=True)

    md = []
    md.append(f"# LLM request #{idx}")
    md.append("")
    md.append(f"- **ts:** `{ev['ts']}`  ({rel_ms(t0, ts).strip()} from start)")
    md.append(f"- **run_id:** `{ev['run_id']}`")
    md.append(f"- **model:** `{model}`")
    md.append(f"- **system_prompt:** {len(system_prompt)} chars")
    md.append(f"- **messages:** {len(messages)}")
    md.append(f"- **tools:** {len(tools)} ({', '.join(t.get('name', '?') for t in tools)})")
    md.append(f"- **max_tokens:** {p.get('max_tokens')}")
    md.append(f"- **temperature:** {p.get('temperature')}")
    md.append(f"- **reasoning_effort:** {p.get('reasoning_effort')}")
    md.append("")
    md.append("## system_prompt")
    md.append("")
    md.append("```")
    md.append(system_prompt)
    md.append("```")
    md.append("")
    md.append("## tools")
    md.append("")
    for t in tools:
        name = t.get("name", "?")
        desc = (t.get("description") or "").replace("\n", " ")
        if len(desc) > 200:
            desc = desc[:200] + "…"
        md.append(f"- **{name}** — {desc}")
    md.append("")
    md.append("## messages (tail summary)")
    md.append("")
    md.append("```")
    md.append(fmt_messages(messages))
    md.append("```")
    md.append("")
    md.append("## messages (raw, last 3)")
    md.append("")
    md.append("```json")
    md.append(json.dumps(messages[-3:], indent=2, ensure_ascii=False))
    md.append("```")
    file.write_text("\n".join(md), encoding="utf-8")

    return f"LLM #{idx}: model={model} msgs={len(messages)} sys={len(system_prompt)}B tools={len(tools)} → [{file.name}](01-llm/{file.name})"


def render_tool_event(ev: dict, t0: dt.datetime) -> str:
    ts = parse_ts(ev["ts"])
    p = ev["payload"]
    kind = ev["kind"]
    name = p.get("name", "?")
    if kind == "tool_call":
        args = json.dumps(p.get("args", {}), ensure_ascii=False)
        args_short = args if len(args) < 400 else args[:400] + "…"
        return (
            f"### {rel_ms(t0, ts)}  →  {name}\n"
            f"```\n{args_short}\n```\n"
        )
    elif kind == "tool_result":
        is_err = p.get("is_error", False)
        out = json.dumps(p.get("output"), ensure_ascii=False)
        out_short = out if len(out) < 600 else out[:600] + "…"
        prefix = "FAIL" if is_err else "OK"
        return (
            f"### {rel_ms(t0, ts)}  ←  {name}  [{prefix}]\n"
            f"```\n{out_short}\n```\n"
        )
    return ""


def render_sink_event(ev: dict, t0: dt.datetime) -> str:
    ts = parse_ts(ev["ts"])
    p = ev["payload"]
    kind = ev["kind"]
    if kind == "sink_send":
        body = p.get("body", "")
        thread = p.get("thread_parent")
        thread_str = f" thread={thread['id'][:14]}" if thread else " (top-level)"
        return f"### {rel_ms(t0, ts)}  send  → {p.get('conv')}{thread_str}\n```\n{body}\n```\n"
    elif kind == "sink_react":
        return f"### {rel_ms(t0, ts)}  react → {p.get('conv')} {p.get('emoji')} on {p.get('parent', {}).get('id', '?')[:14]}\n"
    elif kind == "sink_edit":
        return f"### {rel_ms(t0, ts)}  edit  → {p.get('conv')} target={p.get('target', {}).get('id', '?')[:14]}\n```\n{p.get('new_text')}\n```\n"
    elif kind == "sink_delete":
        return f"### {rel_ms(t0, ts)}  delete → {p.get('conv')} target={p.get('target', {}).get('id', '?')[:14]}\n"
    elif kind == "sink_upload":
        return f"### {rel_ms(t0, ts)}  upload → {p.get('conv')} {p.get('filename')} ({p.get('byte_len')} bytes)\n"
    elif kind == "sink_notify_user":
        return f"### {rel_ms(t0, ts)}  notify → {p.get('recipient')}\n```\n{p.get('body')}\n```\n"
    elif kind == "sink_send_error":
        return f"### {rel_ms(t0, ts)}  send_error: {p.get('error')}\n"
    return ""


def render_embed_event(ev: dict, t0: dt.datetime) -> str:
    ts = parse_ts(ev["ts"])
    p = ev["payload"]
    kind = ev["kind"]
    if kind == "embedding_request":
        texts = p.get("texts", [])
        preview = " | ".join(t.replace("\n", " ")[:80] for t in texts)
        return f"### {rel_ms(t0, ts)}  embed → model={p.get('model')} texts={len(texts)}\n```\n{preview}\n```\n"
    elif kind == "embedding_response":
        return (
            f"### {rel_ms(t0, ts)}  embed ← model={p.get('model')} dim={p.get('dim')} "
            f"count={p.get('vector_count')} norm={p.get('first_vector_l2_norm')}\n"
        )
    elif kind == "embedding_error":
        return f"### {rel_ms(t0, ts)}  embed FAIL: {p.get('error')}\n"
    return ""


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("jsonl", type=Path)
    ap.add_argument("out_dir", type=Path, nargs="?", default=None)
    args = ap.parse_args()

    if not args.jsonl.exists():
        print(f"input not found: {args.jsonl}", file=sys.stderr)
        return 1

    events: list[dict] = []
    with args.jsonl.open() as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError as e:
                print(f"skip bad line: {e}", file=sys.stderr)

    if not events:
        print("no events", file=sys.stderr)
        return 1

    run_id = events[0].get("run_id", args.jsonl.stem)
    out_dir = args.out_dir or args.jsonl.parent / f"{run_id}-readable"
    out_dir.mkdir(parents=True, exist_ok=True)
    t0 = parse_ts(events[0]["ts"])

    timeline: list[str] = []
    tools_md: list[str] = []
    sink_md: list[str] = []
    embed_md: list[str] = []
    outcome_md: list[str] = []
    llm_idx = 0

    for ev in events:
        kind = ev["kind"]
        ts = parse_ts(ev["ts"])
        rel = rel_ms(t0, ts)
        p = ev.get("payload", {})

        if kind == "inbound":
            tail = (p.get("tail_user") or "").replace("\n", " ")
            if len(tail) > 160:
                tail = tail[:160] + "…"
            timeline.append(f"{rel}  inbound       subject={p.get('subject_id', '?')[:24]}\n            user: {tail!r}")
        elif kind == "llm_request":
            llm_idx += 1
            line = render_llm_request(llm_idx, ev, out_dir, t0)
            timeline.append(f"{rel}  {line}")
        elif kind in ("tool_call", "tool_result"):
            tools_md.append(render_tool_event(ev, t0))
            if kind == "tool_call":
                name = p.get("name", "?")
                args_preview = json.dumps(p.get("args", {}), ensure_ascii=False)
                if len(args_preview) > 80:
                    args_preview = args_preview[:80] + "…"
                timeline.append(f"{rel}  tool→         {name}({args_preview})")
            else:
                name = p.get("name", "?")
                prefix = "FAIL" if p.get("is_error") else "ok"
                timeline.append(f"{rel}  tool←         {name} [{prefix}]")
        elif kind.startswith("sink_"):
            sink_md.append(render_sink_event(ev, t0))
            if kind == "sink_send":
                body = (p.get("body") or "").replace("\n", " ")
                if len(body) > 80:
                    body = body[:80] + "…"
                thread = " (top-level)" if not p.get("thread_parent") else " (threaded)"
                timeline.append(f"{rel}  sink_send     {body!r}{thread}")
            else:
                timeline.append(f"{rel}  {kind:<13} {p.get('conv', p.get('recipient', ''))}")
        elif kind.startswith("embedding_"):
            embed_md.append(render_embed_event(ev, t0))
            if kind == "embedding_request":
                timeline.append(f"{rel}  embed→        model={p.get('model')} texts={len(p.get('texts', []))}")
            elif kind == "embedding_response":
                timeline.append(f"{rel}  embed←        dim={p.get('dim')} norm={p.get('first_vector_l2_norm')}")
        elif kind == "outcome":
            text = (p.get("text") or "").replace("\n", " ")
            if len(text) > 200:
                text = text[:200] + "…"
            outcome_md.append(f"# Outcome\n\n- **kind:** `{p.get('kind')}`\n- **text:** `{text}`\n")
            timeline.append(f"{rel}  outcome       {p.get('kind')}  {text!r}")
        else:
            timeline.append(f"{rel}  ??? {kind}  {json.dumps(p)[:120]}")

    # Write index
    idx_lines = [
        f"# Flight recorder — `{run_id}`",
        "",
        f"- **events:** {len(events)}",
        f"- **start:** {events[0]['ts']}",
        f"- **end:**   {events[-1]['ts']}",
        f"- **duration:** {(parse_ts(events[-1]['ts']) - t0).total_seconds():.2f}s",
        "",
        "## sub-reports",
        "",
        f"- [LLM calls](01-llm/) ({llm_idx})",
        "- [Tools](02-tools.md)",
        "- [Channel sink](03-sink.md)",
        "- [Embeddings](04-embeddings.md)",
        "- [Outcome](05-outcome.md)",
        "",
        "## timeline",
        "",
        "```",
        *timeline,
        "```",
    ]
    (out_dir / "00-INDEX.md").write_text("\n".join(idx_lines), encoding="utf-8")
    (out_dir / "02-tools.md").write_text("# Tool calls\n\n" + "\n".join(tools_md), encoding="utf-8")
    (out_dir / "03-sink.md").write_text("# Channel sink\n\n" + "\n".join(sink_md), encoding="utf-8")
    (out_dir / "04-embeddings.md").write_text("# Embeddings\n\n" + "\n".join(embed_md), encoding="utf-8")
    (out_dir / "05-outcome.md").write_text("\n".join(outcome_md) or "(no outcome)\n", encoding="utf-8")

    print(f"wrote {out_dir}/  ({len(events)} events, {llm_idx} llm calls)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
