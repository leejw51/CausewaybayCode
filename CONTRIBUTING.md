# Contributing

Thank you for looking. Issues and pull requests are welcome; for a larger change, open an
issue first to say what you have in mind.

## Building

You need what the README's [Quick start](README.md#quick-start) lists: an Apple Silicon Mac,
Xcode, Rust with the `wasm32-unknown-unknown` target, `cmake`, `capnp`, `trunk` and Python 3.
The model (20 GB) is needed only to run the agent for real, not to build or to run `make test`.

```sh
make build      # the web client (trunk), then cwbcode and cwbcodeclient in release mode, into rust/bin
make help       # every target
```

The `make` targets work from the repository root or from `rust/`. The code is in `rust/`:

| | |
|---|---|
| `rust/proto` | the Cap'n Proto messages (`schema/cwbcode.capnp`) the three programs exchange |
| `rust/server` | `cwbcode`, the backend: the model (`src/inkling`, `llm.rs`, `worker.rs`), the agent (`agent.rs`, `prompt.rs`), its tools (`tools.rs`, `dyntool.rs`), the sandbox (`sandbox.rs`, `projects.rs`), the API and sign-in (`api.rs`, `auth.rs`, `control.rs`), transport (`net.rs`), SQLite (`store.rs`, `kb.rs`) |
| `rust/client` | `cwbcodeclient`, the terminal client; `tests/backend.rs` starts real backends |
| `rust/web` | the web client (Leptos, WebAssembly), built by `trunk` into `rust/web/dist` and embedded in the backend |
| `rust/scripts` | the model download (`model.py`), supervisor's configuration, `web_test.py`, `e2e.sh` |
| `design/` | the design canvas and the pictures drawn for it |

The backend embeds `rust/web/dist` when it is built. Without it, it still builds, with a page
that says to run `make build`; build the web client before testing anything in the browser.

## Testing

```sh
make test       # unit tests, and real backends driven by a scripted model: no model needed
make test-web   # the web client in headless Chrome (needs Google Chrome and the Python package `websockets`)
```

Run both before sending a pull request that touches the backend, the protocol or the web
client. `make test` runs the backend integration tests one at a time on purpose: they time
the 1.5 s the client gives HTTP/3 before falling back to HTTP/2.

The others need the model and the memory for it, so stop your own backend first:
`make test-model` (semantic search), `make e2e` (real tasks through the backend and the
client) and `make test-playwright` (the agent writes a Rust crate through the web client;
needs Node and Google Chrome). Say in the pull request which of them you ran.

A test that needs the model is marked `#[ignore]`, so that `make test` stays free of it.
Integration tests give the backend a script of the model's turns with `--script` (see
`rust/client/tests/backend.rs`), a scratch home folder, and `--env-file /dev/null`, so that
no `.env` of yours decides how they behave.

## Style

Follow what is there:

- **Comments and documentation** are plain, full sentences: what a thing is or why it is so,
  not how clever it is. The README is written the same way; keep its voice when you change it.
- **Test names are sentences** that state the behaviour, such as
  `a_turn_asks_runs_tools_logs_everything_and_can_be_undone`, and a test checks the whole
  behaviour through the real backend where it can.
- **Formatting**: there is no rustfmt configuration, and the code is not kept rustfmt-clean.
  Match the file you are in rather than reformatting it, so that a diff shows only the change.
- **Safety first**: a change to the tools, the sandbox, paths, sign-in or what the backend
  answers to whom needs a test that shows the limit holding (see the tests named after what
  a steered model or another site's page cannot do). The limits are listed under
  [Safety](README.md#how-it-works) in the README; keep that list true.
- **The web client's text** is in `rust/web/src/i18n.rs`, in all eight languages. A new key
  needs all eight; its test (`cd rust/web && cargo test`) checks that each key appears once
  and has every language.
- **Nothing from the network at run time**: the web client loads no fonts, scripts or
  pictures from the internet, and the agent has no network tools. Keep it so.
- **Commit messages** are one plain sentence in the imperative that says what changed, as in
  `git log`.

## Third-party material

Code, fonts or pictures from elsewhere need their licence to allow it, the licence text in
the repository, and an entry in [`THIRD_PARTY_NOTICES`](THIRD_PARTY_NOTICES) (fonts also in
`rust/web/assets/fonts/CREDITS.txt`). Rust crates are listed in `Cargo.lock` and need no entry.

By contributing you agree that your contribution is licensed under the
[Apache License 2.0](LICENSE) of this project (as section 5 of that licence says), and that
you will follow the
[Code of Conduct](CODE_OF_CONDUCT.md).
