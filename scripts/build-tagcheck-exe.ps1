# Build YUNYIN-TagCheck.exe (the tag health-check GUI) from scripts/tagcheck.py.
#
# Needs Python 3.10+ with tkinter (the official Windows installer includes it):
#   python -m pip install --upgrade pyinstaller
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\build-tagcheck-exe.ps1
$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$src  = Join-Path $repo "scripts\tagcheck.py"
$icon = Join-Path $repo "tools\icon.ico"
$out  = Join-Path $repo "dist"

# 找 Python：先看 PATH，再看用户级安装目录
$py = $null
$cmd = Get-Command python -ErrorAction SilentlyContinue
if ($cmd) { $py = $cmd.Source }
if (-not $py) {
    foreach ($v in @("Python313", "Python312", "Python311", "Python310")) {
        $cand = Join-Path $env:LOCALAPPDATA "Programs\Python\$v\python.exe"
        if (Test-Path $cand) { $py = $cand; break }
    }
}
if (-not $py) {
    Write-Error "Python not found. Install Python 3.10+ (with tcl/tk) first, then rerun."
    exit 1
}
Write-Output ("python: " + $py)
& $py -c "import tkinter" 2>$null
if ($LASTEXITCODE -ne 0) {
    Write-Error "This Python has no tkinter (reinstall it with the 'tcl/tk' option checked)."
    exit 1
}

& $py -m PyInstaller --version 2>$null
if ($LASTEXITCODE -ne 0) {
    Write-Output "installing pyinstaller ..."
    & $py -m pip install --upgrade pyinstaller
}

$work = Join-Path $env:TEMP "tagcheck-build"
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
$args = @("-m", "PyInstaller", "--noconfirm", "--onefile", "--noconsole",
          "--name", "YUNYIN-TagCheck")
if (Test-Path $icon) { $args += @("--icon", $icon) }
$args += @("--distpath", (Join-Path $work "dist"), "--workpath", (Join-Path $work "build"),
           "--specpath", $work, $src)
& $py @args

$exe = Join-Path $work "dist\YUNYIN-TagCheck.exe"
if (-not (Test-Path $exe)) { Write-Error "build failed"; exit 1 }
New-Item -ItemType Directory -Force -Path $out | Out-Null
Copy-Item $exe (Join-Path $out "YUNYIN-TagCheck.exe") -Force
Write-Output ("built: " + (Join-Path $out "YUNYIN-TagCheck.exe") + "  (" +
              [Math]::Round((Get-Item $exe).Length / 1MB, 1) + " MB)")
