#!/usr/bin/env bash
# AgentRelay - portable verification harness (Linux/macOS).
#
# Builds the daemon, runs its test suite, then drives the built binary through a real
# overnight-run lifecycle in a throwaway sandbox, asserting on the actual output at
# each step. Ends with a PASS/FAIL line and a non-zero exit if anything failed.
#
# Portable by design: finds the repo from its own location, uses $TMPDIR for the
# sandbox, needs only cargo, git, and node - what the project already requires.
#
# Usage (from anywhere):   bash scripts/verify.sh
# Flags: --skip-tests to jump to the lifecycle, --keep-sandbox to inspect it after.
set -u

skip_tests=0
keep_sandbox=0
for a in "$@"; do
  case "$a" in
    --skip-tests)   skip_tests=1 ;;
    --keep-sandbox) keep_sandbox=1 ;;
  esac
done

# ~/.cargo/bin is not always on PATH in a fresh shell.
[ -d "$HOME/.cargo/bin" ] && PATH="$PATH:$HOME/.cargo/bin"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$script_dir")"
daemon_dir="$root/daemon"
app_dir="$root/app/src-tauri"

GREEN=$'\033[32m'; RED=$'\033[31m'; CYAN=$'\033[36m'; RESET=$'\033[0m'
pass=0; fail=0; failures=()

check() { # name, ok(0/1), detail
  if [ "$2" -eq 1 ]; then
    printf '  %s[PASS]%s %s\n' "$GREEN" "$RESET" "$1"; pass=$((pass+1))
  else
    printf '  %s[FAIL]%s %s  %s\n' "$RED" "$RESET" "$1" "${3:-}"; fail=$((fail+1)); failures+=("$1")
  fi
}
section() { printf '\n%s== %s ==%s\n' "$CYAN" "$1" "$RESET"; }

# ---------------------------------------------------------------------------------
section "Build the daemon"
build="$(cd "$daemon_dir" && cargo build 2>&1)"
echo "$build" | grep -q "Finished" && ! echo "$build" | grep -qE "^error"; check "daemon builds" $((1-$?))
agentrelay="$daemon_dir/target/debug/agentrelay"
[ -x "$agentrelay" ]; check "agentrelay exists" $((1-$?))

if [ "$skip_tests" -eq 0 ]; then
  section "Daemon test suite"
  t="$(cd "$daemon_dir" && cargo test 2>&1)"
  line="$(echo "$t" | grep -oE 'test result: ok\. [0-9]+ passed; [0-9]+ failed' | tail -1)"
  passed="$(echo "$line" | grep -oE '[0-9]+ passed' | grep -oE '[0-9]+')"
  echo "$line" | grep -qE '0 failed' && [ -n "$line" ]; check "cargo test green" $((1-$?)) "(${passed:-0} passed)"
fi

# ---------------------------------------------------------------------------------
section "Build the desktop app"
appbuild="$(cd "$app_dir" && cargo build 2>&1)"
echo "$appbuild" | grep -q "Finished" && ! echo "$appbuild" | grep -qE "^error\["; check "app builds" $((1-$?))
[ -x "$app_dir/target/debug/agentrelay-app" ]; check "agentrelay-app exists" $((1-$?))

# ---------------------------------------------------------------------------------
section "End-to-end lifecycle (real binary, throwaway sandbox)"
sandbox="$(mktemp -d "${TMPDIR:-/tmp}/agentrelay-verify-XXXXXX")"
cd "$sandbox"

git init -q -b main
printf 'sample project' > README.md
git add -A
git -c user.email=t@e.com -c user.name=T commit -q -m base

"$agentrelay" init >/dev/null 2>&1
[ -f .agentrelay/config.json ]; check "init scaffolds the workspace" $((1-$?))
[ -f .agentrelay/.gitignore ]; check "init writes a self-ignoring .gitignore" $((1-$?))

doctor="$("$agentrelay" doctor 2>&1)"
echo "$doctor" | grep -q "\[OK\] git";  check "doctor finds git"  $((1-$?))
echo "$doctor" | grep -q "\[OK\] node"; check "doctor finds node" $((1-$?))

cat > .agentrelay/config.json <<'JSON'
{
  "program": "claude",
  "guards": [ { "kind": "no_push" }, { "kind": "forbid_path", "spec": [".env"] } ],
  "validators": [ { "kind": "tests", "command": "node --version" } ],
  "budget": { "mission_usd": 5.0 }
}
JSON

status="$("$agentrelay" status 2>&1)"
echo "$status" | grep -q '\$5\.00'; check "status shows a spending cap" $((1-$?))

"$agentrelay" validate --unit u1 >/dev/null 2>&1; check "validate passes on a real passing check" $((1-$?))

printf 'three hours of my own work' > my-notes.txt
"$agentrelay" begin --unit u1 >/dev/null 2>&1
printf 'what the agent produced' > agent-output.txt
done_out="$("$agentrelay" done "unit 1: add agent-output" --unit u1 2>&1)"
echo "$done_out" | grep -q "Checkpointed"; check "done checkpoints a passing unit" $((1-$?))
committed="$(git show --stat --name-only --format= HEAD 2>&1)"
{ echo "$committed" | grep -q "agent-output.txt" && ! echo "$committed" | grep -q "my-notes.txt"; }
check "the checkpoint contains ONLY the agent's file" $((1-$?))
[ -f my-notes.txt ]; check "the user's own file was left uncommitted" $((1-$?))

report="$("$agentrelay" report 2>&1)"
echo "$report" | grep -q "\[VERIFIED\]"; check "report labels evidence VERIFIED" $((1-$?))

"$agentrelay" install-hook >/dev/null 2>&1
[ -f .claude/settings.json ]; check "install-hook writes .claude/settings.json" $((1-$?))

printf '{"tool_name":"Write","tool_input":{"file_path":".env"}}' | "$agentrelay" hook --cwd "$sandbox" >/dev/null 2>&1
[ $? -eq 2 ]; check "the hook DENIES writing a forbidden path" $((1-$?))

printf '{"tool_name":"Write","tool_input":{"file_path":"src/ok.rs"}}' | "$agentrelay" hook --cwd "$sandbox" >/dev/null 2>&1
[ $? -eq 0 ]; check "the hook ALLOWS an in-workspace write" $((1-$?))

cd "$root"
if [ "$keep_sandbox" -eq 1 ]; then
  printf '\nSandbox kept at: %s\n' "$sandbox"
else
  rm -rf "$sandbox"
fi

# ---------------------------------------------------------------------------------
section "Summary"
if [ "$fail" -eq 0 ]; then
  printf '  %s%d passed, %d failed%s\n' "$GREEN" "$pass" "$fail" "$RESET"
  printf '\n  %sEverything green. AgentRelay is working end to end.%s\n' "$GREEN" "$RESET"
  exit 0
else
  printf '  %s%d passed, %d failed%s\n' "$RED" "$pass" "$fail" "$RESET"
  printf '  Failed checks:\n'
  for f in "${failures[@]}"; do printf '    - %s\n' "$f"; done
  exit 1
fi
