# PhuketBuddy 构建脚本
$ErrorActionPreference = 'Stop'

$root = 'G:\Project'
$env:CARGO_HOME  = "$root\.rust\cargo"
$env:RUSTUP_HOME = "$root\.rust\rustup"
$env:PATH        = "$root\.rust\cargo\bin;" + $env:PATH
$env:TEMP        = "$root\.tmp"
$env:TMP         = "$root\.tmp"
New-Item -ItemType Directory -Force $env:TEMP | Out-Null

Push-Location "$root\PhuketBuddy\src-tauri"
try {
    cargo build --release
    $exe = "target\release\phuketbuddy.exe"
    if (Test-Path $exe) {
        $size = [math]::Round((Get-Item $exe).Length / 1MB, 2)
        Write-Host "`n构建成功：$((Resolve-Path $exe).Path)  ($size MB)" -ForegroundColor Green
        Copy-Item $exe "$root\PhuketBuddy\PhuketBuddy.exe" -Force
        Write-Host "已复制到：$root\PhuketBuddy\PhuketBuddy.exe" -ForegroundColor Green
    }
} finally {
    Pop-Location
}
