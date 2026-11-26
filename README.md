<p align="center">
  <img src="rust/web/assets/pwa/icon-512.png" width="128" alt="The Causewaybay Code icon: a Causeway Bay tram under billboard screens, in pixel art">
</p>

# Causewaybay Code (CC)

Causewaybay Code, CC for short, is an AI coding agent and general assistant that runs
entirely on your Mac. The model is
Inkling 2 Mini, run natively on Apple MLX in Rust with its draft network for speculative
decoding; nothing is sent to any service. A backend holds the model and runs the agent; you
work with it from a terminal client in any project folder, or from a web client the backend
serves, which can also be installed as an app.

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

## Quick start

You need:

- An Apple Silicon Mac with enough memory: the model takes 20 GB and a session peaks around
  27 GB (developed and tested on a 36 GB M5 Max)
- Xcode (MLX is compiled from source, and its Metal shaders with Xcode's `metal` tool)
- Rust with the `wasm32-unknown-unknown` target, `cmake`, `capnp` and `trunk`
  (`brew install cmake capnp trunk`; `rustup target add wasm32-unknown-unknown`)
- Python 3 for `make start` and the model download; [Ollama](https://ollama.com) is optional
  (the model can come from Ollama or from Hugging Face, no account or key needed for either;
  the Mac app needs neither Python nor Ollama: its backend downloads the model itself)

```sh
git clone https://github.com/leejw51/CausewaybayCode.git
cd CausewaybayCode
make download   # the model (20 GB, once): asks whether from Ollama or Hugging Face
make build      # the first build compiles MLX from source and takes a few minutes
make start      # starts the backend and prints its web address and certificate
```

Open the `open` link `make start` printed (`https://127.0.0.1:7443/#code=...` in local mode:
a one-time code the page exchanges for the backend's token, which it keeps; the link works
once, within five minutes, and `make url` prints a new one), accept the certificate (its
fingerprint is on the `cert` line), and pick a folder under
your home folder to work in. Or, in a project folder, run the terminal client:

```sh
cd ~/code/myapp
~/path/to/CausewaybayCode/rust/bin/cwbcodeclient
```

`make install` puts both programs in `~/bin` instead. `make stop` stops the backend.

## The programs

The programs keep the names `cwbcode` and `cwbcodeclient`: a program called `cc` would take
the place of the C compiler on your PATH.

| Program | |
|---|---|
| `cwbcode` | the **backend**: holds the model, runs the agent and its tools in any folder under your home folder, keeps everything in SQLite |
| `cwbcodeclient` | the **terminal client**: works on the current folder through the backend |
| web | a **dashboard and client** in the browser (Rust, Leptos, WebAssembly; movement by [Motion](https://motion.dev)), served by the backend |

They talk in Cap'n Proto messages over **HTTP/3**, or **HTTP/2** where QUIC is not available,
with a certificate from the machine's own certificate authority, and a token.

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

## Build and run

```sh
make download   # the model (20 GB, once): asks whether from Ollama or Hugging Face
make build      # web client, then both programs in release mode, copied to rust/bin
make install    # the same, and copies cwbcode and cwbcodeclient to ~/bin (prints where)
make package    # the Mac app: a window on the web client, with the backend inside (see below)
make app        # open it
make release    # everything built, in rust/dist: the app zipped, its disk image, SHA-256 sums
make start      # start the backend under supervisor (0.0.0.0 when a wallet is set, else 127.0.0.1)
make stop       # stop it
```

The implementation is in `rust/`; other languages will get folders of their own beside it.
The `make` targets work from the repository root (which passes them on) or from `rust/`.

`make start` prints the mode, the web address, the token where one is needed (in local mode
a one-time link instead) and the certificate's fingerprint; `make url` prints them again
(with a new link), `make status` and `make logs` show
how it is doing. The first build compiles MLX from source and takes a few minutes. The
weights are read where they are, in Ollama's store or in `~/.cwbcode/models`; Ollama does not
need to be running (and if it has the model loaded, stop it first: two copies do not fit).

**The model.** `make start` starts the backend at once; when the model is not there, the
backend downloads it itself (from Hugging Face, with your token when there is one), the web
client shows the progress, and it loads by itself when done. To fetch it beforehand instead
(`rust/scripts/model.py`): `make download` asks whether with Ollama or from Hugging Face;
`make pull` takes Ollama without asking; `make model` prints where it is and what it is.
Either way the progress shows the speed and the time left, and a stopped download picks up
where it left off.

| | from Ollama | from Hugging Face |
|---|---|---|
| needs | Ollama installed (`ollama serve` is started for the download if it is not running) | nothing: [thinkingmachines/Inkling-2-Mini](https://huggingface.co/thinkingmachines/Inkling-2-Mini) is public |
| key | none | none; `$HF_TOKEN`, or the token `hf auth login` saved, is used when there is one (and is sent to huggingface.co only) |
| goes to | `~/.ollama/models` (`$OLLAMA_MODELS`) | `~/.cwbcode/models` (`$CWBCODE_MODELS`), in the same layout |

The Hugging Face checkpoint holds the same bytes as Ollama's; it is written, as it arrives,
one tensor per file, labelled as Ollama labels them, and every file is checked against the
SHA-256 in Ollama's registry. The backend looks in Ollama's store first, then in
`~/.cwbcode/models`.

**The backend downloads it too**, without Python or Ollama (`rust/server/src/download.rs`,
the same Hugging Face recipe in Rust, into the same store and with the same resume file):

- By default a model that is in neither store is downloaded at start and then loaded, without
  a restart: no flag is needed (`make start` still runs `scripts/model.py ensure` first, and
  `--download-model` is still taken, and changes nothing).
- `cwbcode serve --no-download-model` (or `CWBCODE_DOWNLOAD_MODEL=0`) turns that off: a
  missing model is `missing` (in Status, the bar's pill and the dashboard), and the web
  client's chat view and model card offer **Download the model (about 20 GB)**: the call
  (`downloadModel`) needs the sign-in or token, as every call does.
- A Hugging Face token is used when there is one: `$HF_TOKEN`, `$HUGGING_FACE_HUB_TOKEN`, the
  file `$HF_TOKEN_PATH` names, or the one `hf auth login` saved (`$HF_HOME/token`,
  `~/.cache/huggingface/token` by default; the Mac app, started from the Finder without the
  shell's environment, finds that file). It goes to huggingface.co only, never to the CDN it
  redirects to, and downloads are faster with it. Every client says whether one is used and
  where it was found (`env HF_TOKEN`, `~/.cache/huggingface/token`; never the token); without
  one they say so, with a one-line tip (`hf auth login`, then restart). A token Hugging Face
  refuses (401, 403) is dropped, and the download goes on without it, saying so.
- While it runs the model is `downloading`, then `verifying` (the check against Ollama's
  registry), then `loading`. Every client follows it on **`POST /api/events`**: one stream,
  the same messages pushed to all of them from one broadcast (nobody polls), a message on
  every change of state and at least every 500 ms while it downloads, ending once the model is
  ready; asked as any call is (the token or a session, the backend's own page). A
  `Progress` message carries the state, the bytes done and in all, the speed over the last
  five seconds, the tensor files done and in all, and where it comes from (as fields, which
  each client words in its own language); Status carries the same. The web client shows the
  Forge (below) and falls back to asking for Status every second when the stream cannot be
  had; the terminal client draws a one-line bar until the model is ready, and
  `cwbcodeclient status` shows the download too. A download that
  stops (a network error, the app quit) keeps what it finished: starting it again, or
  starting the backend again, goes on from there, and the page offers to try again with
  the reason. The backend's log (`server.log`, or `app-backend.log` for the app) says where
  it comes from, how far it is every 30 s, and what the check against Ollama's registry
  found.
- Disk space is checked first (what is left to fetch, and 2 GB). Large tensors come in
  32 MB pieces over 16 connections.

**Local mode and remote mode.** The backend starts in local mode. Setting the wallet that
owns it turns remote mode on:

| | local mode (no wallet) | remote mode (a wallet is set) |
|---|---|---|
| listens on | `127.0.0.1:7443`: this machine only | `0.0.0.0:7443`: your network and tailnet too |
| login | the backend's token, from this machine only | required: sign in with that wallet |
| web client | opens from the `open` link `make url` prints (a one-time code for the token) | asks for the wallet's recovery phrase |
| terminal client | just works | on this Mac just works; elsewhere `cwbcodeclient login` |

Turn it on from the web client's **Dashboard → Remote access** (type the wallet's address,
or its recovery phrase to have the address worked out in the browser), or from the terminal:

```sh
cwbcodeclient remote                  # which mode, and where the backend listens
cwbcodeclient remote on 0x5290…9EE7   # or `remote on` alone, to type the recovery phrase
cwbcodeclient remote off              # back to this machine only, with its token
```

It takes effect at once, without a restart: the backend moves its listeners to the new
address and starts (or stops) asking for the wallet. The setting is kept in
`~/.cwbcode/settings.jsonl`, one JSON object per line, the last line in force:

```json
{"time":"2026-11-25 23:40:12","set":"wallet","value":"0x52908400098527886E0F7030069857D2E4169EE7"}
{"time":"2026-11-26 09:02:51","set":"wallet","value":""}
```

Turning it on needs the backend's token (in local mode, whoever holds it on this machine); turning it off
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

The phrase is never stored: it controls the whole wallet, not only this backend. With **stay
signed in on this device** ticked (it is by default), the session is kept in the browser's
storage, so a refresh comes straight back in; when it ends (sessions last 30 days) the page
asks for the phrase again. **Sign out** ends it. Untick the box on a device that is not yours
alone. A phrase that earlier versions kept is deleted when the page loads.

Use a wallet made for this, with nothing in it: whoever has its phrase has this backend, and
anything the wallet holds.

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
`/run`, `/clear`, `!command`. Ctrl-C stops the model or a running command; Ctrl-D leaves.

### The web client

Open the address `make start` printed (`https://127.0.0.1:7443` in local mode,
`https://localhost:7443` and the machine's other addresses in remote mode) and accept the
certificate, or trust its authority once (below). In local mode open the `open` link instead:
its one-time code, taken out of the address at once, is exchanged for the backend's token,
which the page keeps. The exchange works only from this machine, once, within five minutes,
so the token itself never stands in an address the browser's history keeps. A link that no
longer works leaves a token the page already has alone. (Or type the token from
`~/.cwbcode/token`, or open `https://127.0.0.1:7443/#token=<token>`.) With a wallet, sign in
with its recovery phrase.

- **Dashboard**: model state, memory, context in use, protocol, projects, remote access.
- **Chat**: open any folder under `~` (one that does not exist is created, with the folders
  above it), talk to the agent, watch its thinking and tool calls,
  approve edits and commands with a click, attach images, undo a turn's changes, switch
  between `ask first`, `auto`, `read-only` and `plan first`.
- **Knowledge**: the project's knowledge base: search, save notes, upload files.
- **Tools**: the tools the agent has made, with their Rust source.

The look follows its sibling Causewaybay Hacker's playground: dense, dark and rounded, with
no picture behind it. A bar across the top holds the views, the backend's state and the
settings; a sidebar holds the projects and the conversations and folds away (the button at
the left of the bar), so the work has the whole window; panels reach the edges.

- **Projects** live under the home folder: the field says `~/` and takes what is typed as a
  path under it. Paths are shown with `~`, never with the account's name. The × on a
  project takes it off the list and nothing else: its folder, files and records stay, and
  opening it again brings its conversations back.
- **Run, Console, Clear.** Run runs the project's code without the model: the command typed
  in the console, or one guessed from the folder (`cargo run`, `make run`, `go run .`,
  `npm start`, or the source file changed last, compiled first where it must be), in the
  project and in the sandbox, for at most two minutes. Console shows what the runs printed.
  Clear empties the console and starts a new conversation (the old one stays in the list).
  In the terminal: `/run [command]`.
- **A conversation** has a bar showing how full its context is (the backend summarizes by
  itself when it fills; near the end the page says so and offers a new conversation), **Copy
  all** (what was asked, answered and run, as text), **Undo changes** and **Summarize**, and
  beside it an outline: one bar for each thing said or done, which a tap goes to.
- **Settings** (the gear): light / dark, side by side / stacked (the sidebar becomes a
  strip), the **typeface** (fifty-four, from the font packs of Causewaybay Hacker and
  FruitNation Messenger: pixel faces such as VT323, NeoDunggeunmo, Galmuri, IBM VGA and
  Commodore 64; code faces such as JetBrains Mono, Fira Code, Cascadia Code and Iosevka; text
  faces such as Atkinson Hyperlegible, Geist, Nanum Gothic and Noto Sans CJK; or the
  system's own; the whole page is set in it), the **size** (A- and A+, four steps; it is the
  conversation's alone: what is said, its code and what you type, not the interface), and the
  **language**: English,
  한국어, Čeština, 日本語, 廣東話, 中文, Español, Türkçe (the interface; what the backend
  and the model say arrives as it is).

**On the home screen.** The page can be installed (Safari: Share, then Add to Home Screen;
Chrome: Install): it has a manifest, icons and a service worker, and then opens in a window
of its own. The service worker asks the backend first, every time, so nothing is served
stale; it keeps a copy only for when the backend cannot be reached, so that the app still
opens and says it is offline. A browser registers a service worker only where it trusts the
certificate, so trust the backend's certificate authority once on each device first. (An iPad
that showed a letter instead of the icon on its home screen probably did so for the same
reason; that is not yet confirmed.)

**Trusting the backend on a device.** The backend makes its own certificate authority on
first start (`~/.cwbcode/ca.pem`, its key `ca-key.pem` readable only by you) and signs its
certificate with it. Install that authority once on a phone, tablet or another computer and
every certificate the backend makes after is trusted there: no warning, and the page can be
installed as an app. The dashboard (**Trust this backend on this device**) and the sign-in page link to
it at `/ca.crt`, and `make url` prints where. Beside the download the page shows the
authority's SHA-256: before turning trust on, check that it is the one on the `ca` line
`cwbcode url` prints on the backend's machine (the device shows it too, in the certificate's
details); a page that is not your backend's could offer another. On an iPhone or iPad: open the download in
Safari, then Settings → Profile Downloaded → Install, then Settings → General → About →
Certificate Trust Settings → turn it on. On a Mac: open it in Keychain Access and set it to
Always Trust. The authority is limited, with critical name constraints, to this machine's
names and addresses: `localhost`, names under `.local` and `.ts.net`, the machine's host
name, the loopback addresses, and the private (10/8, 172.16/12, 192.168/16) and Tailscale
(100.64/10) ranges. A device that trusts it trusts it for nothing else, so even whoever had
`ca-key.pem` could not use it to pass for another website; the key still never leaves the
machine, and the sandbox keeps commands from reading it. (An address outside those ranges, a
public one, is left out of the certificate.) An authority made by an earlier version, without
these limits, is replaced once when the backend starts (it says so); trust the new one on each
device then, and remove the old one. Install it only on your own devices.

These, and where you are (view, project, conversation), are kept in the browser, so a
refresh comes back to the same place. The typefaces and the pictures are in the program:
the page loads nothing from the internet.

Movement is Motion's: what travels (a view arriving, a card appearing, a number changing)
eases in and out exponentially; what answers a touch (a pressed button, the selection
sliding to the chosen view) follows a spring, as on iOS. It is switched off for "reduce
motion".

### The Mac app

`make package` builds **Causewaybay Code.app** (Tauri, `rust/desktop`), at
`rust/desktop/target/release/bundle/macos/`; `make app` opens it, or drag it to
/Applications. It is a window on the web client with the backend inside it, run as
Causewaybay Hacker's app runs its server: the app shows a starting page, always starts the
backend it carries, and stops it again on quit. That backend listens on this machine only
(127.0.0.1: 7481 for the window, 7453 for HTTPS, 7454 for HTTP/3; the next free ports when
those are taken), never on other interfaces: remote mode is for `make start`, which is for
development and runs beside the app on its own ports. (Both use `~/.cwbcode`; a `make
start` backend with the model loaded beside the app's would be two copies of it in memory.)
Then the window opens the backend's page with its token, as the `open` link does.

The model is not inside the app, and need not be downloaded first: on a Mac that has never
had it, the backend the app starts downloads it by itself (as every backend does; about 20 GB
from Hugging Face, no account, no Python, no Ollama), the window shows the download's
progress where a new user lands, and the model loads when it is done. Quitting the app part
way keeps what is done; opening it again goes on from there. A model already in Ollama's
store or `~/.cwbcode/models` (`make download`) is used as it is.

- The window reaches the backend over plain HTTP on `127.0.0.1:7481` (`cwbcode serve
  --http-port`), because a web view will not take the backend's certificate. That port
  answers this machine only, only for requests addressed to `127.0.0.1` or `localhost` at
  that port (so a site that points its name at 127.0.0.1 cannot reach it), and asks for the
  token like the HTTPS one; the page gets no commands of the app's. If the port is taken,
  the backend says so in its log and serves on without it.
- Whatever answers on that port must first show that it holds the backend's token: the app
  sends a fresh nonce to `/app-proof` and checks the answer (an HMAC of it keyed with the
  token in `~/.cwbcode/token`, bound to the port). Another program there gets no token; the
  starting page says what is on the port (also an older backend, which has no `/app-proof`:
  restart it from this version).
- A backend the app starts goes with it: on quit, on a terminate signal, and also when the
  app is killed or crashes (`--exit-with-stdin`: the app holds its standard input). If the
  backend stops while the window is open, the window goes back to the starting page with the
  end of its log; one started with `make start` is waited for, and the page comes back with
  it. One app at a time (`~/.cwbcode/app.lock`).
- MLX's Metal library (`mlx.metallib`) goes in the app's `Contents/Resources`, and the
  backend tells MLX to load it from there.
- Opened from the Finder (or with only the system's PATH), the app takes the PATH of an
  interactive login shell (`printenv PATH`, so fish works too), so the agent's commands find
  cargo and Homebrew's tools as in a terminal. A shell that takes more than 3 s is stopped,
  and the backend gets the PATH the app has.
- A backend it starts writes to `~/.cwbcode/app-backend.log`, and reads a wallet from
  `~/.cwbcode/app.env` if there is one (`CWBCODE_WALLET=0x...`, remote mode).
  `CWBCODE_APP_PORT` and `CWBCODE_APP_ARGS` change its port and add arguments; `make app
  APP_PORT=...` passes the port on.
- `make package-dmg` also makes a disk image. `make release` puts in `rust/dist` the app on a
  disk image and zipped, `cwbcode-<version>-macos-arm64.zip` (cwbcode and cwbcodeclient on
  their own, with MLX's Metal library beside them), and their SHA-256 sums.
- Signing, as in FruitNationMessenger (`rust/scripts/codesign-binary.sh`, `sign-dmg.sh`):
  with a Developer ID (`APPLE_SIGNING_IDENTITY`, or the one in your keychain) the app, the
  programs in it, the standalone programs and the disk image are signed with the hardened
  runtime and a timestamp. With `APPLE_ID`, `APPLE_PASSWORD` and `APPLE_TEAM_ID` too (or
  `APPLE_NOTARY_PROFILE`), Apple notarizes them, and the app and the disk image get their
  tickets stapled, so they open like any other app once downloaded. Without a Developer ID
  everything is signed ad hoc: the first time, open the app with Control-click → Open.
- The **Release (macOS app)** workflow (`.github/workflows/release-macos.yml`, by hand or on a
  `v*` tag) runs `make release` on GitHub with the repository's signing secrets (named in the
  workflow) and keeps `rust/dist` as an artifact.

## Plan first, your own commands, hooks

**Plan first.** In the `plan` mode (the web client's mode menu, `/mode plan` in the terminal)
the agent may read and search but not write or run: it looks into the request and answers
with a numbered plan. Switch the mode to have it carried out.

**Your own commands.** A message `/name the rest` is sent as the text of `commands/name.md`,
looked for in the project's `.cwbcode` folder and then in `~/.cwbcode`. `$ARGUMENTS` in the
text stands for the rest of the message. For example `~/.cwbcode/commands/review.md`:

```
Review $ARGUMENTS for bugs and risky edge cases. List them, most serious first.
```

and then `/review src/fetch.rs`, in the web client or the terminal.

**Hooks.** Commands of yours that run around the agent's tool calls, in `~/.cwbcode/hooks.json`:

```json
{"before": [{"tool": "bash", "run": "if grep -q 'git push'; then echo 'no pushing from here'; exit 1; fi"}],
 "after":  [{"tool": "edit_file|write_file", "run": "cargo fmt 2>&1 | tail -5"}]}
```

`tool` is a tool's name, several joined with `|`, or `*`. A hook gets the call as JSON on its
input (`{"tool", "args"}`, and `"output"` afterwards) and runs as the agent's own commands
do: in the project, in the sandbox, for at most a minute. A `before` hook that fails refuses
the call, and what it printed is the reason the model is given. What an `after` hook prints
is added to the tool's result, so a formatter's or a linter's words reach the model; what it
changes in the project is versioned and undone like an edit.

The agent can neither read nor write these files: `.cwbcode` folders are closed to it.

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
length, with an empty frame as heartbeat; `POST /api/events` answers a stream of `Progress`
messages (the model's state and its download), framed the same way. The same messages are used by the terminal client
and, compiled to WebAssembly, by the web client.

**Transport** (`net.rs`, `rust/client/src/net.rs`). The backend listens on a TCP port for
HTTP/2 over TLS (7443) and on the next UDP port for HTTP/3 over QUIC (7444), with one
certificate, signed by the machine's own certificate authority (good for ten years, allowed
to sign server certificates only, for this machine's names and addresses only, and kept). The
certificate is made on first start, good for 397 days, for serving only, and names every
address of the machine the authority may sign for (localhost, LAN, Tailscale); it is made
again, with a new fingerprint but the same authority, when the machine has an address it does
not name or 30 days before it ends. Only the backend makes it, when it starts: `cwbcode url`
reads the files and says when the next start will make a new one. (A backend from before there
was an authority makes a new certificate once, so its fingerprint changes once.) An authority
that is there but cannot be used (its key or `ca.json` missing, or not its own) stops the
backend with a message rather than being replaced, since devices trust it; its files are
written whole (to a temporary file, then moved into place).
(Safari takes the first page from a certificate that breaks Apple's rules, a longer life,
no serving purpose, an address not named, and then refuses everything after it.) HTTP/3 is deliberately not on the HTTPS port
and not advertised: a browser that expects it there tries it, refuses a certificate it does
not trust over QUIC, and shows the site as unreachable (Safari did). The terminal client tries HTTP/3 for 1.5 s and
falls back to HTTP/2; it trusts exactly the backend's certificate (pinned), whatever name
the backend is reached by. A browser decides for itself, and uses QUIC only with a
certificate it trusts, so **the web client mostly runs over HTTP/2** (the dashboard shows which).

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

- In local mode (no wallet) the backend listens on this machine only and asks for its token
  (`~/.cwbcode/token`, readable only by you), from this machine only: another user of the
  Mac, a program of someone else's, or anything behind a port forward (`ssh -L`, `tailscale
  serve`) gets nothing without it. It answers only its own page and clients that are not
  pages: a page of another website open in your browser is refused. With a wallet, every request needs a session
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
- Typing a recovery phrase into a web page is only as safe as the page: use it on the
  backend you run yourself, over its own address, and check the certificate fingerprint. The
  page does not store the phrase; it keeps only the session. Use a wallet with nothing in it.
- A browser cannot pin the certificate: on first visit compare the fingerprint it shows with
  the one `cwbcode url` prints, or trust the backend's authority on the device, comparing
  its fingerprint (the `ca` line) once.

## Tests

```sh
make test         # unit tests, and the backend started for real with a scripted model:
                  # HTTP/3 and the HTTP/2 fallback, local mode, wallet sign-in, token,
                  # requests from other sites' pages, certificate pinning, folders
                  # outside home, approvals (answered, abandoned, interrupted), the event
                  # log, file versions and undo, the knowledge base, tools made at runtime,
                  # plan mode, hooks, the user's own commands, taking a project off the list,
                  # and the model's download from a fake Hugging Face (progress, a stop
                  # and a resume, every file Ollama's, the token never sent to the CDN)
make test-web     # the web client in headless Chrome: sign-in with a recovery phrase (and
                  # wrong ones refused), that the phrase is never stored, a folder created on opening,
                  # themes, layouts, typefaces, sizes, languages and a refresh that comes back to the same
                  # place, a project, chat, approval, undo, history,
                  # dashboard, knowledge base, sign-out, and local mode with the one-time link,
                  # and remote mode turned on and off from the dashboard, and a backend
                  # without the model: the download button, and the retry
make test-playwright   # Playwright drives the web client with the real model: the agent
                  # writes a Rust crate and extends it; the crate must build, pass its own
                  # tests and pass tests it has never seen (needs Node and Google Chrome)
                  # and, in a second test, the web client's own features: projects under
                  # ~/, the conversation's typeface, the context bar, copying, plan mode,
                  # your own commands and hooks
make test-model   # semantic search with the model: paraphrases, Korean against English;
                  # and the download's plan against the real Hugging Face and Ollama's
                  # registry (all 1250 files, a few MB read)
make e2e          # real tasks with the real model through the backend and the client
```

`make test` needs no model: the backend is started with `--script`, a JSON file of the
model's turns. `make test-web` needs Google Chrome in `/Applications` and the Python package
`websockets`; `make test-playwright` needs Node and Google Chrome, and like `make test-model`
and `make e2e` the model, with the backend stopped (two copies do not fit).

## Limits

- One model: Inkling 2 Mini, from Ollama's store or downloaded from Hugging Face. macOS on Apple Silicon only.
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
- Of what Claude Code has, these are not here: web search and fetch (see above), sub-agents
  (there is one model and one conversation in memory at a time), MCP servers, and
  background tasks.

## License

Apache License 2.0, see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). `rust/server/src/inkling`
is derived from Ollama's MLX runner (MIT License, kept with its notice), and the web client
ships Motion and typefaces under their own licences (mostly the SIL Open Font License); see
[`THIRD_PARTY_NOTICES`](THIRD_PARTY_NOTICES). The pictures were
made for this project with an image model.

Contributions are welcome: see [`CONTRIBUTING.md`](CONTRIBUTING.md). To report a security
problem, see [`SECURITY.md`](SECURITY.md).
