# Security

## Reporting a problem

Please report a security problem privately, not in a public issue: use **Report a
vulnerability** on the repository's
[Security tab](https://github.com/leejw51/CausewaybayCode/security/advisories/new)
(GitHub's private vulnerability reporting). Say what you found, the steps or a test that
shows it, and the version (the commit) you tried. You will get an answer as soon as the
maintainer can give one; this is a project kept by one person, so there is no fixed time.

Only the latest commit on `main` is looked after; there are no released versions to patch.

## What the backend protects, and what it does not

Causewaybay Code runs an AI agent that reads files and runs commands on your Mac. What it
promises is described in full under [Safety](README.md#how-it-works) in the README; in short:

**Who can reach it.**

- **Local mode** (no wallet set, the default): the backend listens on `127.0.0.1:7443` and
  asks for its token (`~/.cwbcode/token`, readable only by you), from this machine only. The
  `open` link `cwbcode url` (or `make url`) prints carries a one-time code, not the token: the
  page exchanges it for the token, from this machine only, once, within five minutes, so the
  token never stands in an address the browser's history keeps. It answers only its own page and
  clients that are not web pages: a page of another website open in your browser is refused.
- **Remote mode** (a wallet set, with `CWBCODE_WALLET` in `.env` or the environment, from
  the dashboard, or with `cwbcodeclient remote on`): the backend listens on `0.0.0.0:7443`,
  and every request needs a session obtained by signing a one-time challenge with that
  wallet (EIP-191, naming the wallet and the backend's certificate, good for five minutes,
  once). The recovery phrase is used in the browser or the terminal client and never sent.
  The backend's own token (`~/.cwbcode/token`) is accepted from this machine only. Failed
  sign-ins are limited to ten a minute.
- A backend bound beyond this machine without a wallet (`BIND=...`) asks for its token,
  as `cwbcode serve --require-token` always does.
- Traffic is HTTPS (HTTP/2 over TLS, or HTTP/3 over QUIC) with a certificate signed by the
  machine's own certificate authority (`~/.cwbcode/ca.pem`; its key `ca-key.pem` is
  readable only by you and denied to commands). The authority may sign only for this
  machine's names and addresses (`localhost`, names under `.local` and `.ts.net`, the host
  name, loopback, private and Tailscale ranges): a device that trusts it does not trust it for
  any other site, even if its key leaked. The terminal client pins the certificate; a browser
  cannot, so compare the fingerprint it shows with the one `cwbcode url` prints, or install
  the authority on the device once (compare its `ca` fingerprint, which the page shows beside
  the download, with the `ca` line of `cwbcode url`) and trust it.

**What the agent can do.**

- There are no network tools. Commands and tools the agent makes run under the macOS
  sandbox (`sandbox-exec`) with the network denied, no hand-off to other programs, and
  writes allowed only in the project, temporary folders and package caches.
  `cwbcode serve --allow-network` opens the network to commands; with `--no-sandbox` every
  command asks first, whatever the mode.
- File tools write only inside the project, with paths resolved through symlinks first.
- Credentials (`~/.ssh`, `~/.aws`, `~/.gnupg`, keychains, `~/.netrc` and others), the
  backend's token and key, and every `.cwbcode` folder are not readable by the file tools
  or by sandboxed commands.
- By default (`ask` mode) every edit and command is shown and waits for your answer.

**What it does not protect against.** The sandbox starts from "allowed" and denies the
above: a command can read the rest of your home folder and the backend's environment, and
what it reads becomes part of the conversation. The model can be steered by text it reads.
Whoever can sign in is you as far as the backend is concerned; in remote mode, that is
whoever holds the wallet's recovery phrase. The web client never stores the phrase, only the
session it signs in to; use a wallet made for this, with nothing in it.

A report is most useful when it shows one of the promises above broken: a tool or a command
reaching the network, writing outside the project, reading a credential or a `.cwbcode`
folder; a request answered without the sign-in it needs; a page of another site driving the
backend; text that a file or tool output can use to pose as the system or the user. That the
model can be talked into running a command you then approve is expected, not a vulnerability.

## Secrets in this repository

`.env` is git-ignored, and only a wallet's public address belongs in it: the backend refuses
a private key or a recovery phrase there. See [`.env.example`](.env.example).
