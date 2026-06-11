# crabgent

crabgent is a modular Rust kernel for agentic LLM applications.

The core crate owns the run loop, provider abstraction, tool dispatch, hook
chain, policy boundary, model registry, and typed domain values. Everything
else lives in focused workspace crates: providers, channels, persistence,
memory, cron, tasks, commands, hooks, tools, and examples.

See [PROJECT.md](PROJECT.md) for the architecture and crate map. See
[SPIRIT.md](SPIRIT.md) for the design rules that keep the workspace small,
composable, and fail-closed.

## Quick Start

Build the workspace:

```sh
cargo build --workspace
```

Run the test suite:

```sh
cargo nextest run --workspace
```

`cargo test --workspace --all-targets` is a usable fallback when nextest is not
installed.

Run Postgres integration tests against one reusable local pgvector container:

```sh
python3 tools/postgres-test-db.py run
python3 tools/postgres-test-db.py run --workspace
python3 tools/postgres-test-db.py stop
```

Run an example:

```sh
cargo run -p crabgent-examples --bin repl-min
cargo run -p crabgent-examples --bin repl-stream
cargo run -p crabgent-examples --bin inject-demo
cargo run -p crabgent-examples --bin custom-tool
```

## Security Note

The built-in `BashTool` executes commands on the host. It has bounds, timeouts,
cancellation handling, and process cleanup, but it is not a sandbox. Use it
only in trusted contexts or replace it with a sandboxed tool.

## For Contributors

Read [AGENTS.md](AGENTS.md) before making broad changes. It documents the local
engineering rules for humans and coding agents.

## License

Apache-2.0 OR MIT. See [LICENSE-APACHE](LICENSE-APACHE) and
[LICENSE-MIT](LICENSE-MIT).
