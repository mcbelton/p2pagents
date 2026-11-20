<#
.SYNOPSIS
  Installs the Proofkin binary (proofkin.exe) from GitHub releases (no source code or Rust needed).

.EXAMPLE
  irm https://github.com/mcbelton/proofkin/releases/latest/download/install.ps1 | iex
#>
param(
    [string]$Version = 'latest',
    [string]$Repo = 'mcbelton/proofkin',
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'proofkin\bin'),
    # GitHub CLI account to download with (default: gh's active account).
    [string]$GhUser
)
$ErrorActionPreference = 'Stop'
$arch = try { [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() } catch { $env:PROCESSOR_ARCHITECTURE }
# Native ARM64 build when available; x64 also runs on Windows ARM through emulation.
$assets = if ($arch -match 'arm64') { 'proofkin-aarch64-pc-windows-msvc.zip', 'proofkin-x86_64-pc-windows-msvc.zip' }
          else { @('proofkin-x86_64-pc-windows-msvc.zip') }
$tmp = Join-Path ([IO.Path]::GetTempPath()) "proofkin-install-$PID"
New-Item -ItemType Directory $tmp -Force | Out-Null
$savedToken = $env:GH_TOKEN
try {
    $tag = if ($Version -eq 'latest') { @() } else { @($Version) }
    $zip = $null
    if (-not $GhUser) {
        # Public repos: plain HTTPS, no GitHub sign-in needed.
        foreach ($asset in $assets) {
            $url = if ($Version -eq 'latest') { "https://github.com/$Repo/releases/latest/download/$asset" }
                   else { "https://github.com/$Repo/releases/download/$Version/$asset" }
            try { Invoke-WebRequest $url -OutFile (Join-Path $tmp $asset) -UseBasicParsing; $zip = Join-Path $tmp $asset; break }
            catch { $httpError = "$url : $($_.Exception.Message)" }
        }
    }
    if (-not $zip) {
        if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
            throw "Could not download $httpError. For a private repo, install the GitHub CLI and sign in with an account that can read it."
        }
        if ($GhUser) {
            $env:GH_TOKEN = gh auth token --hostname github.com --user $GhUser
            if ($LASTEXITCODE -or -not $env:GH_TOKEN) { throw "gh has no github.com account '$GhUser'; sign it in with: gh auth login" }
        }
        # Private repos: download as a GitHub account that can read the repo.
        foreach ($asset in $assets) {
            gh release download @tag -R $Repo -p $asset -D $tmp --clobber 2>$null
            if (-not $LASTEXITCODE) { $zip = Join-Path $tmp $asset; break }
        }
        if (-not $zip) {
            $me = gh api user --jq .login 2>$null
            throw ("Could not download from $Repo as GitHub user '$me'. A private repo looks 'not found' to accounts " +
                "without access. Use an account that can read it: .\install.ps1 -GhUser <login>  " +
                "(see gh auth status), or ask the owner to add '$me' as a collaborator.")
        }
    }
    Write-Host "Downloaded $(Split-Path $zip -Leaf) ($arch)"
    New-Item -ItemType Directory $InstallDir -Force | Out-Null
    Expand-Archive $zip -DestinationPath $InstallDir -Force
} finally {
    $env:GH_TOKEN = $savedToken
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd(';') + ';' + $InstallDir), 'User')
    Write-Host "Added $InstallDir to your user PATH (open a new terminal to use it)."
}
$env:Path = "$InstallDir;$env:Path"
Write-Host "Installed: $(& (Join-Path $InstallDir 'proofkin.exe') --version)"
$legacy = Join-Path $env:LOCALAPPDATA 'p2pagents\bin'
if (Test-Path (Join-Path $legacy 'p2pagents.exe')) {
    Write-Host "p2pagents is now Proofkin: use proofkin (same profiles and data). You can delete $legacy and remove it from your PATH."
}
Write-Host @'
Next steps:
  winget install GitHub.Copilot        # Copilot CLI (each agent runs its own instance)
  winget install GitHub.cli; gh auth login   # optional: easiest way to sign agents in to GitHub accounts
  proofkin setup                      # optional: Claude/Codex support (needs Node.js 18+)
  proofkin doctor                     # check everything
  proofkin auth login copilot --profile <me> --gh-user <github-login>
  proofkin peers id --profile <me>    # share your ticket with peers
'@
