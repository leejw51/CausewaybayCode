# Causewaybay Code, the Rust implementation: a local AI coding agent on Inkling 2 Mini
# (Apple MLX). Run from this folder, or from the repository root, which passes targets on.
#   cwbcode        the backend (model, agent, tools, SQLite; HTTP/3 + HTTP/2)
#   cwbcodeclient  the terminal client
#   web            the dashboard and web client, built into the backend

MODEL ?= inkling-2-mini:26b-a5b-nvfp4
# Where `make start` listens is decided by whether a wallet is set (remote mode) or not
# (local mode). A wallet is set up while running (web dashboard, `cwbcodeclient remote on`;
# kept in ~/.cwbcode/settings.jsonl) or fixed as CWBCODE_WALLET=0x... in the .env file at
# the repository root:
#   a wallet is set:   0.0.0.0:7443, reachable from other machines, sign-in with the wallet
#   no wallet:         127.0.0.1:7443, this machine only, with the backend's token
# BIND=addr:port overrides that either way.
BIND  ?=
ENV_FILE ?= $(abspath ../.env)
# Plain HTTP on 127.0.0.1 only, beside HTTPS (for a web view that will not take the backend's
# certificate). The Mac app does not use it: it runs a backend of its own on 7481.
APP_PORT ?= 7480
SERVE_ARGS := --env-file $(ENV_FILE) $(if $(BIND),--bind $(BIND)) --http-port $(APP_PORT)
# The Mac app (Tauri, in desktop/): the backend goes in as its sidecar, named for this machine.
TRIPLE := $(shell rustc -vV 2>/dev/null | sed -n 's/^host: //p')
APP_BUNDLE := desktop/target/release/bundle/macos/Causewaybay Code.app
# Where `make install` copies the programs.
BINDIR ?= $(HOME)/bin
SUPERVISORD   := .venv/bin/supervisord -c scripts/supervisord.conf
SUPERVISORCTL := .venv/bin/supervisorctl -c scripts/supervisord.conf
RUNNING       := [ -S $(HOME)/.cwbcode/supervisor.sock ] && $(SUPERVISORCTL) pid >/dev/null 2>&1

.PHONY: help download pull model build web install start stop restart status logs url test test-web test-playwright test-model e2e clean package package-dmg release app

help:
	@echo "make download    - download the model (20 GB): asks whether from Ollama or Hugging Face"
	@echo "make pull        - download the model with Ollama, without asking"
	@echo "make model       - where the model is and what it is"
	@echo "make build       - build the web client and both programs (release) into rust/bin"
	@echo "make install     - build, then copy cwbcode and cwbcodeclient to $(BINDIR)"
	@echo "make start       - start the backend under supervisor: local mode (127.0.0.1, the token link)"
	@echo "                   or, with a wallet set, remote mode (0.0.0.0, wallet sign-in)"
	@echo "make stop        - stop the backend"
	@echo "make status      - is it running;  make logs - follow its log;  make url - address and a one-time link"
	@echo "make test        - unit and integration tests (a scripted model, no 20 GB)"
	@echo "make package     - the Mac app (Tauri): a window on the web client, with the backend inside"
	@echo "make app         - open the app: it uses a running backend, or starts its own and stops it on quit"
	@echo "make package-dmg - the app, and a disk image of it"
	@echo "make release     - everything built, in dist/: the app zipped, its disk image, SHA-256 sums"
	@echo "make test-web    - the web client in headless Chrome"
	@echo "make test-playwright - Playwright: the agent writes a Rust crate through the web client (real model)"
	@echo "make test-model  - tests that load the model (semantic search)"
	@echo "make e2e         - real tasks through the backend with the real model"

# The model: scripts/model.py finds it (Ollama's store, then ~/.cwbcode/models), says where it
# is and what it is, and downloads it with a progress bar: through Ollama, or from Hugging Face
# (with $$HF_TOKEN or the saved `hf auth login` token when there is one, without one otherwise).
download:
	@python3 scripts/model.py download --model $(MODEL)

pull:
	@python3 scripts/model.py download --source ollama --model $(MODEL)

model:
	@python3 scripts/model.py status --model $(MODEL)

# The web client is built first: the backend embeds its files.
web:
	cd web && trunk build --release

build: web
	cargo build --release
	@# MLX's Metal library is written by mlx-sys's build script outside target/ (~/.mlx/lib/...,
	@# the path the binary names). A cached target/ (CI, or a cleared ~/.mlx) skips that script:
	@# then build mlx-sys again, which writes it.
	@metallib=$$(strings target/release/cwbcode | grep -m1 '/mlx\.metallib$$'); \
	  if [ -n "$$metallib" ] && [ ! -f "$$metallib" ]; then \
	    echo "MLX's Metal library is missing ($$metallib): building mlx-sys again"; \
	    cargo clean --release -p mlx-sys && cargo build --release; \
	  fi
	@mkdir -p bin
	@# Copied beside, then moved into place: a backend running from ./bin is not written over.
	@for p in cwbcode cwbcodeclient; do cp target/release/$$p bin/.$$p.new && mv -f bin/.$$p.new bin/$$p; done
	@echo "built: $(CURDIR)/bin/cwbcode  $(CURDIR)/bin/cwbcodeclient"

install: build
	@mkdir -p $(BINDIR)
	@cp bin/cwbcode bin/cwbcodeclient $(BINDIR)/
	@echo "installed: $(BINDIR)/cwbcode  $(BINDIR)/cwbcodeclient"
	@case ":$$PATH:" in *":$(BINDIR):"*) ;; *) echo "note: $(BINDIR) is not on your PATH";; esac

# supervisor (Python) keeps the backend running; it lives in this folder's own environment.
.venv/bin/supervisord:
	python3 -m venv .venv
	.venv/bin/pip install --quiet supervisor

start: .venv/bin/supervisord
	@test -x bin/cwbcode || { echo "bin/cwbcode is missing: run 'make build' first"; exit 1; }
	@mkdir -p $(HOME)/.cwbcode
	@if $(RUNNING); then \
		echo "already running (make stop to stop it)"; \
	else \
		bin/cwbcode url $(SERVE_ARGS) >/dev/null || exit 1; \
		CWBCODE_SERVE_ARGS="$(SERVE_ARGS) --download-model" $(SUPERVISORD) && echo "started the backend; log: $(HOME)/.cwbcode/server.log"; \
	fi
	@# The backend runs at once; a model it does not find it downloads itself (from Hugging
	@# Face, with your token when there is one), showing the progress in the web client, and
	@# loads when done. (make download fetches it beforehand instead, Ollama or Hugging Face.)
	@python3 scripts/model.py status --model $(MODEL) >/dev/null 2>&1 || echo "The model is not here yet: the backend is downloading it (about 20 GB); open the web client to watch, it loads by itself when done."
	@sleep 1; bin/cwbcode url $(SERVE_ARGS)
	@if bin/cwbcode url $(SERVE_ARGS) | grep "^bind  *0\.0\.0\.0" >/dev/null; then \
		echo "Reachable from other machines: whoever holds the wallet's recovery phrase can run commands here as you."; \
	else \
		echo "This machine only: open the 'open' link above (it works once, within 5 minutes; make url prints a new one). To use it from other machines turn remote mode on: the web dashboard, or bin/cwbcodeclient remote on <wallet address>."; \
	fi

stop:
	@if $(RUNNING); then \
		$(SUPERVISORCTL) shutdown >/dev/null; \
		i=0; while [ -f $(HOME)/.cwbcode/supervisord.pid ] && [ $$i -lt 60 ]; do sleep 0.5; i=$$((i+1)); done; \
		echo "stopped"; \
	else \
		echo "not running"; \
	fi

restart: stop start

status:
	@if $(RUNNING); then $(SUPERVISORCTL) status; else echo "not running"; fi

logs:
	tail -f $(HOME)/.cwbcode/server.log

url:
	@bin/cwbcode url $(SERVE_ARGS)

# The integration tests start target/release/cwbcode, so it is built first.
test:
	cargo build --release
	cargo test --release -p cwbcode -p cwbcode-proto
	cargo test --release -p cwbcodeclient --lib --bins
	cd web && cargo test
	@# Each of these starts a backend of its own; one at a time, so that timing (the 1.5 s
	@# given to HTTP/3 before falling back) does not depend on how busy the machine is.
	cargo test --release -p cwbcodeclient --test backend -- --test-threads=1

test-web: build
	python3 scripts/web_test.py

# The agent is asked, through the web client in Chrome, to write and extend a Rust crate;
# the crate must build and pass tests it has never seen. Real model: stop the backend first.
test-playwright: build
	cd tests/playwright && npm install --no-audit --no-fund --silent && npx playwright test

test-model:
	cargo test --release -p cwbcode -- --ignored

e2e: build
	scripts/e2e.sh

# The Mac app. The backend (with the web client inside it) is copied in as the app's sidecar,
# and MLX's Metal library into the app's Resources (the backend tells MLX to load it from
# there; otherwise it would need the path this machine's build wrote it to). The bundle is
# signed ad hoc (tauri.conf.json: signingIdentity "-"), and the signature is checked.
# `cargo tauri` comes from `cargo install tauri-cli`.
package: build
	@command -v cargo-tauri >/dev/null || { echo "cargo-tauri is missing: cargo install tauri-cli --version '^2'"; exit 1; }
	@test -n "$(TRIPLE)" || { echo "cannot tell this machine's target triple (is rustc on PATH? rustc -vV)"; exit 1; }
	@mkdir -p desktop/binaries
	cp target/release/cwbcode desktop/binaries/cwbcode-$(TRIPLE)
	@metallib=$$(strings target/release/cwbcode | grep -m1 '/mlx\.metallib$$'); \
	  test -f "$$metallib" || { echo "MLX's Metal library was not found ($$metallib)"; exit 1; }; \
	  cp "$$metallib" desktop/binaries/mlx.metallib
	cd desktop && cargo tauri build --bundles app
	codesign --verify --deep --strict "$(APP_BUNDLE)"
	@echo "built: $(CURDIR)/$(APP_BUNDLE)"
	@echo "open it with: make app   (or drag it to /Applications)"

# The disk image too. (Making it drives the Finder, which needs a logged-in desktop.)
# (CI=true: Tauri's disk-image script then skips laying out the Finder window by AppleScript,
# which times out with no desktop session to drive, in CI or a background shell.)
package-dmg: package
	@# (Both bundles: asked for the disk image alone, Tauri removes the app it made it from.)
	cd desktop && CI=true cargo tauri build --bundles app,dmg
	codesign --verify --deep --strict "$(APP_BUNDLE)"
	@ls desktop/target/release/bundle/dmg/*.dmg

# A release: everything built and signed, in dist/:
#   Causewaybay-Code-<version>-macos-arm64.dmg      the Mac app (backend inside), on a disk image
#   Causewaybay-Code-<version>-macos-arm64.app.zip  the same app, zipped
#   cwbcode-<version>-macos-arm64.zip               cwbcode and cwbcodeclient on their own, with
#                                                   MLX's Metal library beside them
#   SHA256SUMS
# Signing, as in FruitNationMessenger: with a Developer ID (APPLE_SIGNING_IDENTITY, or the one in
# the keychain) everything is signed with the hardened runtime and a timestamp; with APPLE_ID,
# APPLE_PASSWORD and APPLE_TEAM_ID too (or APPLE_NOTARY_PROFILE), Apple notarizes it and the app
# and the disk image get their tickets stapled. Without a Developer ID, everything is signed ad
# hoc. The same command on a Mac and in CI (.github/workflows/release-macos.yml).
VERSION := $(shell sed -n 's/^  "version": "\(.*\)",/\1/p' desktop/tauri.conf.json)
RELEASE_NAME := Causewaybay-Code-$(VERSION)-macos-arm64
CLI_NAME := cwbcode-$(VERSION)-macos-arm64
release: export APPLE_SIGNING_IDENTITY ?= $(shell scripts/apple-identity.sh)
release: package-dmg
	@echo "signing identity: $$APPLE_SIGNING_IDENTITY"
	@rm -rf dist && mkdir -p dist/$(CLI_NAME)
	@# The disk image: Tauri signed and notarized the app in it; the image itself is signed,
	@# notarized and stapled here.
	cp desktop/target/release/bundle/dmg/*.dmg "dist/$(RELEASE_NAME).dmg"
	scripts/sign-dmg.sh "dist/$(RELEASE_NAME).dmg"
	ditto -c -k --keepParent "$(APP_BUNDLE)" "dist/$(RELEASE_NAME).app.zip"
	@# The programs on their own, each signed and notarized.
	cp target/release/cwbcode target/release/cwbcodeclient desktop/binaries/mlx.metallib dist/$(CLI_NAME)/
	scripts/codesign-binary.sh dist/$(CLI_NAME)/cwbcode
	scripts/codesign-binary.sh dist/$(CLI_NAME)/cwbcodeclient
	cd dist && ditto -c -k --keepParent $(CLI_NAME) $(CLI_NAME).zip && rm -rf $(CLI_NAME)
	cd dist && shasum -a 256 *.zip *.dmg > SHA256SUMS
	@echo "release $(VERSION) in $(CURDIR)/dist:"; ls -l dist
	@codesign -dv "$(APP_BUNDLE)" 2>&1 | grep -E "^Authority=|^Signature=" | head -2 || true
# The app runs a backend of its own (127.0.0.1 only, its own ports: 7481 for the window, 7453
# for HTTPS), beside any `make start` one; it stops when the app quits.
app:
	@test -d "$(APP_BUNDLE)" || { echo "no app yet: run make package"; exit 1; }
	open "$(APP_BUNDLE)"

clean:
	cargo clean
	rm -rf desktop/target desktop/binaries desktop/gen
