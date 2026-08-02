# AgentRelay - build a ready-to-open demo workspace.
#
# Creates a real AgentRelay workspace on your Desktop with a small sample project and
# a sensible config already in place, so you can open the desktop app (or run the
# CLI) against something real without setting anything up. It is deliberately a full,
# valid workspace - not a mock - so what you see is what a real run would show.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\make-sandbox.ps1
# Then: open the AgentRelay desktop app -> Choose workspace -> the folder it prints.

$ErrorActionPreference = "Stop"
$env:Path += ";$env:USERPROFILE\.cargo\bin"

$root      = Split-Path -Parent $PSScriptRoot
$agentrelay = Join-Path $root "daemon\target\debug\agentrelay.exe"
if (-not (Test-Path $agentrelay)) {
    Write-Host "The daemon is not built yet. Run this first:" -ForegroundColor Yellow
    Write-Host "  cd $root\daemon; cargo build" -ForegroundColor Yellow
    exit 1
}

$dest = Join-Path ([Environment]::GetFolderPath("Desktop")) "agentrelay-demo"
if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
New-Item -ItemType Directory -Force $dest | Out-Null
Push-Location $dest

# A minimal but real Node project with a test that passes, so `npm test` is a genuine
# validator the daemon can gate on.
New-Item -ItemType Directory -Force "src" | Out-Null
Set-Content -Encoding ascii "package.json" '{ "name": "demo", "version": "1.0.0", "scripts": { "test": "node --test" } }'
Set-Content -Encoding ascii "src\math.js" "export function add(a, b) { return a + b }"
Set-Content -Encoding ascii "src\math.test.js" @'
import { test } from 'node:test'
import assert from 'node:assert'
import { add } from './math.js'
test('add', () => { assert.equal(add(2, 2), 4) })
'@

& git init -q -b main
& git add -A
& git -c user.email=you@example.com -c user.name=You commit -q -m "sample project"

# Scaffold the workspace, then drop in a config that shows off the safety features:
# a guard, a real validator, a spending cap, a stated (unchecked-by-default) scope.
& $agentrelay init | Out-Null
Set-Content -Encoding ascii ".agentrelay\config.json" @'
{
  "program": "claude",
  "guards": [
    { "kind": "no_push" },
    { "kind": "no_deploy" },
    { "kind": "workspace_boundary" },
    { "kind": "forbid_path", "spec": [".env", "**/*.pem"] }
  ],
  "validators": [
    { "kind": "tests", "command": "npm test" }
  ],
  "budget": { "mission_usd": 5.0, "turn_usd": 1.0 },
  "scope": { "touches": ["src/**"] }
}
'@

Pop-Location

Write-Host ""
Write-Host "Demo workspace ready:" -ForegroundColor Green
Write-Host "  $dest"
Write-Host ""
Write-Host "Try it three ways:" -ForegroundColor Cyan
Write-Host "  1. Desktop app:  open the app, Choose workspace -> the folder above, Start daemon."
Write-Host "  2. CLI status:   cd `"$dest`"; & `"$agentrelay`" status"
Write-Host "  3. Guard proof:  it will refuse to write .env - see scripts\verify-all.ps1"
Write-Host ""
Write-Host "Note: a real overnight run also needs Claude installed" -ForegroundColor Yellow
Write-Host "  (npm install -g @anthropic-ai/claude-code). Everything else works without it."
