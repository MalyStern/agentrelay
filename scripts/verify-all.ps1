# AgentRelay - full verification harness.
#
# One command that builds everything, runs every test suite, and then drives the
# daemon through a real overnight-run lifecycle in a throwaway sandbox, asserting on
# the actual output at each step. It ends with a single PASS/FAIL summary and a
# non-zero exit code if anything failed, so it works as a smoke gate.
#
# Why a real lifecycle and not just unit tests: the units are covered by `cargo test`.
# What this adds is the wiring - that the built binary, run as a user runs it, does
# the right thing end to end. That is exactly the seam unit tests cannot see.
#
# Usage (from anywhere):
#   powershell -ExecutionPolicy Bypass -File scripts\verify-all.ps1
# Optional: -SkipTests to jump straight to the lifecycle, -KeepSandbox to inspect it.

param(
    [switch]$SkipTests,
    [switch]$KeepSandbox
)

$ErrorActionPreference = "Continue"
$env:Path += ";$env:USERPROFILE\.cargo\bin"

$root      = Split-Path -Parent $PSScriptRoot
$daemonDir = Join-Path $root "daemon"
$appDir    = Join-Path $root "app\src-tauri"
$q8        = "C:\Users\58294\Desktop\Q8"

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
Check "daemon builds" ($build -match "Finished" -and $build -notmatch "^error")
$agentrelay = Join-Path $daemonDir "target\debug\agentrelay.exe"
Check "agentrelay.exe exists" (Test-Path $agentrelay)
Pop-Location

if (-not $SkipTests) {
    Section "Test suites"
    Push-Location $daemonDir
    $t = & cargo test 2>&1 | Out-String
    $m = [regex]::Match($t, "test result: ok\. (\d+) passed; (\d+) failed")
    $daemonOk = $m.Success -and [int]$m.Groups[2].Value -eq 0
    Check "daemon: cargo test green" $daemonOk ("(" + $m.Groups[1].Value + " passed)")
    Pop-Location

    foreach ($cli in @("patchproof", "repodoctor", "lastgood")) {
        $dir = Join-Path $q8 $cli
        if (-not (Test-Path $dir)) { Check "$cli present" $false "not found"; continue }
        Push-Location $dir
        $out = (& npm.cmd test 2>&1 | Out-String) -replace "`e\[[\d;]*m", ""
        $tm = [regex]::Match($out, "Tests\s+(\d+)\s+passed")
        $green = $tm.Success -and ($out -notmatch "failed")
        Check "$cli: npm test green" $green ("(" + $tm.Groups[1].Value + " passed)")
        Pop-Location
    }
}

# ---------------------------------------------------------------------------------
Section "Build the desktop app"
Push-Location $appDir
$appBuild = & cargo build 2>&1 | Out-String
Check "app builds" ($appBuild -match "Finished" -and $appBuild -notmatch "^error\[")
$appExe = Join-Path $appDir "target\debug\agentrelay-app.exe"
Check "agentrelay-app.exe exists" (Test-Path $appExe)
Pop-Location

# ---------------------------------------------------------------------------------
Section "End-to-end lifecycle (real binary, throwaway sandbox)"

$sandbox = Join-Path $env:TEMP ("agentrelay-verify-" + (Get-Random))
New-Item -ItemType Directory -Force $sandbox | Out-Null
Push-Location $sandbox

# A real git repo with a passing check available (node --version), plus the user's
# own uncommitted file, so we can prove the checkpoint never eats it.
& git init -q -b main
Set-Content -Encoding ascii "README.md" "sample project"
& git add -A
& git -c user.email=t@e.com -c user.name=T commit -q -m "base"

# init
$initOut = & $agentrelay init 2>&1 | Out-String
Check "init scaffolds the workspace" (Test-Path ".agentrelay\config.json")
Check "init writes a self-ignoring .gitignore" (Test-Path ".agentrelay\.gitignore")

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
Set-Content -Encoding ascii ".agentrelay\config.json" $cfg

# status reflects the config, and says the budget out loud
$status = & $agentrelay status 2>&1 | Out-String
Check "status shows a spending cap" ($status -match "\$5\.00")

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

# Guard hook blocks a real forbidden action (no live agent needed - drive the hook)
& $agentrelay install-hook 2>&1 | Out-Null
Check "install-hook writes .claude/settings.json" (Test-Path ".claude\settings.json")
$hookIn = '{"tool_name":"Write","tool_input":{"file_path":".env"},"cwd":"' + ($sandbox -replace '\\','/') + '"}'
$hookOut = $hookIn | & $agentrelay hook --cwd $sandbox 2>&1 | Out-String
Check "the hook DENIES writing a forbidden path" (($hookOut -match "deny") -and ($LASTEXITCODE -eq 2))
$hookOk = '{"tool_name":"Write","tool_input":{"file_path":"src/ok.rs"},"cwd":"' + ($sandbox -replace '\\','/') + '"}'
$hookOk | & $agentrelay hook --cwd $sandbox 2>&1 | Out-Null
Check "the hook ALLOWS an in-workspace write" ($LASTEXITCODE -eq 0)

# templates (only if this build has them - agent-added, guard so an older build passes)
$help = & $agentrelay --help 2>&1 | Out-String
if ($help -match "templates") {
    $tpl = & $agentrelay templates 2>&1 | Out-String
    Check "templates lists at least one built-in" ($tpl.Trim().Length -gt 0)
}

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
