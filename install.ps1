<#
.SYNOPSIS
  Installs the p2pagents binary from GitHub releases (no source code or Rust needed).

.EXAMPLE
  # private repo (you need read access; uses your GitHub CLI sign-in):
  gh release download -R mcbelton/p2pagents -p install.ps1 -O install.ps1; .\install.ps1
  # public repo:
  irm https://github.com/mcbelton/p2pagents/releases/latest/download/install.ps1 | iex
#>
param(
    [string]$Version = 'latest',
    [string]$Repo = 'mcbelton/p2pagents',
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'p2pagents\bin')
)
$ErrorActionPreference = 'Stop'
$asset = 'p2pagents-x86_64-pc-windows-msvc.zip'
$tmp = Join-Path ([IO.Path]::GetTempPath()) "p2pagents-install-$PID"
New-Item -ItemType Directory $tmp -Force | Out-Null
try {
    $zip = Join-Path $tmp $asset
    $tag = if ($Version -eq 'latest') { @() } else { @($Version) }
    if (Get-Command gh -ErrorAction SilentlyContinue) {
        # Works for private repos the signed-in GitHub account can read.
        gh release download @tag -R $Repo -p $asset -D $tmp --clobber
        if ($LASTEXITCODE) { throw "gh release download failed (do you have access to $Repo? try: gh auth status)" }
    } else {
        $url = if ($Version -eq 'latest') { "https://github.com/$Repo/releases/latest/download/$asset" }
               else { "https://github.com/$Repo/releases/download/$Version/$asset" }
        Invoke-WebRequest $url -OutFile $zip -UseBasicParsing
    }
    New-Item -ItemType Directory $InstallDir -Force | Out-Null
    Expand-Archive $zip -DestinationPath $InstallDir -Force
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $InstallDir) {
    [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd(';') + ';' + $InstallDir), 'User')
    Write-Host "Added $InstallDir to your user PATH (open a new terminal to use it)."
}
$env:Path = "$InstallDir;$env:Path"
Write-Host "Installed: $(& (Join-Path $InstallDir 'p2pagents.exe') --version)"
Write-Host @'
Next steps:
  winget install GitHub.Copilot        # Copilot CLI (each agent runs its own instance)
  winget install GitHub.cli; gh auth login   # optional: easiest way to sign agents in to GitHub accounts
  p2pagents setup                      # optional: Claude/Codex support (needs Node.js 18+)
  p2pagents doctor                     # check everything
  p2pagents auth login copilot --profile <me> --gh-user <github-login>
  p2pagents peers id --profile <me>    # share your ticket with peers
'@
