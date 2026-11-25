<#
.SYNOPSIS
    SpicaRustNative v2.5 Windows x86_64 build script
    Derler:
      1. spica_rust_native.dll        (In-Process Rust Brain Engine - Spatial Hash + Rayon + Governor)
      2. spica-launcher.exe      (Rust Startup Orchestrator, Supervisor & Brain IPC/HTTP Daemon)
      3. SpicaRustNative-2.5.0.jar (Purpur 1.21.4 Plugin, .dll gomulu)
#>

param(
    [switch]$SkipTests,
    [switch]$RunBench
)

$ErrorActionPreference = "Stop"

$RootDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$PluginDir = Join-Path $RootDir "spica-rust-native-plugin"
$NativeOutDir = Join-Path $PluginDir "src\main\resources\native\windows-x86_64"
$DistDir = Join-Path $RootDir "dist\windows-x86_64"

function Assert-Tool($name, $hint) {
    if (-not (Get-Command $name -ErrorAction SilentlyContinue)) {
        Write-Error "'$name' bulunamadi! Kurulum ipucu: $hint"
        exit 1
    }
}

Assert-Tool "cargo" "winget install Rustlang.Rustup"
Assert-Tool "mvn"   "winget install EclipseAdoptium.Temurin.21.JDK && winget install Apache.Maven"

Write-Host "`n[1/4] Rust Workspace derleniyor (spica-native .dll + spica-launcher .exe)..." -ForegroundColor Cyan
Push-Location $RootDir
try {
    $env:RUSTFLAGS = "-C target-cpu=native"
    if (-not $SkipTests) {
        cargo test --workspace --release
    }
    cargo build --workspace --release
    if ($RunBench) {
        Write-Host "`n[Bench] Standalone Rust Brain benchmark calistiriliyor..." -ForegroundColor Yellow
        cargo run --release -p spica-rust-native --bin bench
    }
} finally {
    Remove-Item Env:\RUSTFLAGS -ErrorAction SilentlyContinue
    Pop-Location
}

Write-Host "`n[2/4] spica_rust_native.dll plugin kaynaklarina kopyalaniyor..." -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path $NativeOutDir | Out-Null
$DllPath = Join-Path $RootDir "target\release\spica_rust_native.dll"
$LauncherPath = Join-Path $RootDir "target\release\spica-launcher.exe"
Copy-Item -Force $DllPath (Join-Path $NativeOutDir "spica_rust_native.dll")

Write-Host "`n[3/4] Purpur 1.21.4 Java 21 eklentisi paketleniyor (Maven)..." -ForegroundColor Cyan
Push-Location $PluginDir
try {
    mvn -B clean package
    if ($LASTEXITCODE -ne 0) { throw "Maven build failed with exit code $LASTEXITCODE" }
} finally {
    Pop-Location
}

Write-Host "`n[4/4] Ciktilar dist/windows-x86_64 klasorune toplaniyor..." -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
$JarPath = Join-Path $PluginDir "target\SpicaRustNative-2.5.0.jar"
Copy-Item -Force $JarPath (Join-Path $DistDir "SpicaRustNative-2.5.0.jar")
Copy-Item -Force $LauncherPath (Join-Path $DistDir "spica-launcher.exe")
Copy-Item -Force $DllPath (Join-Path $DistDir "spica_rust_native.dll")

Write-Host "`nTAMAMLANDI!" -ForegroundColor Green
Write-Host "  Rust Launcher : $DistDir\spica-launcher.exe"
Write-Host "  Purpur Plugin : $DistDir\SpicaRustNative-2.5.0.jar"
Write-Host "  Native Engine : $DistDir\spica_rust_native.dll"
Write-Host "`nKullanim:" -ForegroundColor Yellow
Write-Host "  1. SpicaRustNative-2.5.0.jar -> Purpur 1.21.4 sunucunun plugins/ klasorune at"
Write-Host "  2. spica-launcher.exe        -> purpur-1.21.4.jar yanina koy ve calistir"
