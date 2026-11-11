# Starts the newest build of the GUI and the server (inocode.exe). Whatever is behind the
# sources (committed or not) is built first. A stamp next to each build holds the fingerprint of
# the sources it was built from; a build with another stamp is old.
#
#   launch.ps1            build what is old, start the GUI
#   launch.ps1 -Check     only say what is old (builds nothing, starts nothing)
#   launch.ps1 -NoStart   build what is old, do not start
#
# A running GUI is never stopped, and the build never writes a file that runs: the builds are
# copied to target\run\<fingerprint>\ and started from there.
param([switch]$Check, [switch]$NoStart)

$ErrorActionPreference = 'Stop'
$gui = Split-Path $PSScriptRoot -Parent
$root = Split-Path (Split-Path $gui -Parent) -Parent
$srv = Join-Path $root 'cli\inocode'
$srvOut = Join-Path $srv 'codex-rs\target-win\dev-small'
$guiOut = Join-Path $gui 'target\debug'
$run = Join-Path $gui 'target\run'
$logs = Join-Path $env:TEMP 'inocode-launch'
New-Item -ItemType Directory -Force $logs | Out-Null

$serverExes = 'inocode.exe', 'inocode-code-mode-host.exe', 'inocode-command-runner.exe', 'inocode-windows-sandbox-setup.exe', 'inocode-windows-managed-deny-probe.exe'

# A fingerprint of the files a build is made from: path, size and time of each file that git
# lists (tracked, or untracked and not ignored), so committed and uncommitted changes both count.
function Get-Fingerprint([string]$repo, [string[]]$paths) {
    $list = & git -C $repo -c core.quotePath=false ls-files -c -o --exclude-standard -- $paths
    $sb = New-Object System.Text.StringBuilder
    foreach ($f in ($list | Sort-Object -Unique)) {
        $i = New-Object System.IO.FileInfo (Join-Path $repo $f)
        if ($i.Exists) { [void]$sb.Append("$f|$($i.Length)|$($i.LastWriteTimeUtc.Ticks)`n") } else { [void]$sb.Append("$f|gone`n") }
    }
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $bytes = $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($sb.ToString()))
    ($bytes | ForEach-Object { $_.ToString('x2') }) -join ''
}

function Test-Current([string]$dir, [string]$stampName, [string]$fp, [string[]]$exes) {
    foreach ($e in $exes) { if (-not (Test-Path (Join-Path $dir $e))) { return $false } }
    $stamp = Join-Path $dir $stampName
    (Test-Path $stamp) -and ((Get-Content $stamp -Raw).Trim() -eq $fp)
}

$guiFp = Get-Fingerprint $gui @('Cargo.toml', 'Cargo.lock', 'build.rs', 'src', 'ui', 'assets')
$srvFp = Get-Fingerprint $srv @('codex-rs')
$guiOld = -not (Test-Current $guiOut '.launch-stamp' $guiFp @('inocode-desktop.exe'))
$srvOld = -not (Test-Current $srvOut '.launch-stamp' $srvFp $serverExes)

if ($Check) {
    "GUI: " + $(if ($guiOld) { '古い' } else { '最新' })
    "サーバー: " + $(if ($srvOld) { '古い' } else { '最新' })
    return
}

function Show-Failure([string]$what, [string]$log) {
    Write-Host "$what のビルドに失敗しました" -ForegroundColor Red
    Get-Content $log -Tail 30 | ForEach-Object { Write-Host $_ }
}

$failed = $false
if ($srvOld) {
    Write-Host 'サーバーをビルドしています…'
    $log = Join-Path $logs 'server-build.log'
    $env:CARGO_HOME = Join-Path $srv '.build\cargo-home'
    New-Item -ItemType Directory -Force (Join-Path $srv '.build\tmp') | Out-Null
    $env:TMP = Join-Path $srv '.build\tmp'; $env:TEMP = $env:TMP
    $env:CARGO_TARGET_DIR = Join-Path $srv 'codex-rs\target-win'
    $env:CARGO_NET_GIT_FETCH_WITH_CLI = 'true'
    $env:CARGO_INCREMENTAL = '0'
    $env:RUST_MIN_STACK = '67108864'
    # The V8 files that are already in .build (nothing is downloaded here).
    $v8 = Join-Path $srv '.build\rusty_v8'
    $env:RUSTY_V8_ARCHIVE = Join-Path $v8 'rusty_v8_ptrcomp_sandbox_release_x86_64-pc-windows-msvc.lib.gz'
    $env:RUSTY_V8_SRC_BINDING_PATH = Join-Path $v8 'src_binding_ptrcomp_sandbox_release_x86_64-pc-windows-msvc.rs'
    if (-not ((Test-Path $env:RUSTY_V8_ARCHIVE) -and (Test-Path $env:RUSTY_V8_SRC_BINDING_PATH))) {
        Write-Host "V8 の成果物がありません: $v8" -ForegroundColor Red
        $failed = $true
    } else {
        Push-Location (Join-Path $srv 'codex-rs')
        # cargo writes its progress to stderr; that is not an error of the script.
        $ErrorActionPreference = 'Continue'
        try {
            & cargo build --profile dev-small -p codex-cli -p codex-code-mode-host -p codex-windows-sandbox --bin inocode --bin inocode-code-mode-host --bin inocode-command-runner --bin inocode-windows-sandbox-setup --bin inocode-windows-managed-deny-probe *> $log
            $code = $LASTEXITCODE
        } finally { Pop-Location; $ErrorActionPreference = 'Stop' }
        if ($code -ne 0) { Show-Failure 'サーバー' $log; $failed = $true }
        else { Set-Content -Path (Join-Path $srvOut '.launch-stamp') -Value $srvFp -Encoding ascii }
    }
}
if (-not $failed -and $guiOld) {
    Write-Host 'GUI をビルドしています…'
    $log = Join-Path $logs 'gui-build.log'
    Remove-Item Env:CARGO_TARGET_DIR, Env:CARGO_HOME, Env:CARGO_INCREMENTAL, Env:RUSTY_V8_ARCHIVE, Env:RUSTY_V8_SRC_BINDING_PATH -ErrorAction SilentlyContinue
    Push-Location $gui
    $ErrorActionPreference = 'Continue'
    try {
        & cargo build *> $log
        $code = $LASTEXITCODE
    } finally { Pop-Location; $ErrorActionPreference = 'Stop' }
    if ($code -ne 0) { Show-Failure 'GUI' $log; $failed = $true }
    else { Set-Content -Path (Join-Path $guiOut '.launch-stamp') -Value $guiFp -Encoding ascii }
}

# The builds are copied to a folder of their own (a running one keeps its files).
function Publish([string]$kind, [string]$fp, [string]$from, [string[]]$files) {
    $dest = Join-Path $run ("$kind-" + $fp.Substring(0, 12))
    if (-not (Test-Path (Join-Path $dest $files[0]))) {
        New-Item -ItemType Directory -Force $dest | Out-Null
        foreach ($f in $files) { Copy-Item (Join-Path $from $f) (Join-Path $dest $f) -Force }
    }
    (Get-Item $dest).LastWriteTime = Get-Date
    $dest
}

function Latest([string]$kind) {
    Get-ChildItem $run -Directory -Filter "$kind-*" -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending | Select-Object -First 1
}

if ($failed) {
    $g = Latest 'gui'; $s = Latest 'server'
    if (-not $g -or -not $s) { exit 1 }
    Write-Host ("前のビルド(GUI {0:MM/dd HH:mm}、サーバー {1:MM/dd HH:mm})は古いものです" -f $g.LastWriteTime, $s.LastWriteTime) -ForegroundColor Yellow
    if ((Read-Host '前のビルドで起動しますか? [y/N]') -notmatch '^[yY]') { exit 1 }
    $guiDir = $g.FullName; $srvDir = $s.FullName
} else {
    $guiDir = Publish 'gui' $guiFp $guiOut @('inocode-desktop.exe')
    $srvDir = Publish 'server' $srvFp $srvOut $serverExes
    # Copies that are no longer the newest two go; one that runs cannot be removed and stays.
    foreach ($kind in 'gui', 'server') {
        Get-ChildItem $run -Directory -Filter "$kind-*" | Sort-Object LastWriteTime -Descending | Select-Object -Skip 2 | ForEach-Object {
            try { Remove-Item $_.FullName -Recurse -Force -ErrorAction Stop } catch { }
        }
    }
}

if ($NoStart) { return }
$env:INOCODE_BIN = Join-Path $srvDir 'inocode.exe'
Start-Process (Join-Path $guiDir 'inocode-desktop.exe') -WorkingDirectory $gui
