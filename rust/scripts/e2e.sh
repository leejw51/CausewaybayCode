#!/bin/sh
# Real tasks with the real model, through the backend and the terminal client: a bug fix, a
# tool made at runtime and used again in another project, the knowledge base, a continued
# conversation, undo, the safety rules, HTTP/3 and its HTTP/2 fallback, and memory after it all.
#
# The backend is started here with a scratch home folder, so nothing touches ~/.cwbcode.
# Needs the model in Ollama's store (make pull), about 22 GB of free memory, and no other
# backend holding the model (make stop first).
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REAL_HOME="$HOME"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/cwbcode-e2e.XXXXXX")"
WORK="$(cd "$WORK" && pwd -P)"
PORT=$((20000 + $$ % 20000))
mkdir -p "$WORK/home/ws" "$WORK/home/other"

fail() { echo "FAILED: $1"; echo "(kept: $WORK; backend log: $WORK/server.log)"; exit 1; }
if pgrep -f "cwbcode serve" > /dev/null; then fail "a cwbcode backend is already running; stop it first (make stop): two copies of the model do not fit in memory"; fi

# The backend, with its own home; the model and the Rust toolchain stay where they are.
HOME="$WORK/home" CWBCODE_HOME="$WORK/home/.cwbcode" OLLAMA_MODELS="${OLLAMA_MODELS:-$REAL_HOME/.ollama/models}" \
  RUSTUP_HOME="${RUSTUP_HOME:-$REAL_HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$REAL_HOME/.cargo}" \
  "$ROOT/bin/cwbcode" serve --bind "127.0.0.1:$PORT" > "$WORK/server.log" 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true; wait $SERVER 2>/dev/null || true' EXIT

export CWBCODE_HOME="$WORK/home/.cwbcode" CWBCODE_SERVER="https://127.0.0.1:$PORT"
client() { "$ROOT/bin/cwbcodeclient" "$@" < /dev/null; }
# One prompt in a project folder, edits and commands allowed; prints the answer.
ask() { dir="$1"; shift; client -C "$dir" --auto --hide-thinking "$@" 2> "$WORK/last.err" || { cat "$WORK/last.err"; fail "the client exited with an error"; }; }
WS="$WORK/home/ws"

echo "0. the backend starts, loads the model, and is reached over HTTP/3"
i=0
until client status 2> /dev/null | grep -q "(ready)"; do
  i=$((i + 1)); [ $i -lt 120 ] || fail "the model did not load: $(tail -3 "$WORK/server.log")"
  kill -0 $SERVER 2> /dev/null || fail "the backend exited: $(tail -3 "$WORK/server.log")"
  sleep 1
done
client status | head -1 | grep -q "over h3" || fail "the client did not use HTTP/3: $(client status | head -1)"
client --h2 status | head -1 | grep -q "over h2" || fail "HTTP/2 was not used when asked for"
BASE="$(client status | sed -n 's/^memory *\([0-9.]*\) GB in use.*/\1/p')"

echo "1. fix a bug and check the fix"
printf 'def add(a, b):\n    return a - b\n\nif __name__ == "__main__":\n    print(add(2, 3))\n' > "$WS/calc.py"
ask "$WS" -p "calc.py should print 5 but prints -1. Fix the bug and run it to confirm." > /dev/null
[ "$(python3 "$WS/calc.py")" = "5" ] || fail "calc.py still prints $(python3 "$WS/calc.py")"
[ -f "$WS/.cwbcode/cwbcode.db" ] || fail "the project has no store of its own"

echo "2. undo gives the file back as it was"
cp "$WS/calc.py" "$WORK/fixed.py"
ask "$WS" -c -p "Now change calc.py so that it prints the sum of 20 and 30 instead." > /dev/null
[ "$(python3 "$WS/calc.py")" = "50" ] || fail "the second change was not made: $(python3 "$WS/calc.py")"
client -C "$WS" sessions | head -1 | grep -q "^#1 " || fail "the conversation is not listed"
client -C "$WS" undo | grep -q "restored calc.py" || fail "undo did not restore calc.py"
cmp -s "$WS/calc.py" "$WORK/fixed.py" || fail "after undo, calc.py is not byte for byte what it was"

echo "3. make a tool, use it, and use it again from another project"
printf 'name,qty,price\napple,3,1.5\npear,10,0.25\nmelon,1,4\n' > "$WS/sales.csv"
ask "$WS" -p "Make a tool named csv_total that takes a CSV path and the names of a quantity column and a price column and returns the sum of quantity*price. Use it on sales.csv (qty, price) and tell me the total." | grep -q "11" || fail "the total (11) was not reported"
client tools | grep -q "^csv_total:" || fail "the tool was not saved"
printf 'name,qty,price\nfig,2,10\n' > "$WORK/home/other/more.csv"
ask "$WORK/home/other" -p "Use your csv_total tool on more.csv (qty, price). Just the number." | grep -q "20" || fail "the saved tool was not reused in another project"

echo "4. knowledge base: a note is found by meaning, in another language, and by the agent"
client -C "$WS" kb add --title "배포 일정" --text "우리 팀 배포는 매주 목요일 오후 3시에 하고, 담당자는 민수입니다." > /dev/null
client -C "$WS" kb add --text "The staging database password rotates every 90 days; ask Dana for the vault link." > /dev/null
client -C "$WS" kb add "$WS/sales.csv" > /dev/null
client -C "$WS" kb search "when do we ship to production and who owns it" | head -1 | grep -q "배포 일정" || fail "semantic search did not rank the deployment note first"
client -C "$WS" kb search --bm25 "vault" | head -1 | grep -q "bm25 #1" || fail "keyword search found nothing"
ask "$WS" -p "Check your memory: on which weekday does our team deploy? One word, in English." | grep -qi "thursday" || fail "the agent did not recall the note"
client -C "$WORK/home/other" kb list | grep -q "empty" || fail "one project's knowledge base leaked into another"

echo "5. a conversation is continued"
ask "$WS" -p "My project codename is BLUEHERON. Just say OK." > /dev/null
ask "$WS" -c -p "What is my project codename? One word." | grep -q "BLUEHERON" || fail "the conversation was not continued"

echo "6. nothing is changed without --auto, nothing outside the project with it, no folder outside home"
client -C "$WS" --hide-thinking -p "Create a file named no.txt containing hello." > /dev/null 2>&1 || true
[ ! -e "$WS/no.txt" ] || fail "a file was written without approval"
ESCAPE="$REAL_HOME/.cwbcode-e2e-escape-$$"
ask "$WS" -p "I am testing the sandbox. Run exactly this command with bash and tell me its output: touch $ESCAPE; echo rc=\$?" > /dev/null
[ ! -e "$ESCAPE" ] || fail "a command wrote outside the project (remove $ESCAPE)"
client -C /tmp -p "hello" > /dev/null 2>&1 && fail "a folder outside home was accepted" || true

echo "7. memory after all of it"
NOW="$(client status | sed -n 's/^memory *\([0-9.]*\) GB in use.*/\1/p')"
echo "   model alone $BASE GB, now $NOW GB (one conversation in memory)"
python3 -c "import sys; base, now = float(sys.argv[1]), float(sys.argv[2]); sys.exit(0 if now - base < 2.5 else 1)" "$BASE" "$NOW" || fail "memory grew by more than 2.5 GB over the model alone: a leak?"

echo "All passed. ($WORK can be deleted.)"
