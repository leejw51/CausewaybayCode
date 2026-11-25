# Causeway Bay Code

An AI coding agent and general assistant that runs entirely on your Mac. The model is
Inkling 2 Mini, run natively on Apple MLX in Rust with its draft network for speculative
decoding; nothing is sent to any service.

| Program | |
|---|---|
| `cwbcode` | the **backend**: holds the model, runs the agent and its tools in any folder under your home folder, keeps everything in SQLite |
| `cwbcodeclient` | the **terminal client**: works on the current folder through the backend |
| web | a **dashboard and client** in the browser (Rust, Leptos, WebAssembly; movement by [Motion](https://motion.dev)), served by the backend |

They talk in Cap'n Proto messages over **HTTP/3**, or **HTTP/2** where QUIC is not available,
with a self-signed certificate and a token.

```
› Read greet.py, then fix the typo in its greeting word using edit_file.

⏺ read_file(greet.py)
⏺ edit_file(greet.py)

? Edit greet.py
  -     return "helo " + name
  +     return "hello " + name
  Allow? [y]es / [a]lways this session / [n]o, or say what to do instead: y
Fixed the typo in greet.py: `"helo "` → `"hello "`.
[3.8k tokens in context · wrote 206 · 108 tok/s · 2 tool calls · 4.3s]
```

What the agent does:

- **Codes**: reads, searches, writes and edits files, runs commands, checks its own work,
  keeps a task list for longer work.
- **Is a general agent too**: answers questions, reads images, works on any files.
- **Makes its own tools at runtime**: when a task needs a capability it lacks, it writes a
  tool in Rust, which is compiled, saved and callable from then on, in every project.
- **Remembers**: each project has a knowledge base of notes, files and images, searched by
  BM25 and by meaning together.
- **Keeps everything**: every conversation, every event in it, and every version of every
  file it read or changed are in SQLite, so a turn's changes can be undone.
- **Is safe to leave running**: no tool reaches the network, commands run in the macOS
  sandbox, files change only inside the project, and by default every edit and command is
  shown to you first.

## Requirements

- Apple Silicon Mac with enough memory: the model takes 20 GB and a session peaks around
  27 GB (developed and tested on a 36 GB M5 Max)
- Rust with the `wasm32-unknown-unknown` target, `cmake`, `capnp` and `trunk`
  (`brew install cmake capnp trunk`; `rustup target add wasm32-unknown-unknown`)
- [Ollama](https://ollama.com), only to download the model; Python 3 for `make start`

## Build and run

```sh
make pull       # ollama pull inkling-2-mini:26b-a5b-nvfp4  (20 GB, once)
make build      # web client, then both programs in release mode, copied to ./bin
make install    # the same, and copies cwbcode and cwbcodeclient to ~/bin (prints where)
make start      # start the backend under supervisor, on 0.0.0.0:7443
make stop       # stop it
```

`make start` prints the web address and the token; `make url` prints them again, `make
status` and `make logs` show how it is doing. The first build compiles MLX from source and
takes a few minutes. The weights are read straight from Ollama's store; Ollama does not need
to be running (and if it has the model loaded, stop it first: two copies do not fit).

**`make start` listens on every interface**, so that other machines on your network can use
the backend. Anyone who has the token can make the agent run commands on this Mac as you:
keep the token private. `make start BIND=127.0.0.1:7443` keeps the backend to this machine;
so does running `cwbcode serve` yourself, which binds 127.0.0.1 unless told otherwise.

### The terminal client

```sh
cd ~/code/myapp
cwbcodeclient                                # talk to the agent about this folder
cwbcodeclient "add a --json flag to the cli" # start with a message
cwbcodeclient -p "explain src/main.rs"       # answer once and exit
git diff | cwbcodeclient -p "review this"    # stdin is added to the prompt
cwbcodeclient -c                             # continue this folder's last conversation
cwbcodeclient -i chart.png "what does this show?"
cwbcodeclient status | projects | sessions | tools | undo
cwbcodeclient kb add notes.md design.png     # kb search "..." | list | get | export | rm
```

| Option | |
|---|---|
| `--auto` | edits in the project and sandboxed commands run without asking |
| `--read-only` | reads and searches only |
| `--think none\|minimal\|low\|medium\|high\|max` | how much the model thinks first |
| `-C DIR` | another project folder |
| `--server URL`, `--token T`, `--cert PEM` | a backend elsewhere (by default: this machine's, with the token and certificate in `~/.cwbcode`) |
| `--insecure` | accept any certificate (encrypted, server not verified) |
| `--h2` | HTTP/2 without trying HTTP/3 |

With `-p`, nobody is there to approve anything: edits and commands are refused unless
`--auto` is given. In the conversation, `/help` lists the commands: `/image`, `/file`,
`/remember`, `/add`, `/kb`, `/search`, `/load`, `/export`, `/forget`, `/save`, `/sessions`,
`/resume`, `/undo`, `/changes`, `/todos`, `/compact`, `/tools`, `/mode`, `/think`, `/status`,
`!command`. Ctrl-C stops the model or a running command; Ctrl-D leaves.

### The web client

Open the address `make start` printed (`https://localhost:7443`), accept the self-signed
certificate, and enter the token.

- **Dashboard**: model state, memory, context in use, protocol, projects.
- **Chat**: open any folder under `~`, talk to the agent, watch its thinking and tool calls,
  approve edits and commands with a click, attach images, undo a turn's changes, switch
  between `ask first`, `auto` and `read-only`.
- **Knowledge**: the project's knowledge base: search, save notes, upload files.
- **Tools**: the tools the agent has made, with their Rust source.

## Where everything is kept

All of it is SQLite, in two places:

| | |
|---|---|
| `~/.cwbcode/` | the backend's own: `cwbcode.db` (projects it has opened, tools the agent has made), compiled tools, `cert.pem` and `key.pem`, `token`, logs |
| `<project>/.cwbcode/cwbcode.db` | one per project (the folder ignores itself in git): conversations, an append-only log of every event (messages, thinking, tool calls and results, approvals and answers), every version of every file the agent read or changed, the knowledge base with its files and images byte for byte |

A project is any folder under your home folder (also through a link placed in it, such as
`~/vivid -> /Volumes/disk/vivid`). The home folder itself, credentials (`~/.ssh`, `~/Library`,
...) and `~/.cwbcode` are refused.

## How it works

**Model** (`rust/server/src/inkling`, `llm.rs`). Inkling 2 Mini is a 26B-parameter
routed-expert model with 5B active per token. The MLX port and its draft network come from
[CausewaybayMaker](../CausewaybayMaker): the draft network proposes up to seven tokens, the
model checks them in one pass and keeps the run it agrees with. Measured on an M5 Max: 60 to
110 tokens/s while writing; a 15k-token prompt is read in about 12 s. The conversation lives
in the model's KV cache, so each step reads only what is new.

**One model thread** (`worker.rs`). MLX state is per thread and the model is 20 GB, so
everything that needs it (a chat turn, embeddings, a summary) is a job on one thread, one at
a time. One conversation's cache is in memory; asking for another reads it back from its
store. Everything else (status, lists, transcripts, keyword search, approvals, interrupt) is
answered at once, also while the model works.

**Protocol** (`rust/proto`, `schema/cwbcode.capnp`). `POST /api/call` takes a `Request` and
answers a `Response`; `POST /api/chat` answers a stream of `Event`s, each framed by its
length, with an empty frame as heartbeat. The same messages are used by the terminal client
and, compiled to WebAssembly, by the web client.

**Transport** (`net.rs`, `rust/client/src/net.rs`). The backend listens on a TCP port for
HTTP/2 over TLS and on the UDP port of the same number for HTTP/3 over QUIC, with one
self-signed certificate made on first start. The terminal client tries HTTP/3 for 1.5 s and
falls back to HTTP/2; it trusts exactly the backend's certificate (pinned), whatever name
the backend is reached by. A browser decides for itself: with a self-signed certificate
browsers do not use QUIC, so **the web client runs over HTTP/2** (the dashboard shows which).

**Agent** (`agent.rs`, `prompt.rs`). The model's own chat format, as Ollama's `inkling2`
renderer writes it; the model's turn is read token by token into thinking, answer and tool
calls; calls are run and their results fed back until it answers without calling one.
Control tokens are written only by cwbcode: in anything else (what you type, file contents,
tool output) the spelling of a control token stays plain text, so a file cannot pose as the
system or the user. Past `--context` tokens the model's context is replaced by its own
summary; the log keeps the full transcript.

**Tools** (`tools.rs`): `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`,
`bash`, `view_image`, `todo_write`, `memory_save`, `memory_save_file`, `memory_search`,
`memory_get`, `make_tool`, `run_tool`, `list_tools`.

**Tools made at runtime** (`dyntool.rs`). `make_tool` takes a name, a description, a JSON
Schema for the arguments and one Rust source file defining
`fn run(args: &Json) -> Result<String, String>` (standard library only). cwbcode appends a
small `Json` type and a `main`, compiles it with `rustc` and stores the source in
`~/.cwbcode/cwbcode.db`. A compile error goes back to the model, which fixes its source.

**Knowledge base** (`kb.rs`). A search runs two rankings and merges them by reciprocal rank
fusion: *BM25* over an FTS5 index, with the query's words as prefixes (so `배포` finds
`배포는`), weighted by the share of the query's words a match contains; and *semantic*, the
cosine similarity of embeddings. An embedding is the model's own output state after reading
the text and being asked what it is about, so there is no second model, and a Korean
question finds an English note.

**Safety** (`sandbox.rs`, `projects.rs`, `api.rs`).

- Every API request needs the token (`~/.cwbcode/token`, readable only by you).
- There are no network tools. Commands and made tools run under `sandbox-exec` with the
  network denied and writes allowed only in the project, temporary folders and package
  caches. `cwbcode serve --allow-network` opens the network to commands; with
  `--no-sandbox` every command needs your approval, whatever the mode.
- File tools write only inside the project, resolved through symlinks first.
- Credentials, the backend's token and key, and each project's `.cwbcode` records are not
  readable by the file tools or by sandboxed commands.
- Modes: `ask` (default) shows each edit and command and waits for an answer; `auto` does
  not ask; `read-only` changes nothing. An approval nobody answers (a closed tab, an
  interrupt, 15 minutes) counts as no.

## Tests

```sh
make test         # unit tests, and the backend started for real with a scripted model:
                  # HTTP/3 and the HTTP/2 fallback, token, certificate pinning, folders
                  # outside home, approvals (answered, abandoned, interrupted), the event
                  # log, file versions and undo, the knowledge base, tools made at runtime
make test-web     # the web client in headless Chrome: sign-in, a project, chat, approval,
                  # undo, history, dashboard, knowledge base
make test-playwright   # Playwright drives the web client with the real model: the agent
                  # writes a Rust crate and extends it; the crate must build, pass its own
                  # tests and pass tests it has never seen (needs Node and Google Chrome)
make test-model   # semantic search with the model: paraphrases, Korean against English
make e2e          # real tasks with the real model through the backend and the client
```

## Limits

- One model: Inkling 2 Mini from Ollama's store. macOS on Apple Silicon only.
- One request at a time uses the model; others wait their turn.
- Memory grows with the context: 27 GB at the peak with 25k tokens, 31 GB with 48k (measured).
- What a command (or a made tool) changes is versioned and undone like an edit only in
  projects of up to 4,000 files outside build folders (files up to 256 KB each); in larger
  ones, undo covers `write_file` and `edit_file`. Undo never touches a file that has changed
  since the agent wrote it.
- A tool made in the middle of a conversation is called through `run_tool` until the next
  conversation, where it is declared under its own name.
- PDFs and other non-text files are stored but not read: they are found by their title.
- Saving a large text takes time: each chunk is a pass of the model (a 66 KB file took 20 s).
- No web access, by design.

## License

MIT. `rust/server/src/inkling` is derived from Ollama's MLX runner, and the web client
ships Motion; see `THIRD_PARTY_NOTICES`.
