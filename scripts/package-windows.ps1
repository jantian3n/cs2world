[CmdletBinding()]
param(
    [string]$Destination = (Join-Path $PSScriptRoot "../dist/rust/windows"),
    [string]$Target = "x86_64-pc-windows-msvc",
    [switch]$IncludeService
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location -LiteralPath $repo
try {
    cargo build --release --locked --target $Target -p cs2world-xp
    if ($LASTEXITCODE -ne 0) { throw "Windows client build failed ($LASTEXITCODE)" }
    $cargoTarget = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }
    $binary = Join-Path $cargoTarget "$Target/release/cs2world-xp.exe"
    $folder = [IO.Directory]::CreateDirectory($Destination).FullName
    $result = Join-Path $folder 'CS2World-XP.exe'
    Copy-Item -LiteralPath $binary -Destination $result -Force
    [pscustomobject]@{ Path = $result; Bytes = (Get-Item -LiteralPath $result).Length; SHA256 = (Get-FileHash -LiteralPath $result).Hash }
    if ($IncludeService) {
        cargo build --release --locked --target $Target -p cs2world-server --features windows-service
        if ($LASTEXITCODE -ne 0) { throw "Windows service build failed ($LASTEXITCODE)" }
        $serviceFolder = [IO.Directory]::CreateDirectory((Join-Path (Split-Path -Parent $folder) 'windows-service'))
        $serviceResult = Join-Path $serviceFolder.FullName 'cs2world-server.exe'
        Copy-Item -LiteralPath (Join-Path $cargoTarget "$Target/release/cs2world-server.exe") -Destination $serviceResult -Force
        [pscustomobject]@{ Path = $serviceResult; Bytes = (Get-Item -LiteralPath $serviceResult).Length; SHA256 = (Get-FileHash -LiteralPath $serviceResult).Hash }
    }
} finally {
    Pop-Location
}
