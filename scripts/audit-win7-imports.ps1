<#
.SYNOPSIS
  Refuses a Windows 7 build whose PE imports Windows 7 cannot satisfy.

.DESCRIPTION
  A Rust/Tauri exe that imports one symbol Windows 7 lacks fails to LOAD there:
  the user sees "The procedure entry point X could not be located" (or a missing
  combase.dll) and Orcaa never starts. Nothing in the build tells us; this does.

  Runs `dumpbin /IMPORTS` (Visual Studio, located via vswhere), prints the whole
  import table for the log, then fails when:
    * an imported DLL is not on the Windows 7 allowlist, or
    * a DLL / entry point is on the explicit denylist (WinRT, Win8+/Win10+ APIs,
      the API-set contracts Windows 7 never shipped).

  Used by the `build-win7` job in .github/workflows/release.yml and runnable
  locally from a Developer PowerShell:
    ./scripts/audit-win7-imports.ps1 -Exe src-tauri/target/x86_64-win7-windows-msvc/release/orcaa-desktop.exe
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Exe
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $Exe)) {
    Write-Error "Executable not found: $Exe"
}

# --- locate dumpbin -----------------------------------------------------------
function Find-Dumpbin {
    if ($env:DUMPBIN -and (Test-Path -LiteralPath $env:DUMPBIN)) { return $env:DUMPBIN }

    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path -LiteralPath $vswhere) {
        $found = & $vswhere -latest -products * -find "VC\Tools\MSVC\*\bin\Hostx64\x64\dumpbin.exe" 2>$null |
            Select-Object -First 1
        if ($found) { return $found }
    }

    $cmd = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    Write-Error "dumpbin.exe not found. Install the Visual Studio C++ build tools or set DUMPBIN=<path>."
}

$dumpbin = Find-Dumpbin
Write-Host "dumpbin: $dumpbin"
Write-Host "exe:     $Exe"

# --- parse the import table ---------------------------------------------------
$raw = & $dumpbin /NOLOGO /IMPORTS $Exe
if ($LASTEXITCODE -ne 0) { Write-Error "dumpbin failed with exit code $LASTEXITCODE" }

# dumpbin prints one block per DLL:
#     <dll name>
#       <import address table RVA> ... (header lines)
#         <hint> <function>
$imports = [ordered]@{}
$current = $null
foreach ($line in $raw) {
    if ($line -match '^\s{4}(\S+\.(dll|DLL|drv|DRV))\s*$') {
        $current = $Matches[1].ToLowerInvariant()
        if (-not $imports.Contains($current)) {
            $imports[$current] = New-Object System.Collections.Generic.List[string]
        }
        continue
    }
    if ($current -and $line -match '^\s+[0-9A-Fa-f]+\s+(\S+)\s*$') {
        $imports[$current].Add($Matches[1])
        continue
    }
    if ($line -match '^\s*Summary\s*$') { $current = $null }
}

if ($imports.Count -eq 0) {
    Write-Error "No import table parsed. Did the dumpbin output format change? Raw output follows.`n$($raw -join "`n")"
}

# --- Windows 7 knowledge ------------------------------------------------------
# DLLs every Windows 7 SP1 has. Anything else is a hard failure (better a false
# alarm you extend this list for than an exe that won't start).
$allowedDlls = @(
    "kernel32.dll", "user32.dll", "gdi32.dll", "advapi32.dll", "shell32.dll",
    "ole32.dll", "oleaut32.dll", "comctl32.dll", "comdlg32.dll", "winspool.drv",
    "ws2_32.dll", "bcrypt.dll", "crypt32.dll", "secur32.dll", "ntdll.dll",
    "uxtheme.dll", "dwmapi.dll", "shlwapi.dll", "imm32.dll", "setupapi.dll",
    "cfgmgr32.dll", "version.dll", "userenv.dll", "msimg32.dll", "winmm.dll",
    "iphlpapi.dll", "psapi.dll", "wininet.dll", "winhttp.dll", "rpcrt4.dll",
    "propsys.dll", "gdiplus.dll", "wtsapi32.dll", "powrprof.dll", "netapi32.dll",
    "dbghelp.dll", "hid.dll"
)

# Present on Win8+/Win10+ only, or WinRT. Any hit is fatal regardless of allowlist.
$deniedDllPatterns = @(
    '^combase\.dll$',                 # WinRT activation (Windows 8+)
    '^bcryptprimitives\.dll$',        # ProcessPrng (Windows 10 std)
    '^shcore\.dll$',                  # Windows 8.1+ (DPI, streams)
    '^api-ms-win-core-winrt-',        # WinRT API sets
    '^api-ms-win-core-.*-l1-2-',      # Windows 8+ contract revisions
    '^api-ms-win-crt-',               # UCRT via API sets (KB2999226 is not on a fresh Win7)
    '^ext-ms-win-',                   # Windows 10 extension API sets
    '^windows\.'                      # Windows.* WinRT DLLs
)

# Entry points that live in an allowed DLL but only exist from Windows 8/10 on.
$deniedFunctions = @(
    "ProcessPrng", "RoOriginateErrorW", "RoOriginateError", "RoGetActivationFactory",
    "RoInitialize", "RoUninitialize", "WindowsCreateString", "WindowsDeleteString",
    "WindowsGetStringRawBuffer", "SetProcessDpiAwarenessContext", "GetDpiForWindow",
    "GetDpiForMonitor", "GetDpiForSystem", "AdjustWindowRectExForDpi",
    "EnableNonClientDpiScaling", "SetThreadDpiAwarenessContext",
    "WaitOnAddress", "WakeByAddressSingle", "WakeByAddressAll",
    # NOTE: TryAcquireSRWLock{Exclusive,Shared}, GetTouchInputInfo, ChangeWindowMessageFilterEx and
    # SetWindowDisplayAffinity were INTRODUCED in Windows 7 — do not list them.
    "GetSystemTimePreciseAsFileTime", "GetOverlappedResultEx", "PrefetchVirtualMemory",
    "CreateFile2", "GetTempPath2W", "SetDefaultDllDirectories"
)

# Exported by Windows 7 SP1 only after a specific Windows Update. Reported as a
# WARNING, not a failure: the import is legitimate (it comes from Microsoft's own
# WebView2LoaderStatic.lib), but a never-updated Win7 install will not start the
# app — the README's system requirements name the update for that reason.
$warnFunctions = @{
    "EventSetInformation" = "needs KB2882822 (2013) on Windows 7 SP1 - imported by Microsoft's WebView2LoaderStatic.lib"
}

# --- report + verdict ---------------------------------------------------------
$problems = New-Object System.Collections.Generic.List[string]
$warnings = New-Object System.Collections.Generic.List[string]

Write-Host ""
Write-Host "Import table:"
foreach ($dll in $imports.Keys) {
    $fns = $imports[$dll]
    Write-Host ("  {0}  ({1} imports)" -f $dll, $fns.Count)

    $denied = $deniedDllPatterns | Where-Object { $dll -match $_ } | Select-Object -First 1
    if ($denied) {
        $problems.Add("DLL '$dll' does not exist on Windows 7 (matched $denied)")
    } elseif ($allowedDlls -notcontains $dll) {
        $problems.Add("DLL '$dll' is not on the Windows 7 allowlist. Verify it ships with Win7 SP1 and extend the list, or remove the dependency.")
    }

    foreach ($fn in $fns) {
        if ($deniedFunctions -contains $fn) {
            $problems.Add("Entry point '$fn' (from $dll) is not exported on Windows 7")
        } elseif ($warnFunctions.ContainsKey($fn)) {
            $warnings.Add("Entry point '$fn' (from $dll) $($warnFunctions[$fn])")
        }
    }
}

Write-Host ""
if ($warnings.Count -gt 0) {
    Write-Host "Windows 7 import audit warnings (the app starts only on an UPDATED Windows 7 SP1):" -ForegroundColor Yellow
    foreach ($w in $warnings) {
        Write-Host "  - $w" -ForegroundColor Yellow
        Write-Host "::warning::$w"
    }
    Write-Host ""
}
if ($problems.Count -gt 0) {
    Write-Host "Windows 7 import audit FAILED:" -ForegroundColor Red
    foreach ($p in $problems) { Write-Host "  - $p" -ForegroundColor Red }
    Write-Host ""
    Write-Host "Full function list per DLL (for diagnosis):"
    foreach ($dll in $imports.Keys) {
        Write-Host "  [$dll]"
        foreach ($fn in ($imports[$dll] | Sort-Object)) { Write-Host "      $fn" }
    }
    exit 1
}

Write-Host "Windows 7 import audit passed: every imported DLL and entry point exists on Windows 7 SP1." -ForegroundColor Green
exit 0
