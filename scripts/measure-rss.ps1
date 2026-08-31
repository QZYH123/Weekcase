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

function Write-Unavailable {
    param([string]$Why)
    Write-Warning "WorkingSetSize unavailable: $Why"
    Write-Host 'WARNING: WorkingSetSize unavailable'
    Write-Host "WARNING: $Why"
    Write-Host 'WARNING: this run is not a release measurement.'
    Write-GateNote
}

function Get-WeekcaseProcess {
    param([System.Diagnostics.Process]$Prefer)
    if ($null -ne $Prefer) {
        $Prefer.Refresh()
        if (-not $Prefer.HasExited) {
            return $Prefer
        }
    }
    Get-Process -Name 'weekcase' -ErrorAction SilentlyContinue |
        Where-Object { -not $_.HasExited } |
        Select-Object -First 1
}

function Get-WorkingSetSizeBytes {
    param([int]$Id)
    $cim = Get-CimInstance -ClassName Win32_Process -Filter "ProcessId=$Id" -ErrorAction SilentlyContinue
    if ($cim) {
        return [int64]$cim.WorkingSetSize
    }
    $p = Get-Process -Id $Id -ErrorAction SilentlyContinue
    if ($p) {
        return [int64]$p.WorkingSet64
    }
    return $null
}

function Stop-WeekcaseGently {
    param([int]$Id)
    $p = Get-Process -Id $Id -ErrorAction SilentlyContinue
    if ($null -eq $p -or $p.HasExited) {
        return
    }
    # Kill leaves the notify icon until mouseover; WM_QUIT hits the GetMessage
    # loop so tray run() can NIM_DELETE before exit.
    if (-not ('WeekcaseQuit' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class WeekcaseQuit {
  [DllImport("user32.dll", CharSet = CharSet.Unicode)]
  public static extern IntPtr FindWindow(string lpClassName, string lpWindowName);
  [DllImport("user32.dll")]
  public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);
  [DllImport("user32.dll")]
  public static extern bool PostThreadMessage(uint idThread, uint Msg, UIntPtr wParam, IntPtr lParam);
  public const uint WM_QUIT = 0x0012;
}
'@
    }
    $tid = [uint32]0
    try {
        $hwnd = [WeekcaseQuit]::FindWindow('WeekcaseTray', $null)
        if ($hwnd -ne [IntPtr]::Zero) {
            $windowPid = [uint32]0
            $windowTid = [WeekcaseQuit]::GetWindowThreadProcessId($hwnd, [ref]$windowPid)
            if ($windowPid -eq $Id) {
                $tid = $windowTid
            }
        }
        if ($tid -eq 0 -and $p.Threads.Count -gt 0) {
            $tid = [uint32]$p.Threads[0].Id
        }
        if ($tid -ne 0) {
            $null = [WeekcaseQuit]::PostThreadMessage($tid, [WeekcaseQuit]::WM_QUIT, [UIntPtr]::Zero, [IntPtr]::Zero)
        }
    } catch {
        $tid = 0
    }
    if ($p.WaitForExit(5000)) {
        return
    }
    Stop-Process -Id $Id -ErrorAction SilentlyContinue
    if ($p.WaitForExit(2000)) {
        return
    }
    Stop-Process -Id $Id -Force -ErrorAction SilentlyContinue
}

if (-not (Test-Path -LiteralPath $ExePath)) {
    Write-Unavailable "weekcase.exe not found: $ExePath (cargo build --release)"
    exit 0
}

$target = $null
$owned = $false
try {
    $target = Get-WeekcaseProcess
    if ($null -ne $target) {
        Write-Host "using existing weekcase pid=$($target.Id)"
    } else {
        $child = Start-Process -FilePath $ExePath -WorkingDirectory (Split-Path -Parent $ExePath) -PassThru
        Start-Sleep -Seconds 2
        $child.Refresh()
        if (-not $child.HasExited) {
            $target = $child
            $owned = $true
            Write-Host "started weekcase pid=$($target.Id)"
        } else {
            # Mutex holder is the process whose WS we actually want.
            $target = Get-WeekcaseProcess
            if ($null -eq $target) {
                Write-Unavailable "process exited before idle wait (exit=$($child.ExitCode)); no weekcase PID"
                exit 0
            }
            Write-Host "using existing weekcase pid=$($target.Id)"
        }
    }

    Write-Host "idle ${IdleSeconds}s pid=$($target.Id)"
    Start-Sleep -Seconds $IdleSeconds

    $live = Get-WeekcaseProcess -Prefer $target
    if ($null -eq $live) {
        Write-Unavailable 'process exited during idle wait'
        $owned = $false
        exit 0
    }

    $ws = Get-WorkingSetSizeBytes -Id $live.Id
    if ($null -eq $ws) {
        Write-Unavailable "could not read WorkingSetSize for pid=$($live.Id)"
        exit 0
    }

    $mb = [math]::Round($ws / 1MB, 2)
    Write-Host "WorkingSetSize=$ws"
    Write-Host "WorkingSetSizeMB=$mb"
    Write-GateNote
    if ($ws -gt 20MB) {
        Write-Host 'WARNING: above 20 MB release cap (human gate only).'
    }
} catch {
    Write-Unavailable "$_"
} finally {
    if ($owned -and $null -ne $target) {
        Stop-WeekcaseGently -Id $target.Id
    }
}

exit 0
