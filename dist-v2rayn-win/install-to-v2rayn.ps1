[CmdletBinding()]
param(
    [string]$V2rayNDir,
    [string]$Server,
    [int]$Port = 27015,
    [string]$PSK,
    [switch]$UseClientName,
    [switch]$Force
)

$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$sourceExe = Join-Path $scriptDir "sing-box.exe"
if (-not (Test-Path -LiteralPath $sourceExe)) {
    Write-Error "Missing source binary: $sourceExe"
    exit 1
}

function Resolve-SingBoxBinDir {
    param([string]$InputDir)

    $cleaned = $InputDir.Trim().Trim('"')
    if (-not (Test-Path -LiteralPath $cleaned)) {
        return $null
    }

    $item = Get-Item -LiteralPath $cleaned
    if (-not $item.PSIsContainer) {
        $cleaned = $item.DirectoryName
    }

    $leaf = Split-Path -Leaf $cleaned
    if ($leaf -ieq "sing_box") {
        return $cleaned
    }
    if ($leaf -ieq "bin") {
        return (Join-Path $cleaned "sing_box")
    }

    $candidate = Join-Path $cleaned "bin\sing_box"
    if ((Test-Path -LiteralPath (Join-Path $cleaned "v2rayN.exe")) -or (Test-Path -LiteralPath (Join-Path $cleaned "bin"))) {
        return $candidate
    }

    return $candidate
}

function Find-V2rayNCandidates {
    $candidates = New-Object System.Collections.Generic.List[string]

    $running = Get-Process -Name "v2rayN", "sing-box", "sing-box-client" -ErrorAction SilentlyContinue
    foreach ($proc in $running) {
        try {
            if ($proc.Path) {
                $dir = Split-Path -Parent $proc.Path
                if ($proc.Name -ieq "v2rayN") {
                    if (-not $candidates.Contains($dir)) {
                        $candidates.Add($dir)
                    }
                }
                elseif ((Split-Path -Leaf $dir) -ieq "sing_box") {
                    $parentBin = Split-Path -Parent $dir
                    $rootDir = Split-Path -Parent $parentBin
                    if (-not $candidates.Contains($rootDir)) {
                        $candidates.Add($rootDir)
                    }
                }
            }
        }
        catch {
        }
    }

    $probeRoots = @(
        (Join-Path $env:LOCALAPPDATA "v2rayN"),
        (Join-Path $env:APPDATA "v2rayN"),
        (Join-Path $env:USERPROFILE "Desktop"),
        (Join-Path $env:USERPROFILE "Downloads"),
        "C:\Program Files",
        "D:\",
        "E:\"
    )

    foreach ($root in $probeRoots) {
        if (-not (Test-Path -LiteralPath $root)) {
            continue
        }
        if (Test-Path -LiteralPath (Join-Path $root "v2rayN.exe")) {
            if (-not $candidates.Contains($root)) {
                $candidates.Add($root)
            }
        }
        try {
            $dirs = Get-ChildItem -LiteralPath $root -Directory -Filter "*v2rayN*" -ErrorAction SilentlyContinue
            foreach ($d in $dirs) {
                if ((Test-Path -LiteralPath (Join-Path $d.FullName "v2rayN.exe")) -or (Test-Path -LiteralPath (Join-Path $d.FullName "bin\sing_box"))) {
                    if (-not $candidates.Contains($d.FullName)) {
                        $candidates.Add($d.FullName)
                    }
                }
            }
        }
        catch {
        }
    }

    return $candidates
}

if ([string]::IsNullOrWhiteSpace($V2rayNDir)) {
    $detected = Find-V2rayNCandidates
    if ($detected.Count -gt 0) {
        Write-Host "Detected v2rayN installation(s):" -ForegroundColor Cyan
        for ($i = 0; $i -lt $detected.Count; $i++) {
            Write-Host ("  [{0}] {1}" -f ($i + 1), $detected[$i])
        }
        $prompt = Read-Host "Enter number [1-$($detected.Count)] or paste full v2rayN directory path"
        if ($prompt -match '^\d+$') {
            $idx = [int]$prompt - 1
            if ($idx -ge 0 -and $idx -lt $detected.Count) {
                $V2rayNDir = $detected[$idx]
            }
        }
        else {
            $V2rayNDir = $prompt
        }
    }
    else {
        $V2rayNDir = Read-Host "Enter your v2rayN installation directory (e.g. D:\Tools\v2rayN-windows-64)"
    }
}

if ([string]::IsNullOrWhiteSpace($V2rayNDir)) {
    Write-Error "v2rayN directory is required."
    exit 1
}

$targetBinDir = Resolve-SingBoxBinDir -InputDir $V2rayNDir
if ([string]::IsNullOrWhiteSpace($targetBinDir)) {
    Write-Error "Directory does not exist: $V2rayNDir"
    exit 1
}

if (-not (Test-Path -LiteralPath $targetBinDir)) {
    New-Item -ItemType Directory -Path $targetBinDir -Force | Out-Null
}

$targetName = if ($UseClientName) { "sing-box-client.exe" } else { "sing-box.exe" }
$targetExe = Join-Path $targetBinDir $targetName

# Stop any running sing-box processes originating from targetBinDir
$runningCores = Get-Process -Name "sing-box", "sing-box-client" -ErrorAction SilentlyContinue
foreach ($proc in $runningCores) {
    $procPath = ""
    try {
        $procPath = $proc.Path
    }
    catch {
    }
    if ($procPath -and ($procPath.StartsWith($targetBinDir, [System.StringComparison]::OrdinalIgnoreCase))) {
        if (-not $Force) {
            $ans = Read-Host "Running process '$($proc.Name)' (PID $($proc.Id)) found in $targetBinDir. Stop it now? [Y/n]"
            if ($ans -and $ans.Trim().ToLowerInvariant() -notin @("y", "yes")) {
                Write-Error "Aborted because core process is still running."
                exit 1
            }
        }
        Stop-Process -Id $proc.Id -Force
        Start-Sleep -Milliseconds 400
    }
}

# Backup existing target executable if present and not already backed up
if (Test-Path -LiteralPath $targetExe) {
    $backupExe = "$targetExe.bak"
    $isAlreadyCS2World = $false
    try {
        $verCheck = & $targetExe version 2>&1 | Out-String
        if ($verCheck -match "cs2world") {
            $isAlreadyCS2World = $true
        }
    }
    catch {
    }

    if (-not (Test-Path -LiteralPath $backupExe) -or (-not $isAlreadyCS2World)) {
        Copy-Item -LiteralPath $targetExe -Destination $backupExe -Force
        Write-Host "Backed up existing binary to: $backupExe" -ForegroundColor Yellow
    }
}

Copy-Item -LiteralPath $sourceExe -Destination $targetExe -Force
Write-Host "Installed cs2world sing-box kernel to: $targetExe" -ForegroundColor Green

$verOutput = & $targetExe version 2>&1 | Out-String
if ($LASTEXITCODE -ne 0 -or ($verOutput -notmatch "sing-box") -or ($verOutput -notmatch "cs2world")) {
    Write-Error "Kernel verification failed:`n$verOutput"
    exit 1
}
Write-Host "Verified kernel version:" -ForegroundColor Cyan
Write-Host $verOutput.Trim()

if ($UseClientName) {
    Write-Host ""
    Write-Host "Note: Installed as sing-box-client.exe. v2rayN prefers sing-box-client.exe over sing-box.exe and will not overwrite it on core updates." -ForegroundColor Cyan
}

if (-not [string]::IsNullOrWhiteSpace($Server) -and -not [string]::IsNullOrWhiteSpace($PSK)) {
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)

    $outboundObj = [ordered]@{
        type        = "cs2world"
        tag         = "proxy"
        server      = $Server
        server_port = $Port
        psk         = $PSK
        kcp         = [ordered]@{
            data_shards   = 10
            parity_shards = 3
            mtu           = 1180
            snd_wnd       = 1024
            rcv_wnd       = 1024
        }
        shaper      = [ordered]@{
            enabled            = $true
            tick_rate_hz       = 64
            idle_sleep_seconds = 600
            combat_mean_bytes  = 180
            fastdl_chunk_size  = 1248
        }
    }
    $outboundPath = Join-Path $scriptDir "my-cs2world-outbound.json"
    [System.IO.File]::WriteAllText($outboundPath, ($outboundObj | ConvertTo-Json -Depth 10), $utf8NoBom)

    $clientObj = [ordered]@{
        log       = [ordered]@{
            level     = "info"
            timestamp = $true
        }
        inbounds  = @(
            [ordered]@{
                type        = "mixed"
                tag         = "mixed-in"
                listen      = "127.0.0.1"
                listen_port = 7890
            }
        )
        outbounds = @(
            [ordered]@{
                type        = "cs2world"
                tag         = "cs2world-out"
                server      = $Server
                server_port = $Port
                psk         = $PSK
                kcp         = [ordered]@{
                    data_shards   = 10
                    parity_shards = 3
                    mtu           = 1180
                    snd_wnd       = 1024
                    rcv_wnd       = 1024
                }
                shaper      = [ordered]@{
                    enabled            = $true
                    tick_rate_hz       = 64
                    idle_sleep_seconds = 600
                    combat_mean_bytes  = 180
                    fastdl_chunk_size  = 1248
                }
            },
            [ordered]@{
                type = "direct"
                tag  = "direct"
            }
        )
        route     = [ordered]@{
            auto_detect_interface = $true
            rules                 = @(
                [ordered]@{
                    ip_is_private = $true
                    outbound      = "direct"
                }
            )
            final                 = "cs2world-out"
        }
    }
    $clientPath = Join-Path $scriptDir "my-cs2world-client.json"
    [System.IO.File]::WriteAllText($clientPath, ($clientObj | ConvertTo-Json -Depth 10), $utf8NoBom)

    $checkOutput = & $targetExe check -c $clientPath --disable-color 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Generated config failed validation:`n$checkOutput"
        exit 1
    }

    Write-Host ""
    Write-Host "Generated and validated ready-to-import configs:" -ForegroundColor Green
    Write-Host "  Custom Outbound (v2rayN 7.x): $outboundPath"
    Write-Host "  Custom Config   (v2rayN 6/7): $clientPath"
}
elseif ((-not [string]::IsNullOrWhiteSpace($Server)) -or (-not [string]::IsNullOrWhiteSpace($PSK))) {
    Write-Warning "Both -Server and -PSK are required to generate ready-to-import config files."
}

Write-Host ""
Write-Host "Done! Restart or reconnect in v2rayN to use the cs2world kernel." -ForegroundColor Green
