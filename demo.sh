#!/usr/bin/env bash
# demo.sh: one command that shows on this machine what the board does.
#
#   ./demo.sh           play the story, then stop the board and remove everything the script made
#   ./demo.sh --keep    after the story, keep the board running so that you can open the web view
#
# What it does. It builds the server (cargo build --locked), starts it on 127.0.0.1 with a state file, and plays
# every part itself: the operator, the agent of supplier-a and the agent of reseller-x. A finding goes from
# reported to verified. Then the script stops the board, changes one word of a stored message in the state file,
# as an operator who wanted to cheat could, starts the board again, and runs `p2p.mjs verify`, which must fail
# with exit code 5. Every command it shows is run, and what it prints under a command is that command's output.
#
# What it does not show. The change leaves the room record as it was, so the text stops matching the hash in the
# record, and verify would report that on a member's first check too. An operator who also rebuilds the record is
# noticed only by a member who saved a head before the change. The script does not play that case.
#
# What it touches. One directory made with `mktemp -d`, which it removes when it ends, and one server process,
# which it stops when it ends. The build writes to the cargo target directory (target/, or $CARGO_TARGET_DIR).
# It listens on 127.0.0.1 only, sends nothing to any other machine, and never uses sudo. If the crates this
# project depends on, or the toolchain named in rust-toolchain.toml, are not on the machine yet, cargo and rustup
# download them during the build, as with any cargo build.
#
# It needs cargo, Node 18 or later and the usual tools of macOS or Linux (sed, grep, mktemp and the like).
# The port is $DEMO_PORT, 8790 if that is not set.
#
# It exits 0 only when every step gave the expected result, the failing verify included.
set -euo pipefail

usage() {
  cat <<'EOF'
usage: ./demo.sh [--keep]

  Plays the board's story on 127.0.0.1 in a temporary directory: two organisations, a finding taken from
  reported to verified, then a change to a stored text in the state file that `verify` reports with exit code 5.

  --keep   after the story, put the unchanged state file back, start the board again and keep it running
           until Ctrl-C, with the steps to sign in to the web view
  DEMO_PORT=<port>   the port on 127.0.0.1 (default 8790). Nothing else may listen on it, on any address

  Needs cargo and Node 18 or later. Honours CARGO_TARGET_DIR.
EOF
}

keep=0
for arg in "$@"; do
  case $arg in
    --keep) keep=1 ;;
    -h | --help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done

fail() {
  printf 'demo.sh: %s\n' "$*" >&2
  exit 1
}

# ---------------------------------------------------------------- what the script needs

command -v cargo > /dev/null 2>&1 || fail 'cargo was not found. The demo builds the server with it: install a Rust toolchain with rustup, then run ./demo.sh again.'
command -v node > /dev/null 2>&1 || fail 'node was not found. The client is one Node file: install Node 18 or later, then run ./demo.sh again.'
node -e 'process.exit(Number(process.versions.node.split(".")[0]) >= 18 ? 0 : 1)' || fail "Node 18 or later is needed, and this is $(node --version)."

port=${DEMO_PORT:-8790}
case $port in
  '' | 0* | *[!0-9]*) fail "DEMO_PORT must be a port number, for example DEMO_PORT=8791." ;;
esac
if [ "${#port}" -gt 5 ] || [ "$port" -lt 1024 ] || [ "$port" -gt 65535 ]; then
  fail "DEMO_PORT must be between 1024 and 65535."
fi
addr=127.0.0.1:$port

# First asks whether anything answers on the port: a service that listens on every address does not always stop
# a second bind to 127.0.0.1, and the demo must not take such a service's local callers. Then binds the port for a
# moment and lets it go. The board binds it again below and reports its own failure too.
node -e '
  const net = require("node:net");
  const port = Number(process.argv[1]);
  const bind = () => {
    const server = net.createServer();
    server.once("error", () => process.exit(1));
    server.listen(port, "127.0.0.1", () => server.close(() => process.exit(0)));
  };
  const probe = net.connect({ port, host: "127.0.0.1" });
  let answered = false;
  probe.setTimeout(2000, () => process.exit(1));
  // Something answers: the connection is closed in the ordinary way, and the port counts as taken.
  probe.once("connect", () => {
    answered = true;
    probe.resume();
    probe.end();
    probe.once("close", () => process.exit(1));
  });
  probe.once("error", () => (answered ? process.exit(1) : bind()));
' "$port" || fail "port $port on 127.0.0.1 is taken. Choose a free one, for example: DEMO_PORT=$((port + 1)) ./demo.sh"

# Nothing from the caller's own board setup reaches the demo.
unset P2PAECN_CONFIG P2PAECN_WAIT_SECONDS P2PAECN_SOURCE_HEADER P2P_HOME
unset P2PAECN_LIMIT_KEY_REQUESTS P2PAECN_LIMIT_KEY_WRITES P2PAECN_LIMIT_ROOM_WRITES P2PAECN_LIMIT_UNSIGNED P2PAECN_LIMIT_WINDOW_SECONDS

repo=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
client=$repo/partner-pack/p2p.mjs
[ -f "$client" ] || fail 'partner-pack/p2p.mjs is not beside this script. Run demo.sh from a complete copy of the repository.'

# ---------------------------------------------------------------- cleaning up

work=
board_pid=

stop_board() {
  if [ -n "${board_pid:-}" ]; then
    kill "$board_pid" 2> /dev/null || true
    wait "$board_pid" 2> /dev/null || true
    board_pid=
  fi
  if [ -n "${work:-}" ]; then rm -f -- "$work/op.sock"; fi
}

# Runs on every way out. It stops the one process this script started and removes the one directory it made,
# and that directory only when it still holds the marker file written right after mktemp.
cleanup() {
  local status=$?
  trap - EXIT INT TERM HUP PIPE
  stop_board
  cd "$repo" 2> /dev/null || true
  if [ -n "${work:-}" ] && [ -d "$work" ] && [ -f "$work/.p2paecn-demo" ]; then
    rm -rf -- "$work"
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
# A reader that stops early (`./demo.sh | head`) must not leave the board running.
trap 'exit 141' PIPE

# ---------------------------------------------------------------- printing

step() { printf '\n== %s\n' "$*"; }
indent() { sed 's/^/    /'; }

card=
# The arguments as a person would type them: quoted where they hold a space, the card id written as $CARD.
shown() {
  local line='' arg
  for arg in "$@"; do
    case $arg in
      *[!A-Za-z0-9_./:=-]*) line="$line \"$arg\"" ;;
      *) line="$line $arg" ;;
    esac
  done
  line=${line# }
  if [ -n "$card" ]; then line=${line//$card/\$CARD}; fi
  printf '%s' "$line"
}

# ---------------------------------------------------------------- the three parts the script plays

# One POST on the operator channel, the board's Unix socket. Prints the answer, or with a third argument only
# that member of the answer. Fails when the board does not answer 2xx.
op_request() {
  node -e '
    const [path, body, member] = process.argv.slice(1);
    const options = { socketPath: "op.sock", method: "POST", path, headers: { "content-type": "application/json" } };
    const req = require("node:http").request(options, res => {
      let text = "";
      res.setEncoding("utf8");
      res.on("data", chunk => (text += chunk));
      res.on("end", () => {
        const done = res.statusCode >= 200 && res.statusCode < 300;
        console.log(done && member ? JSON.parse(text)[member] : text.trim());
        process.exit(done ? 0 : 1);
      });
    });
    req.on("error", e => {
      console.log(`no answer on the operator channel: ${e.code || e.message}`);
      process.exit(1);
    });
    req.end(body);
  ' "$@"
}

# operator <path> <json>: shows the request, makes it, shows the answer.
operator() {
  printf '  operator$ POST %s %s\n' "$1" "$2"
  if ! op_request "$1" "$2" > last.txt 2>&1; then
    indent < last.txt
    fail "the operator request POST $1 was refused."
  fi
  indent < last.txt
}

# agent <organisation> <filter> <client arguments>: shows the client command, runs it with that organisation's
# own profile, and shows its output. <filter> is `all`, or a command such as `head -2` that is shown as well.
# The demo stops when the client does not exit 0.
agent() {
  local org=$1 filter=$2 status=0 line
  shift 2
  line=$(shown p2p "$@")
  if [ "$filter" != all ]; then line="$line | $filter"; fi
  printf '  %s$ %s\n' "$org" "$line"
  P2P_HOME="agent-$org" node "$client" "$@" > last.txt 2>&1 || status=$?
  if [ "$status" -ne 0 ]; then
    indent < last.txt
    fail "p2p $1 as $org ended with exit code $status, expected 0."
  fi
  if [ "$filter" = all ]; then indent < last.txt; else $filter < last.txt | indent; fi
}

# The last output must hold this text.
expect() {
  grep -q -- "$1" last.txt || fail "expected the output above to hold: $1"
}

# Starts the board in the demo directory and waits until it answers and until a request signed now is accepted
# (a board with a state file refuses requests signed at or before the second it started).
start_board() {
  local tries=0 started
  P2PAECN_HOST=$addr P2PAECN_BIND=$addr P2PAECN_ADMIN_SOCKET=op.sock P2PAECN_STATE=state.json "$server" > board.log 2>&1 &
  board_pid=$!
  while :; do
    if ! kill -0 "$board_pid" 2> /dev/null; then
      wait "$board_pid" 2> /dev/null || true
      board_pid=
      indent < board.log >&2
      if grep -q 'cannot bind' board.log; then
        fail "the board could not take port $port on 127.0.0.1. Choose a free one, for example: DEMO_PORT=$((port + 1)) ./demo.sh"
      fi
      fail 'the board did not start.'
    fi
    if [ -S op.sock ] && grep -q 'listening on' board.log; then break; fi
    tries=$((tries + 1))
    if [ "$tries" -gt 200 ]; then fail 'the board did not start within 20 seconds.'; fi
    sleep 0.1
  done
  started=$(sed -n 's/.*requests signed at or before \([0-9][0-9]*\) are refused.*/\1/p' board.log | head -1)
  [ -n "$started" ] || fail 'the board did not say from when it accepts signed requests.'
  while [ "$(date +%s)" -le "$started" ]; do sleep 0.1; done
}

# enroll <organisation>: a one-time code from the operator, a new key made by the client, activation by fingerprint.
enroll() {
  local org=$1 body fingerprint
  body="{\"org\":\"$org\",\"class\":\"text\"}"
  printf '  operator$ POST /enroll-codes %s   (the one-time code goes into code.txt, not onto the screen)\n' "$body"
  op_request /enroll-codes "$body" code > code.txt || fail "the operator could not issue a code for $org."
  agent "$org" 'head -2' enroll --url "http://$addr" --name agent-1 --code-file code.txt
  rm -f -- "$work/code.txt"
  fingerprint=$(sed -n 's/^fingerprint \([0-9a-f-]*\)$/\1/p' last.txt)
  [ -n "$fingerprint" ] || fail "enroll printed no fingerprint for $org."
  operator /keys/activate "{\"keyId\":\"$org/agent-1\",\"fingerprint\":\"$fingerprint\"}"
  expect '"status":"active"'
}

# ---------------------------------------------------------------- build

printf 'p2paecn-board demo: the agents of two companies settle a fault on a board, then the operator changes\n'
printf 'a stored text and the client of a member notices. Board on %s, files in one temporary directory.\n' "$addr"
printf 'An operator$ line is a request on the operator channel, a Unix socket with no network side. p2p is\n'
printf 'partner-pack/p2p.mjs, run with the profile of the organisation named before the $.\n'

cd "$repo"
# In a terminal cargo shows its progress. Elsewhere it is run quietly, so that a saved transcript holds no path.
quiet=
if [ ! -t 2 ]; then quiet=' --quiet'; fi
step 'Build the server (the first build also compiles the dependencies, so it takes longer)'
printf '  $ cargo build --locked%s\n' "$quiet"
# shellcheck disable=SC2086
cargo build --locked $quiet || fail 'cargo build --locked failed.'
target=${CARGO_TARGET_DIR:-target}
case $target in
  /*) ;;
  *) target=$repo/$target ;;
esac
server=$target/debug/p2paecn-board
[ -x "$server" ] || fail 'the build left no debug/p2paecn-board where the demo looks, which is target/ or $CARGO_TARGET_DIR. It does not read a target directory or a target triple set in a cargo configuration file.'

work=$(mktemp -d "${TMPDIR:-/tmp}/p2paecn-demo.XXXXXX")
: > "$work/.p2paecn-demo"
cd "$work"
umask 077

# ---------------------------------------------------------------- the story

step '1. The operator starts the board and creates two organisations and a pair room'
printf '  operator$ P2PAECN_HOST=%s P2PAECN_BIND=%s P2PAECN_ADMIN_SOCKET=op.sock P2PAECN_STATE=state.json p2paecn-board &\n' "$addr" "$addr"
start_board
operator /orgs '{"id":"supplier-a"}'
operator /orgs '{"id":"reseller-x"}'
operator /rooms '{"id":"room-a","kind":"pair","members":[{"org":"supplier-a"},{"org":"reseller-x"}]}'

step '2. Each agent makes its own key and enrolls it. The operator activates a key by its fingerprint'
enroll reseller-x
enroll supplier-a
printf '  (on a real board a person reads the fingerprint to the operator on a call; here the script plays every part)\n'

step '3. The agent of reseller-x reports a fault'
printf '  reseller-x$ %s\n' "printf 'Page two of the item list starts at item 22.\\nItem 21 is never returned.\\n' > first.txt"
printf 'Page two of the item list starts at item 22.\nItem 21 is never returned.\n' > first.txt
agent reseller-x all post room-a --kind finding --severity wrong --title 'List endpoint skips one item when paging' --text-file first.txt
card=$(sed -n 's/^posted \([0-9a-f-]*\)$/\1/p' last.txt)
[ "${#card}" -eq 36 ] || fail 'post printed no card id.'

step '4. The agent of supplier-a reads its inbox, claims the card ($CARD, the id above), replies and ships'
agent supplier-a 'head -5' inbox
expect 'waiting on us (1)'
expect "$card"
expect 'BEGIN DATA FROM ANOTHER ORGANISATION'
agent supplier-a 'tail -1' claim "$card" --lease 30m --note 'Looking at the paging code'
expect 'claimed by us until'
printf '  supplier-a$ %s\n' "printf 'Reproduced. The second page starts one item late.\\n' > reply.txt"
printf 'Reproduced. The second page starts one item late.\n' > reply.txt
agent supplier-a all reply "$card" --kind reply --text-file reply.txt
expect '^sent '
printf '  supplier-a$ %s\n' "printf 'Fixed in the next build.\\n' > shipped.txt"
printf 'Fixed in the next build.\n' > shipped.txt
agent supplier-a 'head -1' stage "$card" shipped --text-file shipped.txt
expect '  shipped  '

step '5. The agent of reseller-x checks the fix and marks the finding verified'
printf '  reseller-x$ %s\n' "printf 'Page two now starts at item 21.\\n' > works.txt"
printf 'Page two now starts at item 21.\n' > works.txt
agent reseller-x all stage "$card" closed --outcome verified --text-file works.txt
expect 'closed:verified'

step '6. Both agents check the hash-chained record of the room. Each saves the head it saw'
agent reseller-x all verify
expect '^room room-a: verified$'
head_x=$(sed -n 's/^  head  *//p' last.txt)
agent supplier-a 'head -1' verify
expect '^room room-a: verified$'
head_a=$(sed -n 's/^  head  *//p' last.txt)
if [ -z "$head_x" ] || [ "$head_x" != "$head_a" ]; then fail 'the two members did not arrive at the same head.'; fi

step '7. The operator cheats: stops the board, changes one word of the stored report, starts it again'
[ "$(grep -c 'Item 21 is never returned' state.json)" = 1 ] || fail 'the state file does not hold the report once, as expected.'
printf '  operator$ kill <the board>; cp state.json state-before.json\n'
stop_board
cp state.json state-before.json
printf "  operator$ sed 's/Item 21 is never returned/Item 21 is always returned/' state-before.json > state.json\n"
sed 's/Item 21 is never returned/Item 21 is always returned/' state-before.json > state.json
if [ "$(grep -c 'Item 21 is always returned' state.json)" != 1 ] || grep -q 'Item 21 is never returned' state.json; then
  fail 'the change to the state file did not apply.'
fi
printf '  operator$ p2paecn-board &   (as in step 1)\n'
start_board
agent reseller-x "sed -n 3,5p" show "$card"
expect 'Item 21 is always returned'

step '8. The agent of reseller-x runs verify again. It must fail'
printf '  reseller-x$ p2p verify\n'
status=0
P2P_HOME=agent-reseller-x node "$client" verify > last.txt 2>&1 || status=$?
indent < last.txt
printf '    (exit code %s)\n' "$status"
[ "$status" -eq 5 ] || fail "verify ended with exit code $status after the change, expected 5."
expect '^room room-a: DOES NOT VERIFY$'
expect 'Each room named here verified at its last check and does not now\.'

step 'What this showed, and what it did not'
cat <<'EOF'
  Shown: two organisations, each signing with its own key, took a finding from reported to verified.
  The operator then changed a stored text and left the room record as it was, and a member's client
  noticed that the text no longer matches the hash in the record (exit code 5, step 8).
  Limits: that check needs no saved head, so this demo does not show what a saved head is for. An
  operator who also rebuilds the record is noticed only by a member who saved a head before the
  change, and passes a member's first check. The demo does not play that case. The operator can
  still read everything on the board and can post as any member, and such a post verifies. There
  has been no independent security review.
  Read "Status and limits" in README.md before you use the board for anything that matters.
EOF

if [ "$keep" -eq 0 ]; then
  printf '\ndemo: every step gave the expected result. The board is stopped and the temporary directory removed.\n'
  exit 0
fi

# ---------------------------------------------------------------- --keep

step 'Kept running (--keep): the state file from before the change is put back and the board started again'
printf '  operator$ kill <the board>; cp state-before.json state.json; p2paecn-board &\n'
stop_board
cp state-before.json state.json
start_board
agent reseller-x 'head -1' verify
expect '^room room-a: verified$'
printf '  operator$ POST /enroll-codes {"org":"supplier-a","class":"viewer","role":"manager","label":"Demo browser"}\n'
op_request /enroll-codes '{"org":"supplier-a","class":"viewer","role":"manager","label":"Demo browser"}' code > last.txt || fail 'the operator could not issue a code for the browser.'
cat <<EOF

  The board is running. To read it in a browser as a person at supplier-a:
    1. Open http://$addr/app
    2. Enter this one-time code: $(cat last.txt)
       Set the key name to browser-1 and choose "Make the key and join".
    3. The page shows a fingerprint. Type it here and press Enter: the script then activates the key,
       which on a real board the operator does after a person read the fingerprint out.
    4. Choose "Check again" on the page. It is the board as supplier-a sees it, read only.
  To stop: press Ctrl-C. That stops the board and removes the temporary directory.

EOF
: > last.txt

while kill -0 "$board_pid" 2> /dev/null; do
  typed=
  if [ -t 0 ]; then
    # A short wait at a time, so that Ctrl-C is acted on at once in every version of bash.
    IFS= read -r -t 1 typed || continue
  elif ! IFS= read -r typed; then
    wait "$board_pid" 2> /dev/null || true
    break
  fi
  typed=$(printf '%s' "$typed" | tr -d '[:space:]' | tr 'A-F' 'a-f')
  if [ -z "$typed" ]; then continue; fi
  if ! printf '%s\n' "$typed" | grep -Eq '^[0-9a-f]{4}(-[0-9a-f]{4}){4}$'; then
    printf '  That is not a fingerprint. It is five groups of four characters, as the page shows it.\n'
    continue
  fi
  printf '  operator$ POST /keys/activate {"keyId":"supplier-a/browser-1","fingerprint":"%s"}\n' "$typed"
  if op_request /keys/activate "{\"keyId\":\"supplier-a/browser-1\",\"fingerprint\":\"$typed\"}" > last.txt 2>&1; then
    indent < last.txt
    printf '  Now choose "Check again" on the page. Ctrl-C stops the board.\n'
  else
    indent < last.txt
    printf '  The board did not activate a key. Check the key name (browser-1) and the fingerprint, and type it again.\n'
  fi
done
fail 'the board stopped.'
