# Pack weekcase.exe + portable.ini at zip root. Marker file next to the exe
# switches Paths onto .\data\; contents of the ini are ignored.
param(
    [string]$ExePath,
    [string]$OutDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot

if (-not $PSBoundParameters.ContainsKey('ExePath')) {
    Push-Location $Root
    try {
        & cargo build --release
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build --release failed ($LASTEXITCODE)"
        }
    } finally {
        Pop-Location
    }
    $ExePath = Join-Path $Root 'target\release\weekcase.exe'
}

if (-not (Test-Path -LiteralPath $ExePath)) {
    throw "weekcase.exe not found: $ExePath"
}

if (-not $OutDir) {
    $OutDir = Join-Path $Root 'target\package'
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$stage = Join-Path $OutDir 'stage'
if (Test-Path -LiteralPath $stage) {
    Remove-Item -LiteralPath $stage -Recurse -Force
}
New-Item -ItemType Directory -Path $stage | Out-Null

Copy-Item -LiteralPath $ExePath -Destination (Join-Path $stage 'weekcase.exe')

# ASCII so Windows PowerShell 5.1 and pwsh write the same bytes (no BOM).
Set-Content -LiteralPath (Join-Path $stage 'portable.ini') -Encoding Ascii -Value @(
    '; Weekcase portable mode'
    '; Put this file next to weekcase.exe. Presence enables:'
    ';   .\data\config.toml'
    ';   .\data\state.json'
    ';   .\data\undo.jsonl'
    ';   .\data\logs\weekcase.log'
    '; Contents of this file are ignored.'
    '; Uninstalling the app does not delete archived files under the archive root.'
)

$zip = Join-Path $OutDir 'weekcase-portable.zip'
if (Test-Path -LiteralPath $zip) {
    Remove-Item -LiteralPath $zip -Force
}

# Pass files (not the stage dir) so they sit at the zip root.
Compress-Archive -LiteralPath @(
    (Join-Path $stage 'weekcase.exe'),
    (Join-Path $stage 'portable.ini')
) -DestinationPath $zip -CompressionLevel Optimal
Remove-Item -LiteralPath $stage -Recurse -Force

Write-Host "Wrote $zip"
Write-Host "  weekcase.exe"
Write-Host "  portable.ini"
