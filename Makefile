.PHONY: build release run run-verbose run-handheld run-tv shot shots check fmt clean help sync-butler handheld handheld-sysroot handheld-deploy handheld-shot handheld-love handheld-glyph handheld-stage handheld-muxapp handheld-port handheld-port-install handheld-sdl-procs run-sdl

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
	@echo "make handheld-love    fetch the LÖVE runtime the device builds ship, into target/handheld-love"
	@echo "make handheld-glyph   render handheld/glyph.svg as the muOS list icon, target/zitch-glyph.{png,svg}"
	@echo "make handheld-muxapp  package it with butler and LÖVE as target/zitch.muxapp for the muOS Archive Manager"
	@echo "make handheld-port    package the same as target/zitch.zip, a PortMaster port"
	@echo "make handheld-port-install  install the port on the device through harbourmaster over ssh"
	@echo "make handheld-sdl-procs  refresh handheld/sdl-dynapi-procs.h from SDL2's source"
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
# Routes a game's statically linked SDL2 into the firmware's; see handheld/README.md.
SDL_SHIM = target/$(HANDHELD_TARGET)/release/libzitch-sdl.so

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
	aarch64-linux-gnu-gcc -shared -fPIC -O2 -Wall -o $(SDL_SHIM) handheld/sdl-dynapi.c -ldl
	@ls -lh target/$(HANDHELD_TARGET)/release/zitch | awk '{print "target/$(HANDHELD_TARGET)/release/zitch: " $$5}'

# LÖVE for the device. muOS has no runtime of its own: the binaries ride
# inside whichever bundled app is written in LÖVE, and those move between
# releases, so itch hosts muOS's build as a redist (see itch-redists).
HANDHELD_LOVE_URL = https://broth.itch.zone/itch-redists/love-muos-11.5-arm64-linux/_1/archive/default
HANDHELD_LOVE ?= target/handheld-love
# The files in that build. A new build ships only after someone checks it
# and updates the build number and these md5s.
HANDHELD_LOVE_FILES = \
	love 3aac90fe8a035e7c9d36d88ac948cabd \
	libs/liblove-11.5.so ed09397d6f061c2ae346559d9534c23a \
	libs/libluajit-5.1.so.2 4256a24e2675e8a36da4e0859ed8cafd \
	love.LICENSE 9643614bb2e63f0649ab5b2ec7143281 \
	luajit.LICENSE a2c43bf4a9ea63755af2131b0ae59ff3
$(HANDHELD_LOVE)/love:
	rm -rf $(HANDHELD_LOVE)
	mkdir -p $(HANDHELD_LOVE)
	curl -sSfL -o $(HANDHELD_LOVE)/love.zip $(HANDHELD_LOVE_URL)
	cd $(HANDHELD_LOVE) && unzip -q love.zip && rm love.zip
	@set -- $(HANDHELD_LOVE_FILES); while [ $$# -ge 2 ]; do \
		echo "$$2  $(HANDHELD_LOVE)/$$1" | md5sum -c --quiet \
		|| { echo "error: $$1 from $(HANDHELD_LOVE_URL) does not match HANDHELD_LOVE_FILES." >&2; \
			rm -rf $(HANDHELD_LOVE); exit 1; }; \
		shift 2; \
	done
	chmod +x $(HANDHELD_LOVE)/love

handheld-love: $(HANDHELD_LOVE)/love

# The list icon mux_launch.sh names with ICON:, for the active theme's glyph
# dir. Jacaranda themes use 26x26 grayscale+alpha PNGs (color type 4, not
# RGBA); Andromeda themes use 80x80 black-filled SVGs. Both are deployed and
# the theme ignores the other. Box/grid catalogue art (GRID:) is a follow-up.
HANDHELD_GLYPH = target/zitch-glyph.png
HANDHELD_GLYPH_SVG = target/zitch-glyph.svg
HANDHELD_GLYPH_DIR = /mnt/mmc/MUOS/theme/MustardOS/glyph/muxapp
$(HANDHELD_GLYPH): handheld/glyph.svg
	rsvg-convert -w 26 -h 26 $< | magick png:- -colorspace Gray -define png:color-type=4 $@
$(HANDHELD_GLYPH_SVG): handheld/glyph.svg
	sed 's|<svg |<svg width="80" height="80" |' $< >$@

handheld-glyph: $(HANDHELD_GLYPH) $(HANDHELD_GLYPH_SVG)

handheld-deploy: handheld $(HANDHELD_LOVE)/love $(HANDHELD_GLYPH) $(HANDHELD_GLYPH_SVG)
	ssh $(HANDHELD) 'mkdir -p $(HANDHELD_APP)/libs'
	scp -q target/$(HANDHELD_TARGET)/release/zitch $(SDL_SHIM) handheld/mux_launch.sh \
		$(HANDHELD_LOVE)/love $(HANDHELD_LOVE)/love.LICENSE $(HANDHELD_LOVE)/luajit.LICENSE $(HANDHELD):$(HANDHELD_APP)/
	scp -q $(HANDHELD_LOVE)/libs/* $(HANDHELD):$(HANDHELD_APP)/libs/
	scp -q $(HANDHELD_GLYPH) $(HANDHELD):$(HANDHELD_GLYPH_DIR)/zitch.png
	scp -q $(HANDHELD_GLYPH_SVG) $(HANDHELD):$(HANDHELD_GLYPH_DIR)/zitch.svg

# The frontend's handoff files, which Andromeda moved out of /tmp. /run/muos
# exists on Jacaranda too, so ask the launch script which names it uses.
MUOS_GO = if grep -q /tmp/rom_go /opt/muos/script/mux/launch.sh 2>/dev/null; then APP_GO=/tmp/app_go; ACT_GO=/tmp/act_go; SAFE_QUIT=/tmp/safe_quit; else APP_GO=/run/muos/application; ACT_GO=/run/muos/action; SAFE_QUIT=/run/muos/safe_quit; fi

# Launch through the muOS frontend (which it kills to get the screen, as a
# menu pick would), wait for the screenshot, fetch it to /tmp/zitch-handheld.png.
# The frontend spins until safe_quit exists, so the kill must be followed by one.
handheld-shot: handheld-deploy
	echo "--screenshot /tmp/zitch.png $(ARGS)" | ssh $(HANDHELD) 'cat > $(HANDHELD_APP)/args; $(MUOS_GO); rm -f /tmp/zitch.png; echo $(HANDHELD_APP) > $$APP_GO; echo app > $$ACT_GO; kill -9 $$(pidof muxfrontend); touch $$SAFE_QUIT; while [ ! -f /tmp/zitch.png ] && [ -z "$$(pidof zitch)" ]; do sleep 0.5; done; while pidof zitch >/dev/null; do sleep 0.5; done; rm -f $(HANDHELD_APP)/args; cat $(HANDHELD_APP)/zitch.log'
	scp -q $(HANDHELD):/tmp/zitch.png /tmp/zitch-handheld.png
	@echo /tmp/zitch-handheld.png

# butler for the device, from broth's linux-arm64-head channel (the
# versioned linux-arm64 channel lags master).
HANDHELD_BUTLER ?= target/handheld-butler
$(HANDHELD_BUTLER)/butler:
	mkdir -p $(HANDHELD_BUTLER)
	curl -sSfL -o $(HANDHELD_BUTLER)/butler.zip https://broth.itch.zone/butler/linux-arm64-head/LATEST/archive/default
	cd $(HANDHELD_BUTLER) && unzip -oq butler.zip && rm butler.zip
	chmod +x $(HANDHELD_BUTLER)/butler

# The files both packages carry: the binary, the SDL shim, butler and LÖVE.
STAGE = target/handheld-stage
handheld-stage: handheld $(HANDHELD_BUTLER)/butler $(HANDHELD_LOVE)/love
	rm -rf $(STAGE)
	mkdir -p $(STAGE)/libs $(STAGE)/licenses
	cp target/$(HANDHELD_TARGET)/release/zitch $(SDL_SHIM) \
		$(HANDHELD_BUTLER)/butler $(HANDHELD_BUTLER)/7z.so $(HANDHELD_BUTLER)/libc7zip.so \
		$(HANDHELD_LOVE)/love $(STAGE)/
	cp $(HANDHELD_LOVE)/libs/* $(STAGE)/libs/
	cp LICENSE $(STAGE)/licenses/zitch.LICENSE
	cp handheld/licenses/butler.LICENSE $(STAGE)/licenses/
	cp $(HANDHELD_LOVE)/love.LICENSE $(HANDHELD_LOVE)/luajit.LICENSE $(STAGE)/licenses/
	cp assets/prompts/LICENSE-kenney.txt $(STAGE)/licenses/kenney.LICENSE
	chmod +x $(STAGE)/zitch $(STAGE)/butler $(STAGE)/love

# A muOS application archive: a zip holding the app folder, which the
# Archive Manager unpacks into the Applications menu (see handheld/README.md).
MUXAPP = target/zitch.muxapp
handheld-muxapp: handheld-stage
	rm -rf target/muxapp $(MUXAPP)
	mkdir -p target/muxapp
	cp -r $(STAGE) target/muxapp/zitch
	cp handheld/mux_launch.sh target/muxapp/zitch/
	chmod +x target/muxapp/zitch/mux_launch.sh
	cd target/muxapp && zip -rq ../zitch.muxapp zitch
	@ls -lh $(MUXAPP) | awk '{print "$(MUXAPP): " $$5}'

# A PortMaster port: the launch script beside a folder of the same name.
# PortMaster expects the binary named for its architecture.
PORT = target/zitch.zip
PORT_SCRIPT = itch.io.sh
PORT_DIR = handheld/portmaster
handheld-port: handheld-stage
	rm -rf target/port $(PORT)
	mkdir -p target/port
	cp -r $(STAGE) target/port/zitch
	mv target/port/zitch/zitch target/port/zitch/zitch.aarch64
	cp $(PORT_DIR)/$(PORT_SCRIPT) target/port/
	cp $(PORT_DIR)/port.json $(PORT_DIR)/README.md $(PORT_DIR)/gameinfo.xml target/port/
	cp $(PORT_DIR)/screenshot.png target/port/zitch/
	chmod +x target/port/$(PORT_SCRIPT)
	cd target/port && zip -rq ../zitch.zip .
	@ls -lh $(PORT) | awk '{print "$(PORT): " $$5}'

# harbourmaster is PortMaster's installer, so the port lands where a
# catalog install would.
PORTMASTER_DIR = /mnt/mmc/MUOS/PortMaster
handheld-port-install: handheld-port
	scp -q $(PORT) $(HANDHELD):/tmp/zitch.zip
	ssh $(HANDHELD) 'cd $(PORTMASTER_DIR) && PATH=/opt/python/bin:$$PATH LD_LIBRARY_PATH=/opt/python/lib ./harbourmaster --quiet --no-check install /tmp/zitch.zip; status=$$?; rm -f /tmp/zitch.zip; exit $$status'

# SDL2's jump table order on Linux, which the shim checks a game's stubs
# against and uses to name the slots that have none.
handheld-sdl-procs:
	set -o pipefail; \
	curl -sSf https://raw.githubusercontent.com/libsdl-org/SDL/SDL2/src/dynapi/SDL_dynapi_procs.h \
		| aarch64-linux-gnu-gcc -E -P -x c -D__LINUX__=1 -DHAVE_STDIO_H=1 -D'SDL_DYNAPI_PROC(rc,fn,params,args,ret)=fn' - \
		| grep '^SDL_' \
		| { printf '/* SDL2'"'"'s jump table order on Linux, from src/dynapi/SDL_dynapi_procs.h\n   on the SDL2 branch. Regenerate with `make handheld-sdl-procs`. */\nstatic const char *const UPSTREAM[] = {\n'; sed 's/.*/    "&",/'; echo '};'; } >handheld/sdl-dynapi-procs.h.new \
		&& test "$$(grep -c '^    \"SDL_' handheld/sdl-dynapi-procs.h.new)" -ge 800 \
		&& mv handheld/sdl-dynapi-procs.h.new handheld/sdl-dynapi-procs.h \
		|| { rm -f handheld/sdl-dynapi-procs.h.new; exit 1; }
	@grep -c '^    "' handheld/sdl-dynapi-procs.h

# The SDL host on the desktop, for checking it before a device round trip.
run-sdl: 
	cargo run --no-default-features --features sdl-host -- --app-name $(APP) $(ARGS)
