# LoC Reduction Analysis

Deep analysis of where the crabgent workspace can shed Rust lines without losing
features, with the safe reductions already shipped and the honest ceiling for
the rest. Findings are evidence-backed (cloc, a shingle duplication scan, and
per-crate recon); numbers are reproducible with the commands cited.

Shipped result so far: commit `2871316` (squash of 7 gated commits). Total Rust
138684 -> 138146. The src-only metric moved 95503 -> 96030 (see why below).

## 1. The metric

The "~95745" figure is **all Rust except top-level `<crate>/tests/` integration
dirs**. Reproduce:

```sh
cloc --include-lang=Rust --fullpath --not-match-d='crabgent[^/]*/tests/' \
     --exclude-dir=target .          # -> 95503 (the 242 gap to 95745 is noise)
cloc --include-lang=Rust --exclude-dir=target .   # -> 138684 total (incl tests/)
```

So inline `#[cfg(test)]` modules and `src`-nested `tests/` files **count**;
top-level `<crate>/tests/` integration dirs (~43k lines) **do not**. This single
fact decides which reductions move the number:

- prod `src` and inline unit tests -> on metric
- top-level `tests/` integration code -> off metric (lowers total Rust only)

## 2. Where the lines actually are

Total Rust 138949: **src 93155 + tests/ 45794**. About **47% of all Rust is
test code** (45.8k in `tests/` + ~26k inline `#[cfg(test)]`). The workspace is
test-heavy, not prod-heavy.

Largest crates (cloc, src + tests):

| Crate | total | src | tests/ |
| --- | --- | --- | --- |
| crabgent-core | 22094 | 14761 | 7333 |
| crabgent-channel | 14227 | 10157 | 4070 |
| crabgent-channel-slack | 10686 | 5897 | 4789 |
| crabgent-provider-openai | 8412 | 4497 | 3915 |
| crabgent-store-sqlite | 6246 | 4781 | 1465 |
| crabgent-store-postgres | 5571 | 3093 | 2478 |
| crabgent-channel-matrix | 5428 | 3792 | 1636 |
| crabgent-provider-anthropic | 5094 | 3936 | 1158 |
| crabgent-store | 4658 | 4418 | 240 |

## 3. Deep findings

**Duplication is broad, not deep.** An 8-line shingle scan over prod `src` finds
only ~800-1200 lines of genuine cross-crate literal copy-paste. The big crate
families (store sqlite vs postgres, the four providers, the four channels) are
**structurally parallel but not literally identical**: store backends diverge
50-79% on real SQL (`?` vs `$1`, FTS5 vs tsvector, `BEGIN IMMEDIATE` vs
`FOR UPDATE SKIP LOCKED`, manual `try_get` vs `FromRow`). There is no large pile
of mechanical dedup waiting.

**The fattest duplication is off-metric.** ~11-12k lines of duplicated test
fixtures (Provider/LLM stubs, `done()`/`capabilities()` builders) live in
top-level `tests/` dirs, which the tracked metric excludes. Consolidating them
lowers total Rust but not the 95745 number.

**Most Provider test stubs are genuinely bespoke.** Of ~110 `impl Provider for`
blocks in test code, the majority inspect the request and echo `req.model`/
effort, hold a post-move shared `Arc<Mutex<Vec<LlmRequest>>>` capture, use
custom `stream()` semantics, count calls, or implement a different trait. A
shared `StubProvider` only replaces the canned/scripted/failure/capability
cases; the rest cannot be folded without weakening tests. Raw "duplicated
scaffolding" line counts overstate the real dedup ceiling by several times.

**Conclusion: the codebase is lean for its feature surface.** The behaviour-
preserving prod-reduction ceiling is a few hundred lines, not thousands.

## 4. What was reduced (shipped, `2871316`)

Production dedup, behaviour-preserving, **-277 on the metric**:

- `string_newtype!` macro folds 9 hand-rolled string-newtypes in crabgent-core.
- One shared `MediaDownloadError` across the channel download adapters.
- Tool `MemoryScope` schema routed through `memory_scope_schema()`; four
  lifecycle tools folded into a `lifecycle_tool!` macro.
- `anthropic_model()` template + `GEMINI_CHAT_MODELS` slice for provider tables.

Test-fixture consolidation: new dev-only `crabgent-test-support` crate
(`StubProvider` + builders + recording/command fixtures), migrated across ~20
crates. Net **total Rust -538**; the metric rose **+527** because the shared
crate's `src` counts while the removed duplication is mostly in off-metric
`tests/` dirs. This was a deliberate trade for lower total code and less
fixture regrowth, chosen over the pure metric win.

## 5. Backfire levers (verified, do not re-attempt blindly)

- **`FromRow` for crabgent-store-sqlite adds lines.** SQLite stores UUID/bool/
  JSON/embedding as TEXT/INTEGER/BLOB, so every mapper still needs fallible
  `TryFrom` plus a row struct: measured +72, not -190. Postgres shrinks (native
  types), sqlite grows. Reverted.
- **The test-support dev-dependency cycle blocks inline tests** of the crates it
  depends on (core, channel, command): two copies compile, types do not unify.
  Only their `tests/` integration files can use it.
- **(resolved) crabgent-provider-openai clippy debt** in the
  `image_generation.rs` auth-refresh path (`too_many_arguments` 8/7,
  `needless_pass_by_value`) has since been cleared. The crate's clippy gate is
  green under `clippy --workspace -D warnings`.

## 6. Remaining opportunities by risk class

- **(A) Safe, small.** A few hundred lines at most: finish trimming inline-test
  scaffolding in non-core crates, minor builder dedup. Marginal.
- **(B) Done.** The shared OpenAI HTTP retry/auth-refresh lifecycle was extracted
  into `crabgent-provider-transport` (`RetryLifecycleConfig`,
  `RetryLifecycleOutcome`, `send_with_retry_lifecycle`); both `client.rs` and
  `image_generation/http.rs` now drive that shared seam, and the auth-refresh
  clippy debt is cleared. The remaining store/provider duplication is risk-class
  (C) only.
- **(C) Risk-C, NOT recommended.** Dialect-unify the store backends or merge the
  provider wire formats: ~3-6k potential prod lines, but a single bug silently
  produces wrong SQL or a wrong wire shape per backend. Only with full Postgres
  testcontainer gating and an explicit decision to accept the regression risk.

## 7. Recommendation

The metric will keep rising mainly because new **features** add code, not
because of waste; there is no large safe pile to cut. Highest-leverage moves,
in order:

1. Make new tests use `crabgent-test-support` by default so fixture duplication
   does not regrow (a lint or review note, not more cutting).
2. Treat ~95-100k as a reasonable envelope for the current feature surface;
   re-measure per feature rather than chase the threshold.
3. Pursue (B) only when someone has time for a careful auth-path pass; pursue
   (C) only if a hard ceiling is mandated and the testcontainer-gated risk is
   explicitly accepted.

Doc-only stale note (resolved): `rust-idioms.md` and `test-budget.md` claimed a
`MockProvider` lives in `crabgent-core`; it does not. The shared test double is
`StubProvider` in the dev-only `crabgent-test-support` crate, and crates inside
the test-support dependency cycle (core, channel, command) keep their own local
`MockProvider` doubles. Both rule files now state this.
