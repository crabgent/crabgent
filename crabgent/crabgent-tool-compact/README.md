# crabgent-tool-compact

Recoverable tool-output compaction (token-killer) for the crabgent kernel.

Fat tool results (long shell stdout, multi-megabyte file reads, verbose MCP
payloads) burn the LLM context budget without adding signal. This crate cuts
the tokens the model ingests while keeping the full artifact recoverable by
construction: compaction is a projection over a preserved original, not a
destructive edit.

## Pieces

- `ToolCompactHook`: an `after_tool` hook. For an oversized output it runs a
  deterministic safety floor and a name-keyed semantic filter, stashes the full
  original in a `ToolCacheStore` keyed by a content-addressed `RecallHandle`,
  and replaces the result with `compacted + coverage-footer`.
- `RecallTool` (tool name `recall`, ops `recall_raw` and `expand`): reads the
  stash back by handle, capped per call and paginated.

Install both with `ToolCompactBuilder` or the `with_tool_compact` kernel-builder
extension. They share one store, session resolver, and auto-disable tracker.

```rust,ignore
use crabgent_tool_compact::KernelBuilderExt;

let kernel = Kernel::builder()
    .provider(provider)
    .policy(policy)
    .with_tool_compact(store) // store: Arc<impl ToolCacheStore>
    .build();
```

## Invariants

- Default OFF: registered explicitly by a deployment. When not registered, or
  on any internal failure, the raw output passes through unchanged. The
  per-tool byte caps (bash 200 KB, `read_file` 30 MB, MCP 5 MB) remain the floor.
- Deterministic: no LLM call anywhere, no regex. Bounded input implies bounded
  time; pathological input degrades to raw passthrough.
- Secret-safe: a suspected secret leak short-circuits to raw passthrough, so the
  compactor never surfaces or echoes a credential the raw output would not
  already have shown.
- Dual-signal success gate: a structured exit/`is_error` signal and a body
  classifier must agree before a command output is compacted as a boring
  success; on conflict the output passes through with a
  `<compaction-uncertain>` marker.
- Auto-disable: after N recalls of a tool within a run (default 3) compaction
  self-disables for that tool for the rest of the run.

## Coexistence with crabgent-tool-cache

`ToolCompactHook` and `crabgent-tool-cache`'s `ToolCacheHook` both rewrite output
on `after_tool`. Register one OR the other, not both: two `Decision::Replace`
hooks on the same result fight. If both are present, configure `ToolCacheHook`
to skip the `recall` tool name.

## Limitation

The auto-disable tracker is in-memory and per-process: a restart resets the
per-(run, tool) recall counters.

## Trust level

This is a `Hook`, which is a trust boundary: a hook can read and rewrite
conversation state. This crate only rewrites tool-result text and stashes the
original; it makes no network calls and reads no secrets. See the workspace
the repository security model.

## Attribution

The semantic filters are a clean-room reimplementation inspired by the design of
the rtk tool (Apache-2.0). No rtk source is included. See the `NOTICE` file.
