param([string]$OutputDirectory = "target/dist", [string]$BuildTargetDirectory = "target")
$ErrorActionPreference = "Stop"
$workspaceRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Push-Location $workspaceRoot
try {
    cargo build --locked --release --target-dir $BuildTargetDirectory -p vrp-server -p vrp-cli
    if ($LASTEXITCODE -ne 0) { throw "Release build failed" }
    $packageName = "vrp-1-windows-$([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLower())-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
    $packageRoot = Join-Path $OutputDirectory $packageName
    New-Item -ItemType Directory -Path $packageRoot -Force | Out-Null
    $releaseDirectory = Join-Path $BuildTargetDirectory "release"
    Copy-Item -LiteralPath (Join-Path $releaseDirectory "vrp-server.exe"), (Join-Path $releaseDirectory "vrp.exe"), "README.md", "README.zh-CN.md", "LICENSE" -Destination $packageRoot
    Copy-Item -LiteralPath "docs", "proto" -Destination $packageRoot -Recurse
    $vendorRoot = Join-Path $packageRoot "web/vendor"
    $noticesRoot = Join-Path $vendorRoot "basemap"
    New-Item -ItemType Directory -Path $noticesRoot, (Join-Path $noticesRoot "fonts"), (Join-Path $noticesRoot "sprites") -Force | Out-Null
    Copy-Item -LiteralPath "web/vendor/maplibre-LICENSE.txt", "web/vendor/pmtiles-LICENSE.txt" -Destination $vendorRoot
    Copy-Item -LiteralPath "web/vendor/basemap/NOTICE.md", "web/vendor/basemap/README.md", "web/vendor/basemap/manifest.json" -Destination $noticesRoot
    Copy-Item -LiteralPath "web/vendor/basemap/fonts/OFL.txt", "web/vendor/basemap/fonts/OFL-CJK.txt" -Destination (Join-Path $noticesRoot "fonts")
    Copy-Item -LiteralPath "web/vendor/basemap/sprites/LICENSE.md" -Destination (Join-Path $noticesRoot "sprites")
    $launcher = '@echo off' + "`r`n" + 'cd /d "%~dp0"' + "`r`n" + 'vrp-server.exe --data-dir var %*' + "`r`n"
    [System.IO.File]::WriteAllText((Join-Path (Resolve-Path $packageRoot).Path "start.cmd"), $launcher, [System.Text.Encoding]::ASCII)
    $archivePath = "$packageRoot.zip"
    Compress-Archive -LiteralPath $packageRoot -DestinationPath $archivePath
    Write-Output (Resolve-Path $archivePath).Path
} finally { Pop-Location }
