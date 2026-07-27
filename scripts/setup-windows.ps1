<#
  AgentRelay — Windows dev-environment setup.

  Installs everything needed to build AgentRelay on your own machine:
    core (always):  Rust (rustup+cargo) · Visual Studio Build Tools (C++) · Node.js LTS · Git · Tauri CLI
    optional:       Docker Desktop · Claude Code CLI   (toggles below)

  HOW TO RUN:
    1. Open PowerShell (any window).
    2. Run:   powershell -ExecutionPolicy Bypass -File .\setup-windows.ps1
       (the script re-launches itself as Administrator — approve the UAC prompt.)

  Safe to re-run: winget skips anything already installed.
#>

# ── toggles ──────────────────────────────────────────────────────────────────
$InstallDocker    = $false   # set to $true to also install Docker Desktop (for overnight isolation)
$InstallClaudeCLI = $false   # set to $true to also install the Claude Code CLI (needs a Claude subscription)

# ── elevate to Administrator (VS Build Tools / Docker need it) ────────────────
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
  Write-Host "Re-launching as Administrator (approve the UAC prompt)..." -ForegroundColor Yellow
  Start-Process powershell -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
  exit
}

$ErrorActionPreference = 'Continue'

function Have($cmd) { $null -ne (Get-Command $cmd -ErrorAction SilentlyContinue) }

function Install-Pkg($id, $name, $extra = @()) {
  Write-Host "==> Installing $name ($id)..." -ForegroundColor Cyan
  try {
    winget install --id $id --exact --accept-source-agreements --accept-package-agreements --silent @extra
  } catch {
    Write-Host "    winget reported: $($_.Exception.Message) (continuing)" -ForegroundColor DarkYellow
  }
}

function Refresh-Path {
  $machine = [System.Environment]::GetEnvironmentVariable('Path','Machine')
  $user    = [System.Environment]::GetEnvironmentVariable('Path','User')
  $env:Path = "$machine;$user;$env:USERPROFILE\.cargo\bin"
}

# ── preflight ────────────────────────────────────────────────────────────────
if (-not (Have winget)) {
  Write-Host "winget is not available. Install 'App Installer' from the Microsoft Store, then re-run." -ForegroundColor Red
  exit 1
}
Write-Host "AgentRelay setup — winget found." -ForegroundColor Green

# ── 1. Visual Studio Build Tools (C++)  [needed by Rust MSVC + Tauri] ─────────
Install-Pkg 'Microsoft.VisualStudio.2022.BuildTools' 'Visual Studio Build Tools (C++)' `
  @('--override', '--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended')

# ── 2. Rust (rustup + cargo) ─────────────────────────────────────────────────
Install-Pkg 'Rustlang.Rustup' 'Rust (rustup)'
Refresh-Path
if (Have rustup) {
  Write-Host "==> Installing the stable Rust toolchain (MSVC)..." -ForegroundColor Cyan
  rustup toolchain install stable
  rustup default stable
}

# ── 3. Node.js LTS (skip if already present) ─────────────────────────────────
if (Have node) { Write-Host "==> Node.js already installed: $(node -v)" -ForegroundColor Green }
else { Install-Pkg 'OpenJS.NodeJS.LTS' 'Node.js LTS' }

# ── 4. Git (skip if already present) ─────────────────────────────────────────
if (Have git) { Write-Host "==> Git already installed: $(git --version)" -ForegroundColor Green }
else { Install-Pkg 'Git.Git' 'Git' }

Refresh-Path

# ── 5. Tauri CLI (needs Rust) ────────────────────────────────────────────────
if (Have cargo) {
  Write-Host "==> Installing Tauri CLI (cargo install tauri-cli)..." -ForegroundColor Cyan
  cargo install tauri-cli --locked
} else {
  Write-Host "    cargo not on PATH yet — open a NEW terminal after this and run: cargo install tauri-cli --locked" -ForegroundColor DarkYellow
}

# ── 6. optional: Docker Desktop ──────────────────────────────────────────────
if ($InstallDocker) { Install-Pkg 'Docker.DockerDesktop' 'Docker Desktop' }

# ── 7. optional: Claude Code CLI ─────────────────────────────────────────────
if ($InstallClaudeCLI) {
  Refresh-Path
  if (Have npm) {
    Write-Host "==> Installing Claude Code CLI (npm i -g @anthropic-ai/claude-code)..." -ForegroundColor Cyan
    npm install -g @anthropic-ai/claude-code
  } else {
    Write-Host "    npm not found — install Node first, then: npm i -g @anthropic-ai/claude-code" -ForegroundColor DarkYellow
  }
}

# ── summary / verification ───────────────────────────────────────────────────
Refresh-Path
Write-Host "`n──────── verification ────────" -ForegroundColor Green
function Check($cmd, $args='--version') {
  if (Have $cmd) { try { Write-Host ("  OK  {0,-8} {1}" -f $cmd, ((& $cmd $args 2>&1 | Select-Object -First 1))) -ForegroundColor Green }
                   catch { Write-Host "  OK  $cmd (installed)" -ForegroundColor Green } }
  else { Write-Host "  --  $cmd  (open a NEW terminal to pick up PATH, or check the install)" -ForegroundColor Yellow }
}
Check rustc; Check cargo; Check node; Check npm; Check git
if (Have cargo) { Check 'cargo-tauri' 'tauri --version' }

Write-Host "`nDone. IMPORTANT: open a NEW terminal so PATH refreshes, then:" -ForegroundColor Cyan
Write-Host "  cd $HOME\AgentRelay\runner ; npm install ; npm test    # 71 tests should pass" -ForegroundColor White
Write-Host "Core install complete. Optional: set `$InstallDocker/`$InstallClaudeCLI = `$true at the top and re-run." -ForegroundColor Gray
