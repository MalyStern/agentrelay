<#
  AgentRelay - Windows dev-environment setup.

  Installs everything needed to build AgentRelay on your own machine:
    core (always):  Rust (rustup+cargo) . Visual Studio Build Tools (C++) . Node.js LTS . Git . Tauri CLI
    optional:       Docker Desktop . Claude Code CLI   (toggles below)

  HOW TO RUN:
    powershell -ExecutionPolicy Bypass -File "$HOME\AgentRelay\scripts\setup-windows.ps1"
    (the script re-launches itself as Administrator - approve the UAC prompt.)

  Safe to re-run: winget skips anything already installed.
#>

# ---- toggles -----------------------------------------------------------------
$InstallDocker    = $false   # set to $true to also install Docker Desktop
$InstallClaudeCLI = $false   # set to $true to also install the Claude Code CLI

# ---- elevate to Administrator ------------------------------------------------
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
  Write-Host "Re-launching as Administrator (approve the UAC prompt)..." -ForegroundColor Yellow
  Start-Process powershell -Verb RunAs -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
  exit
}

$ErrorActionPreference = 'Continue'

function Have([string]$cmd) {
  return $null -ne (Get-Command $cmd -ErrorAction SilentlyContinue)
}

function Install-Pkg([string]$id, [string]$name, [string[]]$extra) {
  Write-Host "==> Installing $name ($id)..." -ForegroundColor Cyan
  # --source winget: skip the 'msstore' source, which fails TLS on this network
  # (0x8a15005e cert mismatch). The community 'winget' source works.
  try {
    if ($extra) {
      winget install --id $id --exact --source winget --accept-source-agreements --accept-package-agreements --silent @extra
    } else {
      winget install --id $id --exact --source winget --accept-source-agreements --accept-package-agreements --silent
    }
  } catch {
    Write-Host "    winget note: $($_.Exception.Message) (continuing)" -ForegroundColor DarkYellow
  }
}

function Refresh-Path {
  $machine = [System.Environment]::GetEnvironmentVariable('Path','Machine')
  $user    = [System.Environment]::GetEnvironmentVariable('Path','User')
  $env:Path = "$machine;$user;$env:USERPROFILE\.cargo\bin"
}

function Check([string]$cmd, [string]$verArg = '--version') {
  if (Have $cmd) {
    try {
      $out = (& $cmd $verArg 2>&1 | Select-Object -First 1)
      Write-Host ("  OK  " + $cmd.PadRight(10) + " " + $out) -ForegroundColor Green
    } catch {
      Write-Host ("  OK  " + $cmd + " (installed)") -ForegroundColor Green
    }
  } else {
    Write-Host ("  --  " + $cmd + "  (open a NEW terminal to pick up PATH)") -ForegroundColor Yellow
  }
}

# ---- preflight ---------------------------------------------------------------
if (-not (Have winget)) {
  Write-Host "winget is not available. Install 'App Installer' from the Microsoft Store, then re-run." -ForegroundColor Red
  exit 1
}
Write-Host "AgentRelay setup - winget found." -ForegroundColor Green

# ---- 1. Visual Studio Build Tools (C++) --------------------------------------
Install-Pkg 'Microsoft.VisualStudio.2022.BuildTools' 'Visual Studio Build Tools (C++)' @('--override','--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended')

# ---- 2. Rust (rustup + cargo) ------------------------------------------------
Install-Pkg 'Rustlang.Rustup' 'Rust (rustup)'
Refresh-Path
if (Have rustup) {
  Write-Host "==> Installing the stable Rust toolchain (MSVC)..." -ForegroundColor Cyan
  rustup toolchain install stable
  rustup default stable
}

# ---- 3. Node.js LTS (skip if present) ----------------------------------------
if (Have node) {
  Write-Host ("==> Node.js already installed: " + (node -v)) -ForegroundColor Green
} else {
  Install-Pkg 'OpenJS.NodeJS.LTS' 'Node.js LTS'
}

# ---- 4. Git (skip if present) ------------------------------------------------
if (Have git) {
  Write-Host ("==> Git already installed: " + (git --version)) -ForegroundColor Green
} else {
  Install-Pkg 'Git.Git' 'Git'
}

Refresh-Path

# ---- 5. Tauri CLI (needs Rust) -----------------------------------------------
if (Have cargo) {
  Write-Host "==> Installing Tauri CLI (cargo install tauri-cli)..." -ForegroundColor Cyan
  cargo install tauri-cli --locked
} else {
  Write-Host "    cargo not on PATH yet - open a NEW terminal, then: cargo install tauri-cli --locked" -ForegroundColor DarkYellow
}

# ---- 6. optional: Docker Desktop ---------------------------------------------
if ($InstallDocker) {
  Install-Pkg 'Docker.DockerDesktop' 'Docker Desktop'
}

# ---- 7. optional: Claude Code CLI --------------------------------------------
if ($InstallClaudeCLI) {
  Refresh-Path
  if (Have npm) {
    Write-Host "==> Installing Claude Code CLI..." -ForegroundColor Cyan
    npm install -g '@anthropic-ai/claude-code'
  } else {
    Write-Host "    npm not found - install Node first, then: npm i -g @anthropic-ai/claude-code" -ForegroundColor DarkYellow
  }
}

# ---- verification ------------------------------------------------------------
Refresh-Path
Write-Host ""
Write-Host "-------- verification --------" -ForegroundColor Green
Check 'rustc'
Check 'cargo'
Check 'node'
Check 'npm'
Check 'git'

Write-Host ""
Write-Host "Done. IMPORTANT: open a NEW terminal so PATH refreshes, then run:" -ForegroundColor Cyan
Write-Host "  cd `$HOME\AgentRelay\runner ; npm install ; npm test" -ForegroundColor White
Write-Host "(71 tests should pass.)" -ForegroundColor White
Write-Host "Optional extras: set `$InstallDocker / `$InstallClaudeCLI = `$true at the top and re-run." -ForegroundColor Gray
