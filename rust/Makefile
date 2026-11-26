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
SERVE_ARGS := --env-file $(ENV_FILE) $(if $(BIND),--bind $(BIND))
# Where `make install` copies the programs.
BINDIR ?= $(HOME)/bin
SUPERVISORD   := .venv/bin/supervisord -c scripts/supervisord.conf
SUPERVISORCTL := .venv/bin/supervisorctl -c scripts/supervisord.conf
RUNNING       := [ -S $(HOME)/.cwbcode/supervisor.sock ] && $(SUPERVISORCTL) pid >/dev/null 2>&1

.PHONY: help download pull model build web install start stop restart status logs url test test-web test-playwright test-model e2e clean

help:
	@echo "make download    - download the model (20 GB): asks whether from Ollama or Hugging Face"
	@echo "make pull        - download the model with Ollama, without asking"
	@echo "make model       - where the model is and what it is"
	@echo "make build       - build the web client and both programs (release) into rust/bin"
	@echo "make install     - build, then copy cwbcode and cwbcodeclient to $(BINDIR)"
	@echo "make start       - start the backend under supervisor: local mode (127.0.0.1, the token link)"
	@echo "                   or, with a wallet set, remote mode (0.0.0.0, wallet sign-in)"
	@echo "make stop        - stop the backend"
	@echo "make status      - is it running;  make logs - follow its log;  make url - address and token"
	@echo "make test        - unit and integration tests (a scripted model, no 20 GB)"
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
		python3 scripts/model.py ensure --model $(MODEL) || exit 1; \
		bin/cwbcode url $(SERVE_ARGS) >/dev/null || exit 1; \
		CWBCODE_SERVE_ARGS="$(SERVE_ARGS)" $(SUPERVISORD) && echo "started the backend; log: $(HOME)/.cwbcode/server.log"; \
	fi
	@sleep 1; bin/cwbcode url $(SERVE_ARGS)
	@if bin/cwbcode url $(SERVE_ARGS) | grep "^bind  *0\.0\.0\.0" >/dev/null; then \
		echo "Reachable from other machines: whoever holds the wallet's recovery phrase can run commands here as you."; \
	else \
		echo "This machine only: open the 'open' link above (it carries the token). To use it from other machines turn remote mode on: the web dashboard, or bin/cwbcodeclient remote on <wallet address>."; \
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

clean:
	cargo clean
