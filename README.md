# Causewaybay Code (CC)

Causewaybay Code, CC for short, is an AI coding agent and general assistant that runs
entirely on your Mac. The model is
Inkling 2 Mini, run natively on Apple MLX in Rust with its draft network for speculative
decoding; nothing is sent to any service.

The programs keep the names `cwbcode` and `cwbcodeclient`: a program called `cc` would take
the place of the C compiler on your PATH.

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
- **Is confined**: no tool reaches the network, commands run in the macOS sandbox, files
  change only inside the project, and by default every edit and command is shown to you
  first. What that does and does not cover is under [Safety](#how-it-works).

## Requirements

- Apple Silicon Mac with enough memory: the model takes 20 GB and a session peaks around
  27 GB (developed and tested on a 36 GB M5 Max)
- Rust with the `wasm32-unknown-unknown` target, `cmake`, `capnp` and `trunk`
  (`brew install cmake capnp trunk`; `rustup target add wasm32-unknown-unknown`)
- [Ollama](https://ollama.com), only to download the model; Python 3 for `make start`

## Build and run

```sh
make pull       # ollama pull inkling-2-mini:26b-a5b-nvfp4  (20 GB, once)
make build      # web client, then both programs in release mode, copied to rust/bin
make install    # the same, and copies cwbcode and cwbcodeclient to ~/bin (prints where)
make start      # start the backend under supervisor (0.0.0.0 with CWBCODE_WALLET in .env, else 127.0.0.1)
make stop       # stop it
```

The implementation is in `rust/`; other languages will get folders of their own beside it.
The `make` targets work from the repository root (which passes them on) or from `rust/`.

`make start` prints the web address and the token; `make url` prints them again, `make
status` and `make logs` show how it is doing. The first build compiles MLX from source and
takes a few minutes. The weights are read straight from Ollama's store; Ollama does not need
to be running (and if it has the model loaded, stop it first: two copies do not fit).

**Local mode and remote mode.** The backend starts in local mode. Setting the wallet that
owns it turns remote mode on:

| | local mode (no wallet) | remote mode (a wallet is set) |
|---|---|---|
| listens on | `127.0.0.1:7443`: this machine only | `0.0.0.0:7443`: your network and tailnet too |
| login | none | required: sign in with that wallet |
| web client | opens straight into the app | asks for the wallet's recovery phrase |
| terminal client | just works | on this Mac just works; elsewhere `cwbcodeclient login` |

Turn it on from the web client's **Dashboard → Remote access** (type the wallet's address,
or its recovery phrase to have the address worked out in the browser), or from the terminal:

```sh
cwbcodeclient remote                  # which mode, and where the backend listens
cwbcodeclient remote on 0x5290…9EE7   # or `remote on` alone, to type the recovery phrase
cwbcodeclient remote off              # back to this machine only, no login
```

It takes effect at once, without a restart: the backend moves its listeners to the new
address and starts (or stops) asking for the wallet. The setting is kept in
`~/.cwbcode/settings.jsonl`, one JSON object per line, the last line in force:

```json
{"time":"2026-11-25 23:40:12","set":"wallet","value":"0x52908400098527886E0F7030069857D2E4169EE7"}
{"time":"2026-11-26 09:02:51","set":"wallet","value":""}
```

Turning it on needs no login (in local mode whoever is at this machine may); turning it off
needs the wallet's owner to be signed in, or the terminal client on this machine. A
conversation in progress is cut short when the listeners move.

Alternatively, fix the wallet in a `.env` file at the repository root (or the environment):
`CWBCODE_WALLET=0x…`. That wins over the setting, and is then changed only there.

`make start` prints the mode, the wallet, the bind address and the kind of login.

**Signing in with the wallet.** The web client asks for the wallet's recovery phrase (12 to
24 words, and an account number if it is not the first). The key is derived in the browser
(BIP-39, path `m/44'/60'/0'/0/n`, as every Ethereum wallet does), it signs a one-time
challenge from the backend (EIP-191, naming the wallet and the backend's certificate, good
for five minutes, once), and the backend checks that the signature is the wallet's. The
phrase is never sent. A phrase of another wallet is told so before anything is sent.

With **remember on this device** ticked (it is by default), the phrase is kept in the
browser's storage beside the session, so that a refresh, or a session that has ended (they
last 30 days), signs in again without typing it. **Sign out** removes both. Anyone who can
use that browser profile can read what it stores: untick the box on a device that is not
yours alone.

Only the wallet's public address is ever saved or put in `.env`: a private key or recovery
phrase there is refused (and never echoed). `.env` is git-ignored. `make start
BIND=addr:port` overrides the bind
address; a backend listening beyond this machine without a wallet asks for its token
(`make url` prints it), as `cwbcode serve --require-token` always does.

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
cwbcodeclient login                          # a backend with a wallet, from another machine
cwbcodeclient kb add notes.md design.png     # kb search "..." | list | get | export | rm
```

| Option | |
|---|---|
| `--auto` | edits in the project and sandboxed commands run without asking |
| `--read-only` | reads and searches only |
| `--think none\|minimal\|low\|medium\|high\|max` | how much the model thinks first |
| `-C DIR` | another project folder |
| `--server URL`, `--cert PEM` | a backend elsewhere (by default: this machine's, with its certificate in `~/.cwbcode`); sign in once with `cwbcodeclient --server URL --cert PEM login` |
| `--token T` | a session or the backend's token, instead of the saved one |
| `--insecure` | accept any certificate (encrypted, server not verified) |
| `--h2` | HTTP/2 without trying HTTP/3 |

With `-p`, nobody is there to approve anything: edits and commands are refused unless
`--auto` is given. In the conversation, `/help` lists the commands: `/image`, `/file`,
`/remember`, `/add`, `/kb`, `/search`, `/load`, `/export`, `/forget`, `/save`, `/sessions`,
`/resume`, `/undo`, `/changes`, `/todos`, `/compact`, `/tools`, `/mode`, `/think`, `/status`,
`!command`. Ctrl-C stops the model or a running command; Ctrl-D leaves.

### The web client

Open the address `make start` printed (`https://localhost:7443`) and accept the self-signed
certificate. In local mode the app opens at once; with a wallet, sign in with its recovery
phrase.

- **Dashboard**: model state, memory, context in use, protocol, projects, remote access.
- **Chat**: open any folder under `~` (one that does not exist is created, with the folders
  above it), talk to the agent, watch its thinking and tool calls,
  approve edits and commands with a click, attach images, undo a turn's changes, switch
  between `ask first`, `auto` and `read-only`.
- **Knowledge**: the project's knowledge base: search, save notes, upload files.
- **Tools**: the tools the agent has made, with their Rust source.

Three controls beside the name: **light / dark**, **side by side / stacked** (the sidebar
becomes a header), and the **language**: English, 한국어, Čeština, 日本語, 廣東話, 中文,
Español, Türkçe (the interface; what the backend and the model say arrives as it is).
These, and where you are (view, project, conversation), are kept in the browser, so a
refresh comes back to the same place.

Movement is Motion's: what travels (a view arriving, a card appearing, a number changing)
eases in and out exponentially; what answers a touch (a pressed button, the selection
sliding to the chosen view) follows a spring, as on iOS. It is switched off for "reduce
motion".

## Where everything is kept

All of it is SQLite, in two places:

| | |
|---|---|
| `~/.cwbcode/` | the backend's own: `cwbcode.db` (projects it has opened, tools the agent has made), `settings.jsonl` (remote mode), compiled tools, `cert.pem` and `key.pem`, `token`, sessions, logs |
| `<project>/.cwbcode/cwbcode.db` | one per project (the folder ignores itself in git): conversations, an append-only log of every event (messages, thinking, tool calls and results, approvals and answers), every version of every file the agent read or changed, the knowledge base with its files and images byte for byte |

A project is any folder under your home folder (also through a link placed in it, such as
`~/vivid -> /Volumes/disk/vivid`). The home folder itself, credentials (`~/.ssh`, `~/Library`,
...) and `~/.cwbcode` are refused.

## How it works

**Model** (`rust/server/src/inkling`, `llm.rs`). Inkling 2 Mini is a 26B-parameter
routed-expert model with 5B active per token. The MLX port and its draft network come from
CausewaybayMaker: the draft network proposes up to seven tokens, the
model checks them in one pass and keeps the run it agrees with. Measured on an M5 Max: 60 to
110 tokens/s while writing; a 15k-token prompt is read in about 12 s. The conversation lives
in the model's KV cache, so each step reads only what is new.

**One model thread** (`worker.rs`). MLX state is per thread and the model is 20 GB, so
everything that needs it (a chat turn, embeddings, a summary) is a job on one thread, one at
a time. One conversation's cache is in memory; asking for another reads it back from its
store. Everything else (status, lists, transcripts, keyword search, approvals, interrupt) is
answered at once, also while the model works.

**Protocol** (`rust/proto`, `rust/proto/schema/cwbcode.capnp`). `POST /api/call` takes a `Request` and
answers a `Response`; `POST /api/chat` answers a stream of `Event`s, each framed by its
length, with an empty frame as heartbeat. The same messages are used by the terminal client
and, compiled to WebAssembly, by the web client.

**Transport** (`net.rs`, `rust/client/src/net.rs`). The backend listens on a TCP port for
HTTP/2 over TLS (7443) and on the next UDP port for HTTP/3 over QUIC (7444), with one
self-signed certificate. It is made on first start, good for 397 days, for serving only, and
names every address of the machine (localhost, LAN, Tailscale); it is made again, with a new
fingerprint, when the machine has an address it does not name or 30 days before it ends.
(Safari takes the first page from a certificate that breaks Apple's rules, a longer life,
no serving purpose, an address not named, and then refuses everything after it.) HTTP/3 is deliberately not on the HTTPS port
and not advertised: a browser that expects it there tries it, refuses the self-signed
certificate over QUIC, and shows the site as unreachable (Safari did). The terminal client tries HTTP/3 for 1.5 s and
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

- In local mode (no wallet) the backend listens on this machine only and asks for no
  login, but answers only its own page and clients that are not pages: a page of another
  website open in your browser is refused. With a wallet, every request needs a session
  obtained by signing in with it; the backend's own token (`~/.cwbcode/token`, readable
  only by you) is accepted from this machine only. Without credentials nothing beyond a
  small sign-in message is read, and failed sign-ins are limited to ten a minute.
- There are no network tools. Commands and made tools run under `sandbox-exec` with the
  network denied, no hand-off to other programs (`open`, AppleScript), and writes allowed
  only in the project, temporary folders and package caches. `cwbcode serve
  --allow-network` opens the network to commands; with `--no-sandbox` every command needs
  your approval, whatever the mode.
- File tools write only inside the project. Paths are resolved through symlinks first,
  including links to files that do not exist yet, and a write does not follow a link.
- Credentials (`~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.kube`, `~/.docker`, `~/.config/gh`,
  `~/.config/gcloud`, keychains, `~/.netrc`, `~/.npmrc`, ...), the backend's token and key,
  and every `.cwbcode` folder's contents are not readable by the file tools (searching a
  folder above them skips them) or by sandboxed commands.
- Modes: `ask` (default) shows each edit and command and waits for an answer; `auto` does
  not ask; `read-only` changes nothing. An approval nobody answers (a closed tab, an
  interrupt, 15 minutes) counts as no.

What it does **not** do:

- The sandbox starts from "allowed" and denies the above. A command can read the rest of
  your home folder (shell history, other projects, browser profiles) and inherits the
  backend's environment variables; with no network it cannot send them anywhere, but what
  it reads becomes part of the conversation.
- The approval prompt shows the first lines of an edit (8 to 12) or of a new tool (40), not
  all of it.
- The model can be steered by text it reads (a file, a page you paste). The limits above
  are what holds then, not the model's judgement.
- Whoever can sign in is you, as far as the backend is concerned: they can choose `auto`
  mode and any folder under your home folder. With a wallet that means whoever holds its
  recovery phrase.
- Typing a recovery phrase into a web page is only as safe as the page, and keeping it in
  the browser ("remember on this device") only as safe as the browser profile: use it on the
  backend you run yourself, over its own address, and check the certificate fingerprint.
- A browser cannot pin the self-signed certificate: on first visit compare the fingerprint
  it shows with the one `cwbcode url` prints.

## Tests

```sh
make test         # unit tests, and the backend started for real with a scripted model:
                  # HTTP/3 and the HTTP/2 fallback, local mode, wallet sign-in, token,
                  # requests from other sites' pages, certificate pinning, folders
                  # outside home, approvals (answered, abandoned, interrupted), the event
                  # log, file versions and undo, the knowledge base, tools made at runtime
make test-web     # the web client in headless Chrome: sign-in with a recovery phrase (and
                  # wrong ones refused), the remembered phrase, a folder created on opening,
                  # themes, layouts, languages and a refresh that comes back to the same
                  # place, a project, chat, approval, undo, history,
                  # dashboard, knowledge base, sign-out, and local mode without a login,
                  # and remote mode turned on and off from the dashboard
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
