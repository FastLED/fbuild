# One-time setup for Docker/WSL USB passthrough on Windows.
# FastLED/fbuild#899 — validated end-to-end at PR #898 closure.
#
# Run elevated (one UAC prompt). After this completes:
#   - usbipd-win is installed (signed driver)
#   - Espressif devices (303A:1001) are bound (one-time per device)
#   - Per-session: usbipd attach --busid X-Y --wsl=Ubuntu  (user-mode)

param(
    [string]$UsbipdMsi = 'C:\tmp\usbipd-install\usbipd-win_5.3.0_x64.msi',
    [string]$AlpineRootfs = 'C:\tmp\alpine-rootfs.tar.gz',
    [string]$WslDistroDir = 'C:\wsl-distros\fbuild-test'
)

$ErrorActionPreference = 'Stop'

function Test-IsAdmin {
    $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
    return $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (-not (Test-IsAdmin)) {
    Write-Host "Re-launching elevated..."
    Start-Process powershell -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File',$MyInvocation.MyCommand.Path -Verb RunAs -Wait
    exit 0
}

# 1. Install usbipd-win
Write-Host "==> [1/3] Installing usbipd-win..."
if (-not (Test-Path 'C:\Program Files\usbipd-win\usbipd.exe')) {
    if (-not (Test-Path $UsbipdMsi)) {
        $url = 'https://github.com/dorssel/usbipd-win/releases/download/v5.3.0/usbipd-win_5.3.0_x64.msi'
        New-Item -ItemType Directory -Force -Path (Split-Path $UsbipdMsi) | Out-Null
        Write-Host "  Downloading from $url..."
        Invoke-WebRequest -Uri $url -OutFile $UsbipdMsi -UseBasicParsing
    }
    msiexec /i "$UsbipdMsi" /qb /norestart | Out-Host
    if (-not (Test-Path 'C:\Program Files\usbipd-win\usbipd.exe')) {
        Write-Error "usbipd install failed"
        exit 1
    }
}
Write-Host "  OK"

# 2. Bind every 303A:1001 device
Write-Host "==> [2/3] Binding Espressif (303A:1001) devices..."
$usbipd = 'C:\Program Files\usbipd-win\usbipd.exe'
$list = (& $usbipd list) -join "`n"
foreach ($line in ($list -split "`n")) {
    if ($line -match '^\s*(\d+-\d+)\s+303a:1001.*(\bNot shared\b)') {
        $busid = $matches[1]
        Write-Host "  Binding $busid..."
        & $usbipd bind --busid $busid
    }
}
Write-Host "  OK"

# 3. Create / verify WSL distro for testing
Write-Host "==> [3/3] WSL test distro..."
$ubuntu = wsl --list --quiet 2>&1 | Out-String
if ($ubuntu -notmatch 'Ubuntu') {
    Write-Host "  Installing Ubuntu (this may take a few minutes)..."
    wsl --install -d Ubuntu --no-launch | Out-Host
}
Write-Host "  OK"

Write-Host ""
Write-Host "==> SETUP COMPLETE."
Write-Host "    Now from a NORMAL (non-admin) PowerShell:"
Write-Host "        usbipd attach --busid <BUSID> --wsl=Ubuntu"
Write-Host "    Then run the test:"
Write-Host "        bash ci/docker-test-serial/run-test.sh"
