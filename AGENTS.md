# Agent Instructions

You are working inside a shareable crabgent source package.

Rules:

* Keep the package local-first. Do not require Matrix or Telegram for setup.
* Use `app/` for the runnable runtime app.
* Use `crabgent/` for upstream library crates.
* Build from the package root with `make build`, or from `app/` with
  `CARGO_TARGET_DIR=../target cargo build --release --bin crabgent`.
* Do not write secrets to shared files. `config.toml`, `data/`, and `target/`
  are local state.
* On macOS, use local ad-hoc signing only for the built binary. Do not use
  Developer ID signing in this source package.
* Linux should build with the same Cargo command. If native packages are
  missing, tell the user the distro package names to install.
* Keep changes small and follow the existing Rust style.

First-run flow:

```sh
cp config.toml.example config.toml
mkdir -p data
make build
make login
make run
make tui
```
