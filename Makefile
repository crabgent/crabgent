.PHONY: build check run tui login clean

build:
	./scripts/build-local.sh

check:
	cd app && CARGO_TARGET_DIR=../target cargo check --bin crabgent

login:
	./target/release/crabgent --config config.toml openai-login

run:
	./target/release/crabgent --config config.toml run

tui:
	./target/release/crabgent --config config.toml tui

clean:
	rm -rf target
