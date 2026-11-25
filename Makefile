# Causeway Bay Code: a local AI coding agent on Inkling 2 Mini (Apple MLX).
#   cwbcode        the backend (model, agent, tools, SQLite; HTTP/3 + HTTP/2)
#   cwbcodeclient  the terminal client
#   web            the dashboard and web client, built into the backend

MODEL ?= inkling-2-mini:26b-a5b-nvfp4
# Where `make start` listens. 0.0.0.0 makes the backend reachable from other machines
# (they need the token: `make url`). Use BIND=127.0.0.1:7443 to keep it to this machine.
BIND  ?= 0.0.0.0:7443
# Where `make install` copies the programs.
BINDIR ?= $(HOME)/bin
SUPERVISORD   := .venv/bin/supervisord -c scripts/supervisord.conf
SUPERVISORCTL := .venv/bin/supervisorctl -c scripts/supervisord.conf
RUNNING       := [ -S $(HOME)/.cwbcode/supervisor.sock ] && $(SUPERVISORCTL) pid >/dev/null 2>&1

.PHONY: help pull build web install start stop restart status logs url test test-web test-playwright test-model e2e clean

help:
	@echo "make pull        - download the model into Ollama's store (ollama pull $(MODEL), 20 GB)"
	@echo "make build       - build the web client and both programs (release) into ./bin"
	@echo "make install     - build, then copy cwbcode and cwbcodeclient to $(BINDIR)"
	@echo "make start       - start the backend under supervisor on $(BIND)"
	@echo "                   (BIND=127.0.0.1:7443 keeps it to this machine)"
	@echo "make stop        - stop the backend"
	@echo "make status      - is it running;  make logs - follow its log;  make url - address and token"
	@echo "make test        - unit and integration tests (a scripted model, no 20 GB)"
	@echo "make test-web    - the web client in headless Chrome"
	@echo "make test-playwright - Playwright: the agent writes a Rust crate through the web client (real model)"
	@echo "make test-model  - tests that load the model (semantic search)"
	@echo "make e2e         - real tasks through the backend with the real model"

pull:
	ollama pull $(MODEL)

# The web client is built first: the backend embeds its files.
web:
	cd rust/web && trunk build --release

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
		CWBCODE_BIND=$(BIND) $(SUPERVISORD) && echo "started the backend on $(BIND); log: $(HOME)/.cwbcode/server.log"; \
	fi
	@sleep 1; bin/cwbcode url --bind $(BIND)
	@case "$(BIND)" in 0.0.0.0*) echo "Reachable from other machines: anyone with the token can run commands here as you.";; esac

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
	@bin/cwbcode url --bind $(BIND)

# The integration tests start target/release/cwbcode, so it is built first.
test:
	cargo build --release
	cargo test --release -p cwbcode -p cwbcode-proto
	cargo test --release -p cwbcodeclient --lib --bins
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
