<#
.SYNOPSIS
  Build, test and run Proofkin. Reloads PATH from the registry first, so it works in
  terminals opened before Rust/MinGW were installed (e.g. an already-running VS Code).

.EXAMPLE
  .\scripts\p2p.ps1 doctor      # check every prerequisite and print the fix for anything missing
  .\scripts\p2p.ps1 build       # cargo build
  .\scripts\p2p.ps1 test        # cargo test (no LLM credentials needed)
  .\scripts\p2p.ps1 demo        # worker + boss windows, allowlisted to each other, over the internet
  .\scripts\p2p.ps1 live-test   # 4 agents over the internet, 2 accounts, allowlist + real Copilot delegation
  .\scripts\p2p.ps1 evidence    # gates + tests matched to evidence/claims.json; report in target\evidence
  .\scripts\p2p.ps1 evidence -Live   # same, plus the live internet test
  .\scripts\p2p.ps1 agent --name me --providers copilot   # run one agent in this terminal
  .\scripts\p2p.ps1 agent auth status                     # any proofkin command, e.g. auth
#>
param(
    [Parameter(Position = 0)]
    [ValidateSet('doctor', 'build', 'test', 'demo', 'live-test', 'evidence', 'agent')]
    [string]$Command = 'doctor',
    # demo: folder the worker shares read-only with boss as 'repo'. Default: this repo.
    [string]$Workspace,
    # evidence: also run the live internet test.
    [switch]$Live,
    [Parameter(ValueFromRemainingArguments)]
    [string[]]$AgentArgs
)
$ErrorActionPreference = 'Stop'
$Repo = Split-Path $PSScriptRoot
if (-not $Workspace) { $Workspace = $Repo }
$Exe = Join-Path $Repo 'target\debug\proofkin.exe'
$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User') + ';' + $env:Path

function Test-Tool([string]$Name, [scriptblock]$Probe, [string]$Fix, [switch]$Optional) {
    $result = try { & $Probe } catch { $null }
    if ($result) {
        Write-Host ("  OK       {0,-22} {1}" -f $Name, $result) -ForegroundColor Green
        return $true
    }
    $tag, $color = if ($Optional) { 'OPTIONAL', 'Yellow' } else { 'MISSING', 'Red' }
    Write-Host ("  {0,-8} {1,-22} fix: {2}" -f $tag, $Name, $Fix) -ForegroundColor $color
    return [bool]$Optional
}

function Invoke-Doctor {
    Write-Host 'Checking prerequisites for Proofkin:'
    $ok = $true
    $ok = (Test-Tool 'rustc >= 1.94' {
            $v = (rustc --version) -replace '^rustc (\d+\.\d+).*', '$1'
            if ([version]$v -ge [version]'1.94') { rustc --version }
        } 'install Rust (README: Setup on Windows), or run: rustup update stable') -and $ok
    $ok = (Test-Tool 'cargo' { cargo --version } 'install Rust via rustup (README)') -and $ok
    if ((rustup show active-toolchain 2>$null) -match 'gnu') {
        $ok = (Test-Tool 'dlltool (MinGW-w64)' { dlltool --version | Select-Object -First 1 } 'winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e') -and $ok
    }
    $ok = (Test-Tool 'copilot CLI' { copilot --version | Select-Object -First 1 } 'winget install --id GitHub.Copilot -e') -and $ok
    $null = Test-Tool 'gh CLI (accounts)' { @((gh auth status --json hosts | ConvertFrom-Json).hosts.PSObject.Properties | ForEach-Object { $h = $_.Name; $_.Value | ForEach-Object { "$($_.login)@$h" } }) -join ', ' } 'winget install --id GitHub.cli -e, then gh auth login (once per account)' -Optional
    $null = Test-Tool 'node (claude/codex)' { node --version } 'winget install --id OpenJS.NodeJS.LTS -e' -Optional
    $null = Test-Tool 'bridge packages' { if (Test-Path (Join-Path $Repo 'bridge\node_modules\@openai\codex-sdk')) { 'installed' } } 'cd bridge; npm install' -Optional
    $null = Test-Tool 'python (live-test)' { python --version } 'winget install --id Python.Python.3.13 -e' -Optional
    $null = Test-Tool 'built binary' { if (Test-Path $Exe) { $Exe } } '.\scripts\p2p.ps1 build' -Optional
    if ($ok) { Write-Host 'Ready. Next: .\scripts\p2p.ps1 demo' -ForegroundColor Green }
    else { Write-Host 'Fix the MISSING items above, then run doctor again.' -ForegroundColor Red }
    return $ok
}

function Invoke-Build {
    Push-Location $Repo
    try {
        cargo build
        if ($LASTEXITCODE) { throw 'cargo build failed' }
    } finally { Pop-Location }
}

switch ($Command) {
    'doctor' { if (-not (Invoke-Doctor)) { exit 1 } }
    'build' { Invoke-Build }
    'test' {
        Push-Location $Repo
        try { cargo test } finally { Pop-Location }
        exit $LASTEXITCODE
    }
    'agent' {
        Invoke-Build
        & $Exe @AgentArgs
    }
    'demo' {
        Invoke-Build
        if (-not (& $Exe auth status --profile worker | Select-String 'copilot\s+signed in' -Quiet)) {
            $first = (gh auth status --json hosts | ConvertFrom-Json).hosts.PSObject.Properties | Select-Object -First 1
            if (-not $first) { throw "Sign the 'worker' profile in first: $Exe auth login copilot --profile worker --gh-user <login>" }
            $acct = $first.Value | Select-Object -First 1
            Write-Host "Signing profile 'worker' in to Copilot as $($acct.login)@$($first.Name) (change with: auth login copilot --profile worker --gh-user <login>)"
            & $Exe auth login copilot --profile worker --gh-user $acct.login --host $first.Name
            if ($LASTEXITCODE) { throw 'sign-in failed' }
        }
        $shell = (Get-Process -Id $PID).Path
        $workerId = ((& $Exe peers id --profile worker) | Select-String 'endpoint id: (\w+)').Matches[0].Groups[1].Value
        $bossId = ((& $Exe peers id --profile boss) | Select-String 'endpoint id: (\w+)').Matches[0].Groups[1].Value
        & $Exe share add repo $Workspace --profile worker | Out-Null
        & $Exe peers allow $bossId --profile worker --name boss --delegate --share data,repo | Out-Null
        & $Exe peers allow $workerId --profile boss --name worker | Out-Null
        Write-Host "Allowlisted: worker <-> boss (boss may delegate to worker, read-only, in worker's data folder and 'repo' = $Workspace). Agents find each other by key over the internet."
        $worker = "`$Host.UI.RawUI.WindowTitle='proofkin worker'; & '$Exe' --name worker --providers copilot --serve"
        $boss = "`$Host.UI.RawUI.WindowTitle='proofkin boss'; Write-Host 'Wait for + worker, then try:  /peers   and   /delegate copilot@worker:repo Summarize README.md in 3 bullets' -ForegroundColor Cyan; & '$Exe' --name boss"
        Start-Process $shell -ArgumentList '-NoExit', '-Command', $worker -WorkingDirectory $Repo
        Start-Sleep 2
        Start-Process $shell -ArgumentList '-NoExit', '-Command', $boss -WorkingDirectory $Repo
        Write-Host "Opened 2 windows: 'proofkin worker' (serves Copilot; boss may read its data folder and $Workspace) and 'proofkin boss'."
        Write-Host 'Type commands in the boss window; /quit in each window to exit.'
    }
    'live-test' {
        Invoke-Build
        python (Join-Path $PSScriptRoot 'live_test.py')
        exit $LASTEXITCODE
    }
    'evidence' {
        $evidenceArgs = @(if ($Live) { '--live' })
        python (Join-Path $PSScriptRoot 'evidence.py') @evidenceArgs
        exit $LASTEXITCODE
    }
}
