# Build and launch OpenPaint locally (PowerShell entry point).
#
# PowerShell can't execute run.sh, so this mirrors it for the Windows/tablet box.
# Both scripts do the same thing; use whichever matches your shell.
#
#   .\run.ps1               # release build (use this for anything feel-related)
#   .\run.ps1 -DebugBuild   # faster compile, but CPU brush stamping feels sluggish
#   .\run.ps1 -- --foo      # anything after `--` is passed to the app
#
# (The switch is -DebugBuild, not -Debug: PowerShell reserves -Debug as a common
# parameter, so declaring it here would be a duplicate-parameter error.)
#
# The console stays attached on purpose: the pen backend logs its first poses
# (position / pressure / tilt) and every DOWN/UP there, which is how stylus
# behavior gets verified. See docs/DECISIONS.md section 8.

[CmdletBinding()]
param(
	[switch]$DebugBuild,
	[Parameter(ValueFromRemainingArguments = $true)]
	[string[]]$AppArgs
)

$ErrorActionPreference = 'Stop'

# Run from the repo root regardless of where this was invoked from.
Set-Location $PSScriptRoot

# rustup installs here; harmless if cargo is already on PATH.
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path $cargoBin) {
	$env:PATH = "$cargoBin;$env:PATH"
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
	Write-Error 'cargo not found. Install Rust via https://rustup.rs'
	exit 1
}

$cargoArgs = @('run')
if (-not $DebugBuild) {
	$cargoArgs += '--release'
}
$cargoArgs += @('--bin', 'openpaint')

# Drop a leading `--` so callers can forward args to the app either way.
$forwarded = @($AppArgs | Where-Object { $_ -ne '--' })
if ($forwarded.Count -gt 0) {
	$cargoArgs += '--'
	$cargoArgs += $forwarded
}

& cargo @cargoArgs
exit $LASTEXITCODE
