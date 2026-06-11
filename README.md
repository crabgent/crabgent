# crabgent source package

This package contains the crabgent runtime app and the crabgent library crates it
depends on. It ships as source code only. There is no binary inside the ZIP.

The package is meant for local use first: terminal UI, web dashboard, local
SQLite storage, and one starter agent. Matrix, Telegram, voice, image handling,
and local command or file tools can be enabled later when needed.

## What You Can Do With It

Use crabgent as a local assistant hub. You can keep separate named sessions,
store useful memory, ask the agent to search that memory, and give it longer
background tasks while you keep working. The web dashboard helps inspect
sessions, memory, scheduled jobs, and voice features.

Typical requests:

```text
Create a new session named "project-planning".
Remember that the launch checklist lives in this session.
Search your memory for everything about the onboarding project.
Start a background task: read these notes and list open questions.
Draft a short reply I can send to the team.
Compare these two options and tell me what you would do.
```

## Layout

```text
app/       local runtime app and UI
crabgent/   library crates used by the app
scripts/   helper scripts
data/      local runtime data, empty by default
```

The app uses path dependencies pointing at `../crabgent`, so it builds without a
separate checkout.

## Requirements

Install Rust. The repo pins the expected toolchain through
`crabgent/rust-toolchain.toml`; rustup will download it when needed.

On macOS, install the Xcode command line tools:

```sh
xcode-select --install
```

On Linux, install a normal build toolchain, `pkg-config`, and OpenSSL
development headers if your distribution needs them for native dependencies.

## Build

From the package root:

```sh
make build
```

This builds `target/release/crabgent`.

On macOS the helper script applies local ad-hoc codesigning only. It does not
use Developer ID signing and does not notarize. On Linux it just builds.

Manual build:

```sh
cd app
CARGO_TARGET_DIR=../target cargo build --release --bin crabgent
```

## First Run

Create a local config and data directory:

```sh
cp config.toml.example config.toml
mkdir -p data
```

Login with OpenAI OAuth:

```sh
make login
```

Start the runtime:

```sh
make run
```

In another terminal, start the TUI:

```sh
make tui
```

Open the dashboard while the runtime is running:

```text
http://127.0.0.1:3100/admin
```

The dashboard token is `[web].auth_token` in `config.toml`. Change it before
using the dashboard beyond localhost.

## Configuration

The example config starts one local agent named `local`.

Keep these files private:

```text
config.toml
data/
target/
```

Use `config.toml.example` as the shared template. Do not put API keys, OAuth
tokens, chat tokens, or private paths into shared files.

## Useful Commands

```sh
make check
make build
make login
make run
make tui
./target/release/crabgent --help
```
