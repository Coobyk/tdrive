[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Version
)

$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$targetExe = Join-Path $repoRoot 'target\x86_64-pc-windows-msvc\release\tdrive.exe'
if (-not (Test-Path -LiteralPath $targetExe)) {
    throw "Built executable not found: $targetExe"
}

$stage = Join-Path $repoRoot 'dist\windows-packaging'
$payload = Join-Path $stage 'payload'
New-Item -ItemType Directory -Force -Path $payload | Out-Null
Copy-Item -LiteralPath $targetExe -Destination (Join-Path $payload 'tdrive.exe') -Force

$zipPath = Join-Path $repoRoot "tdrive-windows-x86_64-$Version.zip"
Compress-Archive -LiteralPath (Join-Path $payload 'tdrive.exe') -DestinationPath $zipPath -Force

$wxsTemplate = Join-Path $PSScriptRoot 'tdrive.wxs'
$wxsPath = Join-Path $stage 'tdrive.wxs'
$wxsContent = (Get-Content -LiteralPath $wxsTemplate -Raw).Replace('@VERSION@', $Version)
[System.IO.File]::WriteAllText($wxsPath, $wxsContent)

$wix = Join-Path $env:USERPROFILE '.dotnet\tools\wix.exe'
if (-not (Test-Path -LiteralPath $wix)) {
    $wixCommand = Get-Command wix.exe -ErrorAction SilentlyContinue
    if ($null -eq $wixCommand) {
        throw 'wix.exe not found; install it with: dotnet tool install --global wix --version 4.0.6'
    }
    $wix = $wixCommand.Source
}

$msiPath = Join-Path $repoRoot "tdrive-windows-x86_64-$Version.msi"
$env:DOTNET_ROLL_FORWARD = 'Major'
& $wix build -arch x64 -pdbtype none -b $stage -o $msiPath $wxsPath
if ($LASTEXITCODE -ne 0) {
    throw "wix build failed with exit code $LASTEXITCODE"
}

$makensis = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe'
if (-not (Test-Path -LiteralPath $makensis)) {
    $makensisCommand = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($null -eq $makensisCommand) {
        throw 'makensis.exe not found; install NSIS first'
    }
    $makensis = $makensisCommand.Source
}

$setupPath = Join-Path $repoRoot "tdrive-windows-x86_64-setup-$Version.exe"
& $makensis "/DAPPVERSION=$Version" (Join-Path $PSScriptRoot 'tdrive.nsi')
if ($LASTEXITCODE -ne 0) {
    throw "NSIS compilation failed with exit code $LASTEXITCODE"
}

foreach ($artifact in @($zipPath, $msiPath, $setupPath)) {
    if (-not (Test-Path -LiteralPath $artifact)) {
        throw "Expected artifact was not created: $artifact"
    }
    $item = Get-Item -LiteralPath $artifact
    Write-Output ("created {0} ({1:N1} MiB)" -f $item.FullName, ($item.Length / 1MB))
}
