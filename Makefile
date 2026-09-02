.PHONY: build run run-verbose shot check fmt clean help

# Extra flags for the app, e.g. make run ARGS="--api-key-file ~/.itch-key"
ARGS ?=
# Config dir under ~/.config to use. kitch borrows that app's saved login
# for development; close kitch first. APP=zitch for a clean database.
APP ?= kitch
SHOT ?= /tmp/zitch.png
# Input to play before the screenshot, e.g. SCRIPT="down,down,right,enter"
SCRIPT ?=

help:
	@echo "make build        compile a debug binary"
	@echo "make run          build and launch the app"
	@echo "make run-verbose  same, logging every JSON-RPC message"
	@echo "make shot         launch, write a screenshot to \$$SHOT ($(SHOT)), exit"
	@echo "                  SCRIPT=\"down,right,enter\" plays input first"
	@echo "make check        format, lint, and type-check without running"
	@echo "make clean        remove build output"
	@echo
	@echo "APP picks the config dir under ~/.config (default $(APP)):"
	@echo "  make run APP=zitch     use a separate database instead of kitch's"
	@echo "Pass other flags with ARGS, e.g."
	@echo "  make run ARGS=\"--api-key-file ~/.itch-key\""

build:
	cargo build

run: build
	./target/debug/zitch --app-name $(APP) $(ARGS)

run-verbose: build
	./target/debug/zitch --app-name $(APP) --verbose $(ARGS)

shot: build
	./target/debug/zitch --app-name $(APP) --screenshot $(SHOT) $(if $(SCRIPT),--screenshot-script "$(SCRIPT)") $(ARGS)

check:
	cargo fmt
	cargo clippy

fmt:
	cargo fmt

clean:
	cargo clean
