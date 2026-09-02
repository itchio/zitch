.PHONY: build run run-verbose shot check fmt clean help sync-butler

# Extra flags for the app, e.g. make run ARGS="--api-key-file ~/.itch-key"
ARGS ?=
# Config dir under ~/.config to use. kitch borrows that app's saved login
# for development; close kitch first. APP=zitch for a clean database.
APP ?= kitch
SHOT ?= /tmp/zitch.png
# A butler checkout, for regenerating src/butlerd/types.rs.
BUTLER_DIR ?= ../butler
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
	@echo "make sync-butler  regenerate src/butlerd/types.rs from \$$BUTLER_DIR ($(BUTLER_DIR))"
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

sync-butler:
	cd $(BUTLER_DIR) && go run ./butlerd/generous rust $(CURDIR)/src/butlerd/types.rs
	rustfmt src/butlerd/types.rs

clean:
	cargo clean
