# Installer test (CI): install, update and uninstall while `dbclaw-ctl.exe mcp` is running,
# the way an AI agent keeps it running. A running .exe can't be replaced or deleted, so the
# installer and uninstaller must stop it first.
#
#   pwsh packaging/windows/test-installer.ps1 -Setup dist\db-claw-v…-setup.exe
param([Parameter(Mandatory = $true)][string]$Setup)
$ErrorActionPreference = 'Stop'

$app = Join-Path $env:LOCALAPPDATA 'Programs\DB Claw'
$ctl = Join-Path $app 'dbclaw-ctl.exe'
$logs = Join-Path $env:RUNNER_TEMP 'dbclaw-installer-logs'
New-Item -ItemType Directory -Force -Path $logs | Out-Null

function Invoke-Setup([string]$Name, [string[]]$Extra = @()) {
    $log = Join-Path $logs "$Name.log"
    $argv = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=$log") + $Extra
    $p = Start-Process -FilePath $Setup -ArgumentList $argv -Wait -PassThru
    $text = Get-Content $log -Raw
    # Locked files show up as access denied / sharing violation (codes 5 / 32) and as
    # suppressed retry/ignore message boxes, even when setup still exits with 0.
    if ($p.ExitCode -ne 0 -or $text -match '(?i)message box|code (5|32)\b|failed') {
        Write-Host $text
        throw "$Name failed (exit code $($p.ExitCode))"
    }
    Write-Host "$Name ok"
}

function Start-Mcp {
    $psi = New-Object System.Diagnostics.ProcessStartInfo $ctl, 'mcp'
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true   # kept open, like an MCP client
    $psi.RedirectStandardOutput = $true
    $proc = [System.Diagnostics.Process]::Start($psi)
    Start-Sleep -Seconds 1
    if ($proc.HasExited) { throw "dbclaw-ctl mcp exited early ($($proc.ExitCode))" }
    # Precondition: the running exe really is locked against replacement.
    try {
        [System.IO.File]::Open($ctl, 'Open', 'ReadWrite', 'None').Close()
        throw 'expected dbclaw-ctl.exe to be locked while it runs'
    } catch [System.IO.IOException] { }
    return $proc
}

# 1. Fresh install.
Invoke-Setup 'install'
if (-not (Test-Path $ctl)) { throw "not installed: $ctl" }

# 2. Update (same command line the app uses) while the MCP server runs.
$mcp = Start-Mcp
Invoke-Setup 'update' @('/CLOSEAPPLICATIONS', '/UPDATE')
if (-not $mcp.WaitForExit(5000)) { throw 'update left dbclaw-ctl mcp running' }
Get-Process dbclaw -ErrorAction SilentlyContinue | Stop-Process -Force   # /UPDATE relaunches the app

# 3. Manual reinstall while the MCP server runs.
$mcp = Start-Mcp
Invoke-Setup 'reinstall'
if (-not $mcp.WaitForExit(5000)) { throw 'reinstall left dbclaw-ctl mcp running' }

# 4. Uninstall while the MCP server runs. The uninstaller hands off to a temp copy and
#    returns at once, so wait for the files to disappear.
$mcp = Start-Mcp
Start-Process -FilePath (Join-Path $app 'unins000.exe') -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait
$deadline = (Get-Date).AddSeconds(60)
while ((Test-Path $ctl) -or (Test-Path (Join-Path $app 'dbclaw.exe'))) {
    if ((Get-Date) -gt $deadline) { throw "uninstall left files in $app" }
    Start-Sleep -Milliseconds 500
}
if (-not $mcp.HasExited) { throw 'uninstall left dbclaw-ctl mcp running' }
Write-Host 'uninstall ok'
