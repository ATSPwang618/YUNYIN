# Build YUNYIN-TagCheck.exe (the tag health-check GUI) from scripts/tagcheck.py.
#
# Needs Python 3.10+ with tkinter (the official Windows installer includes it):
#   python -m pip install --upgrade pyinstaller mutagen
# (mutagen is only needed for the "online match & fix" feature; the exe bundles it)
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\build-tagcheck-exe.ps1
$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$src  = Join-Path $repo "scripts\tagcheck.py"
$icon = Join-Path $repo "tools\icon.ico"
$out  = Join-Path $repo "dist"

# 找 Python：PATH 和用户级安装目录都看一遍，优先挑「tkinter + mutagen + pyinstaller
# 都齐」的那个（机器上可能装了好几套，库只装在其中一个里）。
$candidates = @()
foreach ($name in @("python", "python3")) {
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if ($cmd) { $candidates += $cmd.Source }
}
foreach ($v in @("Python313", "Python312", "Python311", "Python310")) {
    $cand = Join-Path $env:LOCALAPPDATA "Programs\Python\$v\python.exe"
    if (Test-Path $cand) { $candidates += $cand }
}
$candidates = $candidates | Select-Object -Unique

# 探测某个 Python 能不能 import 指定模块（stderr 要吞掉，PS 5.1 会把它当错）
function Test-PythonModule([string]$exe, [string]$code) {
    $prev = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        & $exe -c $code 2>$null | Out-Null
        return ($LASTEXITCODE -eq 0)
    } finally {
        $ErrorActionPreference = $prev
    }
}

$py = $null
$fallback = $null
foreach ($cand in $candidates) {
    if (-not (Test-PythonModule $cand "import tkinter")) { continue }   # 没有 tkinter 的跳过
    if (-not $fallback) { $fallback = $cand }
    if (Test-PythonModule $cand "import mutagen, PyInstaller") { $py = $cand; break }
}
if (-not $py) { $py = $fallback }
if (-not $py) {
    Write-Error "Python not found. Install Python 3.10+ (with tcl/tk) first, then rerun."
    exit 1
}
Write-Output ("python: " + $py)
if (-not (Test-PythonModule $py "import tkinter")) {
    Write-Error "This Python has no tkinter (reinstall it with the 'tcl/tk' option checked)."
    exit 1
}

if (-not (Test-PythonModule $py "import PyInstaller")) {
    Write-Output "installing pyinstaller ..."
    & $py -m pip install --upgrade pyinstaller
}
if (-not (Test-PythonModule $py "import mutagen")) {
    Write-Output "installing mutagen (needed for the online-fix feature) ..."
    & $py -m pip install --upgrade mutagen
}

$work = Join-Path $env:TEMP "tagcheck-build"
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
# 注意：变量别叫 $args —— 那是 PowerShell 的自动变量，追加参数会失效
$pyArgs = @("-m", "PyInstaller", "--noconfirm", "--onefile", "--noconsole",
            "--name", "YUNYIN-TagCheck")
if (Test-Path $icon) { $pyArgs += @("--icon", $icon) }
# mutagen 的子模块是在函数里 import 的，显式声明一下，避免打包漏掉
foreach ($m in @("mutagen", "mutagen.id3", "mutagen.flac", "mutagen.oggvorbis",
                 "mutagen.oggopus", "mutagen.easyid3")) {
    $pyArgs += @("--hidden-import", $m)
}
$pyArgs += @("--distpath", (Join-Path $work "dist"), "--workpath", (Join-Path $work "build"),
             "--specpath", $work, $src)
Write-Output ("running: " + $py + " " + ($pyArgs -join " "))
& $py @pyArgs

$exe = Join-Path $work "dist\YUNYIN-TagCheck.exe"
if (-not (Test-Path $exe)) { Write-Error "build failed"; exit 1 }
New-Item -ItemType Directory -Force -Path $out | Out-Null
Copy-Item $exe (Join-Path $out "YUNYIN-TagCheck.exe") -Force
Write-Output ("built: " + (Join-Path $out "YUNYIN-TagCheck.exe") + "  (" +
              [Math]::Round((Get-Item $exe).Length / 1MB, 1) + " MB)")
