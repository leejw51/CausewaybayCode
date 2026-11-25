@0xb4f1c9a27d53e611;
# The messages of cwbcode's API. A call is an HTTP POST (HTTP/3, on the UDP port after the
# HTTPS port, or HTTP/2 where QUIC is not available) whose body is one Request:
#
#   POST /api/call   -> one Response
#   POST /api/chat   -> a stream of Events, each framed as a 4-byte little-endian length
#                       followed by that many bytes of message
#
# Unless the server is in local mode (Status.auth = open), every request but hello,
# challenge and login carries a session token (or the server's own token) in the
# Authorization header: `Bearer <token>`.

enum Method {
  status @0;           # -> status
  projects @1;         # -> projects
  openProject @2;      # text: folder path (made if missing)  -> projects (the one opened), text ("created ...")
  sessions @3;         # project                              -> sessions
  transcript @4;       # project, session                     -> events (everything that happened)
  newSession @5;       # project                              -> id
  nameSession @6;      # project, session, name
  kbList @7;           # project                              -> docs
  kbAdd @8;            # project, name (title), text | files  -> docs (the entries made)
  kbSearch @9;         # project, text (query), limit, flag (keywords only) -> hits
  kbGet @10;           # project, id                          -> docs (one, with text), data (its file)
  kbDelete @11;        # project, id
  tools @12;           # -> tools
  toolDelete @13;      # name
  approve @14;         # id (of the approval), text (y, a, n, or what to do instead)
  interrupt @15;       # stops what the model or a command is doing
  undo @16;            # project, session                     -> text (what was restored)
  compact @17;         # project, session                     -> text
  chat @18;            # project, session (0: a new one), text, files (images), mode, think  (POST /api/chat)
  changes @19;         # project, session                     -> changes (file versions saved)
  # Signing in. These three need no credentials.
  hello @20;           # -> status (version, auth, wallet, fingerprint only)
  challenge @21;       # -> text (a one-time message for the wallet to sign)
  login @22;           # text (the challenge), name (its signature, hex) -> text (a session token)
  logout @23;          # ends the session the request carries
  setup @24;           # text (a wallet address: remote mode on; "": off) -> status, text
  removeProject @25;   # project: taken off the list. The folder and what is in it stay.
  run @26;             # project, text (a command; "": guessed from the folder) -> text (the
                       # command and what it printed). Run in the project, in the sandbox.
}

struct Upload {
  name @0 :Text;
  data @1 :Data;
}

struct Request {
  method @0 :Method;
  project @1 :Text;
  session @2 :Int64;
  id @3 :Int64;
  text @4 :Text;
  name @5 :Text;
  mode @6 :Text;       # ask | auto | read-only ("" keeps the session's)
  think @7 :Text;      # none | minimal | low | medium | high | max ("" keeps it)
  limit @8 :UInt32;
  flag @9 :Bool;
  files @10 :List(Upload);
}

struct Status {
  version @0 :Text;
  model @1 :Text;
  modelState @2 :Text;     # loading | ready | none | failed: ...
  protocol @3 :Text;       # how this request arrived: h3 | h2 | http/1.1
  busy @4 :Bool;
  busyProject @5 :Text;
  contextTokens @6 :UInt64;
  contextLimit @7 :UInt64;
  memoryActive @8 :UInt64; # bytes
  memoryPeak @9 :UInt64;
  uptimeSeconds @10 :UInt64;
  sandbox @11 :Text;
  home @12 :Text;
  h3 @13 :Bool;            # the server listens for HTTP/3
  queued @14 :UInt32;
  auth @15 :Text;          # open (local mode: no login) | wallet (sign in with the wallet) | token
  wallet @16 :Text;        # the wallet address that may sign in ("" without one)
  fingerprint @17 :Text;   # SHA-256 of the server's certificate
  bind @18 :Text;          # the address the server listens on
  userHome @20 :Text;      # the home folder: clients show paths under it as ~/...
  walletSource @19 :Text;  # where the wallet is set: "" (nowhere), "settings" (set up while
                           # running; can be changed with Method.setup), or the .env file /
                           # "the environment" (fixed there)
}

struct Project {
  path @0 :Text;
  name @1 :Text;
  sessions @2 :UInt32;
  updated @3 :Text;
}

struct Session {
  id @0 :Int64;
  name @1 :Text;
  updated @2 :Text;
  messages @3 :UInt32;
  first @4 :Text;
}

struct Doc {
  id @0 :Int64;
  kind @1 :Text;           # text | file | image
  title @2 :Text;
  source @3 :Text;
  text @4 :Text;
  created @5 :Text;
  hasFile @6 :Bool;
}

struct Hit {
  doc @0 :Int64;
  kind @1 :Text;
  title @2 :Text;
  snippet @3 :Text;
  bm25Rank @4 :UInt32;     # 0: not in that ranking
  semanticRank @5 :UInt32;
  score @6 :Float64;
}

struct Tool {
  name @0 :Text;
  description @1 :Text;
  parameters @2 :Text;     # JSON Schema
  source @3 :Text;
}

struct Change {
  id @0 :Int64;
  path @1 :Text;
  action @2 :Text;         # read | create | write | edit
  bytesBefore @3 :Int64;   # -1: the file did not exist
  bytesAfter @4 :Int64;
  created @5 :Text;
  undone @6 :Bool;
}

struct Event {
  # One thing that happened in a conversation: streamed while it happens, and stored (see
  # Method.transcript). `created` is set on stored events.
  created @0 :Text;
  union {
    session @1 :Int64;         # first event of a chat: the conversation it belongs to
    user @2 :Text;             # the user's message
    thinking @3 :Text;         # a piece of the model's thinking (stored: the whole of it)
    text @4 :Text;             # a piece of the answer (stored: the whole message)
    endMessage @5 :Void;       # the thinking or answer message above is complete
    toolStart @6 :ToolStart;
    toolEnd @7 :ToolEnd;
    note @8 :Text;
    warning @9 :Text;
    error @10 :Text;
    approval @11 :Approval;    # the model waits for Method.approve with this id
    usage @12 :Usage;          # end of a turn
    todos @13 :Text;           # the model's task list, as JSON [{content, status}]
    done @14 :Void;            # end of the stream
    answered @15 :Text;        # stored only: what the user answered to an approval
  }
}

struct ToolStart {
  name @0 :Text;
  summary @1 :Text;
  args @2 :Text;               # JSON
}

struct ToolEnd {
  ok @0 :Bool;
  text @1 :Text;
  images @2 :UInt32;
  seconds @3 :Float64;
}

struct Approval {
  id @0 :Int64;
  what @1 :Text;
  offerAlways @2 :Bool;
}

struct Usage {
  contextTokens @0 :UInt64;
  generated @1 :UInt64;
  tokensPerSecond @2 :Float64;
  toolCalls @3 :UInt32;
  seconds @4 :Float64;
  steps @5 :UInt32;
}

struct Response {
  error @0 :Text;              # "" when the call succeeded
  status @1 :Status;
  projects @2 :List(Project);
  sessions @3 :List(Session);
  events @4 :List(Event);
  docs @5 :List(Doc);
  hits @6 :List(Hit);
  tools @7 :List(Tool);
  changes @8 :List(Change);
  id @9 :Int64;
  text @10 :Text;
  data @11 :Data;
}
