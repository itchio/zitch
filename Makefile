.PHONY: build release run run-verbose run-handheld run-tv shot shots check fmt clean help sync-butler handheld handheld-sysroot handheld-deploy handheld-shot run-sdl

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
	@echo "make release      compile an optimized binary to target/release/zitch"
	@echo "make run          build and launch the app"
	@echo "make run-verbose  same, logging every JSON-RPC message"
	@echo "make run-handheld lay out for a 640x480 screen (RG35XX H), scaled to the window"
	@echo "make run-tv       fullscreen, minimized while a game runs"
	@echo "make shot         launch, write a screenshot to \$$SHOT ($(SHOT)), exit"
	@echo "                  SCRIPT=\"down,right,enter\" plays input first"
	@echo "make shots        the same at 640x480, 1280x720 and 1920x1080, to /tmp/zitch-*.png"
	@echo "make check        format, lint, and type-check without running"
	@echo "make clean        remove build output"
	@echo "make sync-butler  regenerate src/butlerd/types.rs from \$$BUTLER_DIR ($(BUTLER_DIR))"
	@echo "make handheld     cross-compile the SDL host for the RG35XX H (make handheld-sysroot once first; see handheld/README.md)"
	@echo "make handheld-deploy  copy it into the muOS Applications menu over ssh"
	@echo "make handheld-shot    run it on the device headlessly and fetch a screenshot"
	@echo "make run-sdl      the SDL host on the desktop"
	@echo
	@echo "APP picks the config dir under ~/.config (default $(APP)):"
	@echo "  make run APP=zitch     use a separate database instead of kitch's"
	@echo "Pass other flags with ARGS, e.g."
	@echo "  make run ARGS=\"--api-key-file ~/.itch-key\""

build:
	cargo build

release:
	cargo build --release
	@ls -lh target/release/zitch | awk '{print "target/release/zitch: " $$5}'

run: build
	./target/debug/zitch --app-name $(APP) $(ARGS)

run-verbose: build
	./target/debug/zitch --app-name $(APP) --verbose $(ARGS)

run-handheld: build
	./target/debug/zitch --app-name $(APP) --emulate 640x480 $(ARGS)

run-tv: build
	./target/debug/zitch --app-name $(APP) --fullscreen --minimize-while-playing $(ARGS)

shot: build
	./target/debug/zitch --app-name $(APP) --screenshot $(SHOT) $(if $(SCRIPT),--screenshot-script "$(SCRIPT)") $(ARGS)

shots: build
	for size in 640x480 1280x720 1920x1080; do \
		./target/debug/zitch --app-name $(APP) --emulate $$size --screenshot /tmp/zitch-$$size.png $(if $(SCRIPT),--screenshot-script "$(SCRIPT)") $(ARGS); \
	done

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

# --- Handheld (RG35XX H running muOS) ---------------------------------------
# Cross-compiled with the aarch64-linux-gnu-gcc package against the
# firmware's own SDL2, and pushed over ssh into the muOS Applications menu.
HANDHELD ?= root@192.168.4.121
HANDHELD_TARGET = aarch64-unknown-linux-gnu
HANDHELD_APP = /mnt/mmc/MUOS/application/zitch
SYSROOT = target/handheld-sysroot/lib
# The user-local rustup toolchain that carries the aarch64 std.
CARGO_CROSS ?= $(HOME)/.cargo/bin/cargo +stable

# The device's C library, math library, loader and libgcc_s alongside its
# SDL2, so the binary binds to the symbol versions the device has (the
# package's glibc is newer). Only the startup objects come from the package.
handheld-sysroot:
	mkdir -p $(SYSROOT)
	scp -q $(HANDHELD):/usr/lib/libSDL2-2.0.so.0.2800.5 \
		$(HANDHELD):/lib/libc.so.6 $(HANDHELD):/lib/libm.so.6 \
		$(HANDHELD):/lib/ld-linux-aarch64.so.1 $(HANDHELD):/lib/libgcc_s.so.1 $(SYSROOT)/
	cp /usr/aarch64-linux-gnu/lib/Scrt1.o /usr/aarch64-linux-gnu/lib/crt1.o \
		/usr/aarch64-linux-gnu/lib/crti.o /usr/aarch64-linux-gnu/lib/crtn.o \
		/usr/aarch64-linux-gnu/lib/libc_nonshared.a $(SYSROOT)/
	ln -sf libSDL2-2.0.so.0.2800.5 $(SYSROOT)/libSDL2.so
	ln -sf libSDL2-2.0.so.0.2800.5 $(SYSROOT)/libSDL2-2.0.so.0
	ln -sf libm.so.6 $(SYSROOT)/libm.so
	ln -sf libgcc_s.so.1 $(SYSROOT)/libgcc_s.so
	echo 'GROUP ( libc.so.6 libc_nonshared.a AS_NEEDED ( ld-linux-aarch64.so.1 ) )' >$(SYSROOT)/libc.so

handheld:
	$(CARGO_CROSS) build --release --target $(HANDHELD_TARGET) --no-default-features --features sdl-host
	@ls -lh target/$(HANDHELD_TARGET)/release/zitch | awk '{print "target/$(HANDHELD_TARGET)/release/zitch: " $$5}'

handheld-deploy: handheld
	ssh $(HANDHELD) 'mkdir -p $(HANDHELD_APP)'
	scp -q target/$(HANDHELD_TARGET)/release/zitch handheld/mux_launch.sh $(HANDHELD):$(HANDHELD_APP)/

# Launch through the muOS frontend (which it kills to get the screen, as a
# menu pick would), wait for the screenshot, fetch it to /tmp/zitch-handheld.png.
handheld-shot: handheld-deploy
	echo "--screenshot /tmp/zitch.png $(ARGS)" | ssh $(HANDHELD) 'cat > $(HANDHELD_APP)/args; rm -f /tmp/zitch.png; echo $(HANDHELD_APP) > /tmp/app_go; echo app > /tmp/act_go; kill -9 $$(pidof muxfrontend); touch /tmp/safe_quit; while [ ! -f /tmp/zitch.png ] && [ -z "$$(pidof zitch)" ]; do sleep 0.5; done; while pidof zitch >/dev/null; do sleep 0.5; done; rm -f $(HANDHELD_APP)/args; cat $(HANDHELD_APP)/zitch.log'
	scp -q $(HANDHELD):/tmp/zitch.png /tmp/zitch-handheld.png
	@echo /tmp/zitch-handheld.png

# The SDL host on the desktop, for checking it before a device round trip.
run-sdl: 
	cargo run --no-default-features --features sdl-host -- --app-name $(APP) $(ARGS)
