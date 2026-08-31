# Idle Working Set after 5 minutes. 20 MB is a human release gate on a clean
# VM; this script never fails because of the number (not a GHA fail).
param(
    [string]$ExePath,
    [int]$IdleSeconds = 300
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent $PSScriptRoot
if (-not $ExePath) {
    $ExePath = Join-Path $Root 'target\release\weekcase.exe'
}

function Write-GateNote {
    Write-Host 'Idle stretch 12 MB; release cap 20 MB. Measure on a clean VM; not a CI fail gate.'
}

if (-not (Test-Path -LiteralPath $ExePath)) {
    Write-Host "weekcase.exe not found: $ExePath"
    Write-Host 'Build first: cargo build --release'
    Write-GateNote
    exit 0
}

$proc = $null
try {
    $proc = Start-Process -FilePath $ExePath -WorkingDirectory (Split-Path -Parent $ExePath) -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 2
    $proc.Refresh()
    if ($proc.HasExited) {
        Write-Host "process exited before idle wait (exit=$($proc.ExitCode)); another instance?"
        Write-Host 'WorkingSetSize unavailable'
        Write-GateNote
        exit 0
    }

    Write-Host "idle ${IdleSeconds}s pid=$($proc.Id)"
    Start-Sleep -Seconds $IdleSeconds
    $proc.Refresh()
    if ($proc.HasExited) {
        Write-Host "process exited during idle wait (exit=$($proc.ExitCode))"
        Write-Host 'WorkingSetSize unavailable'
        Write-GateNote
        exit 0
    }

    $ws = $null
    $cim = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId=$($proc.Id)" -ErrorAction SilentlyContinue
    if ($cim) {
        $ws = [int64]$cim.WorkingSetSize
    } else {
        $ws = [int64]$proc.WorkingSet64
    }

    $mb = [math]::Round($ws / 1MB, 2)
    Write-Host "WorkingSetSize=$ws"
    Write-Host "WorkingSetSizeMB=$mb"
    Write-GateNote
    if ($ws -gt 20MB) {
        Write-Host 'WARNING: above 20 MB release cap (human gate only).'
    }
} catch {
    Write-Host $_
    Write-GateNote
} finally {
    if ($null -ne $proc) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
}

exit 0
