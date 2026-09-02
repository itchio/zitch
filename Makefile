.PHONY: build run run-verbose shot check fmt clean help

# Extra flags for the app, e.g. make run ARGS="--api-key-file ~/.itch-key"
ARGS ?=
SHOT ?= /tmp/zitch.png

help:
	@echo "make build        compile a debug binary"
	@echo "make run          build and launch the app"
	@echo "make run-verbose  same, logging every JSON-RPC message"
	@echo "make shot         launch, write a screenshot to \$$SHOT ($(SHOT)), exit"
	@echo "make check        format, lint, and type-check without running"
	@echo "make clean        remove build output"
	@echo
	@echo "Pass flags to the app with ARGS, e.g."
	@echo "  make run ARGS=\"--api-key-file ~/.itch-key\""
	@echo "The first sign-in needs an API key from https://itch.io/user/settings/api-keys;"
	@echo "after that the saved profile is reused."

build:
	cargo build

run: build
	./target/debug/zitch $(ARGS)

run-verbose: build
	./target/debug/zitch --verbose $(ARGS)

shot: build
	./target/debug/zitch --screenshot $(SHOT) $(ARGS)

check:
	cargo fmt
	cargo clippy

fmt:
	cargo fmt

clean:
	cargo clean
