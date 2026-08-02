# AgentRelay - portable verification harness.
#
# One command that builds the daemon, runs its test suite, and then drives the built
# binary through a real overnight-run lifecycle in a throwaway sandbox, asserting on
# the actual output at each step. Ends with a single PASS/FAIL line and a non-zero
# exit code if anything failed, so it doubles as a smoke gate.
#
# Portable by design: it finds the repo from its own location, uses the system temp
# dir for the sandbox, and needs nothing beyond what the project already requires -
# cargo, git, and node. No hardcoded machine paths, no other projects. Runs on
# Windows PowerShell and on PowerShell 7 (pwsh) on Linux/macOS.
#
# Usage (from anywhere):
#   Windows:        powershell -ExecutionPolicy Bypass -File scripts\verify.ps1
#   Linux/macOS:    pwsh scripts/verify.ps1
# Flags: -SkipTests to jump to the lifecycle, -KeepSandbox to inspect it after.

param(
    [switch]$SkipTests,
    [switch]$KeepSandbox
)

$ErrorActionPreference = "Continue"

# On PowerShell 5.1 (Windows) the automatic $IsWindows variable does not exist, so a
# bare reference is $null there - which is exactly "we are on Windows".
$onWindows = -not (Test-Path variable:IsWindows) -or $IsWindows
$exe       = if ($onWindows) { ".exe" } else { "" }

# ~/.cargo/bin is not always on PATH in a fresh shell.
$cargoBin = Join-Path $HOME ".cargo/bin"
if (Test-Path $cargoBin) { $env:Path += [IO.Path]::PathSeparator + $cargoBin }

$root      = Split-Path -Parent $PSScriptRoot
$daemonDir = Join-Path $root "daemon"
$appDir    = Join-Path $root (Join-Path "app" "src-tauri")

$script:pass = 0
$script:fail = 0
$script:failures = @()

function Check($name, $ok, $detail = "") {
    if ($ok) {
        Write-Host "  [PASS] $name" -ForegroundColor Green
        $script:pass++
    } else {
        Write-Host "  [FAIL] $name  $detail" -ForegroundColor Red
        $script:fail++
        $script:failures += $name
    }
}

function Section($t) { Write-Host "`n== $t ==" -ForegroundColor Cyan }

# ---------------------------------------------------------------------------------
Section "Build the daemon"
Push-Location $daemonDir
$build = & cargo build 2>&1 | Out-String
Check "daemon builds" ($build -match "Finished" -and $build -notmatch "(?m)^error")
$agentrelay = Join-Path $daemonDir (Join-Path "target" (Join-Path "debug" "agentrelay$exe"))
Check "agentrelay$exe exists" (Test-Path $agentrelay)
Pop-Location

if (-not $SkipTests) {
    Section "Daemon test suite"
    Push-Location $daemonDir
    $t = & cargo test 2>&1 | Out-String
    $m = [regex]::Match($t, "test result: ok\. (\d+) passed; (\d+) failed")
    $daemonOk = $m.Success -and [int]$m.Groups[2].Value -eq 0
    Check "cargo test green" $daemonOk ("(" + $m.Groups[1].Value + " passed)")
    Pop-Location
}

# ---------------------------------------------------------------------------------
Section "Build the desktop app"
Push-Location $appDir
$appBuild = & cargo build 2>&1 | Out-String
Check "app builds" ($appBuild -match "Finished" -and $appBuild -notmatch "(?m)^error\[")
$appExe = Join-Path $appDir (Join-Path "target" (Join-Path "debug" "agentrelay-app$exe"))
Check "agentrelay-app$exe exists" (Test-Path $appExe)
Pop-Location

# ---------------------------------------------------------------------------------
Section "End-to-end lifecycle (real binary, throwaway sandbox)"

$tmp     = [IO.Path]::GetTempPath()
$sandbox = Join-Path $tmp ("agentrelay-verify-" + [Guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Force $sandbox | Out-Null
Push-Location $sandbox

# A real git repo with a passing check available (node --version), plus the user's
# own uncommitted file, so we can prove the checkpoint never eats it.
& git init -q -b main 2>&1 | Out-Null
Set-Content -Encoding ascii "README.md" "sample project"
& git add -A 2>&1 | Out-Null
& git -c user.email=t@e.com -c user.name=T commit -q -m "base" 2>&1 | Out-Null

# init
& $agentrelay init 2>&1 | Out-Null
Check "init scaffolds the workspace" (Test-Path (Join-Path ".agentrelay" "config.json"))
Check "init writes a self-ignoring .gitignore" (Test-Path (Join-Path ".agentrelay" ".gitignore"))

# doctor - should find git+node (claude optional)
$doctor = & $agentrelay doctor 2>&1 | Out-String
Check "doctor finds git" ($doctor -match "\[OK\] git")
Check "doctor finds node" ($doctor -match "\[OK\] node")

# Configure real validators + a forbid-path guard + a budget.
$cfg = @'
{
  "program": "claude",
  "guards": [ { "kind": "no_push" }, { "kind": "forbid_path", "spec": [".env"] } ],
  "validators": [ { "kind": "tests", "command": "node --version" } ],
  "budget": { "mission_usd": 5.0 }
}
'@
Set-Content -Encoding ascii (Join-Path ".agentrelay" "config.json") $cfg

# status reflects the config, and says the budget out loud
$status = & $agentrelay status 2>&1 | Out-String
Check "status shows a spending cap" ($status -match '\$5\.00')

# validate - runs the real command, exit 0 because node --version passes
& $agentrelay validate --unit u1 2>&1 | Out-Null
Check "validate passes on a real passing check" ($LASTEXITCODE -eq 0)

# The completion gate + checkpoint, guarding the user's own uncommitted work.
Set-Content -Encoding ascii "my-notes.txt" "three hours of my own work"
& $agentrelay begin --unit u1 2>&1 | Out-Null
Set-Content -Encoding ascii "agent-output.txt" "what the agent produced"
$done = & $agentrelay done "unit 1: add agent-output" --unit u1 2>&1 | Out-String
Check "done checkpoints a passing unit" ($done -match "Checkpointed")
$committed = & git show --stat --name-only --format= HEAD 2>&1 | Out-String
Check "the checkpoint contains ONLY the agent's file" (($committed -match "agent-output.txt") -and ($committed -notmatch "my-notes.txt"))
Check "the user's own file was left uncommitted" (Test-Path "my-notes.txt")

# The morning report, and its provenance labels
$report = & $agentrelay report 2>&1 | Out-String
Check "report labels evidence VERIFIED" ($report -match "\[VERIFIED\]")

# Guard hook blocks a real forbidden action (no live agent needed - drive the hook).
# The tool call is handed to the hook on stdin. We write it with the platform's own
# stdin plumbing rather than a PowerShell pipe, which on Windows re-encodes the bytes.
& $agentrelay install-hook 2>&1 | Out-Null
Check "install-hook writes .claude/settings.json" (Test-Path (Join-Path ".claude" "settings.json"))

function Invoke-Hook($json) {
    $f = Join-Path $sandbox ("hook-" + [Guid]::NewGuid().ToString("N").Substring(0, 6) + ".json")
    [IO.File]::WriteAllText($f, $json)
    if ($onWindows) {
        cmd /c "type `"$f`" | `"$agentrelay`" hook --cwd `"$sandbox`"" 2>&1 | Out-Null
    } else {
        & /bin/sh -c "'$agentrelay' hook --cwd '$sandbox' < '$f'" 2>&1 | Out-Null
    }
    return $LASTEXITCODE
}

$denyCode = Invoke-Hook '{"tool_name":"Write","tool_input":{"file_path":".env"}}'
Check "the hook DENIES writing a forbidden path" ($denyCode -eq 2)

$allowCode = Invoke-Hook '{"tool_name":"Write","tool_input":{"file_path":"src/ok.rs"}}'
Check "the hook ALLOWS an in-workspace write" ($allowCode -eq 0)

Pop-Location
if ($KeepSandbox) {
    Write-Host "`nSandbox kept at: $sandbox" -ForegroundColor Yellow
} else {
    Remove-Item -Recurse -Force $sandbox -ErrorAction SilentlyContinue
}

# ---------------------------------------------------------------------------------
Section "Summary"
$summaryColor = if ($script:fail -eq 0) { "Green" } else { "Red" }
Write-Host ("  {0} passed, {1} failed" -f $script:pass, $script:fail) -ForegroundColor $summaryColor
if ($script:fail -gt 0) {
    Write-Host "  Failed checks:" -ForegroundColor Red
    $script:failures | ForEach-Object { Write-Host "    - $_" -ForegroundColor Red }
    exit 1
}
Write-Host "`n  Everything green. AgentRelay is working end to end." -ForegroundColor Green
exit 0
