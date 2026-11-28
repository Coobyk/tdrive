[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Version
)

$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$coreDll = Join-Path $repoRoot 'windows\core\target\release\tdrive_core.dll'
if (-not (Test-Path -LiteralPath $coreDll)) {
    throw "Built tdrive_core.dll not found: $coreDll (run cargo build --release --manifest-path windows/core/Cargo.toml)"
}

$stage = Join-Path $repoRoot 'dist\windows-winui-stage'
if (Test-Path -LiteralPath $stage) {
    Remove-Item -LiteralPath $stage -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $stage | Out-Null

$csproj = Join-Path $repoRoot 'windows\TDrive.WinUI\TDrive.WinUI.csproj'
& dotnet publish $csproj -c Release -p:Platform=x64 "-p:Version=$Version" -o $stage
if ($LASTEXITCODE -ne 0) {
    throw "dotnet publish failed with exit code $LASTEXITCODE"
}

Copy-Item -LiteralPath $coreDll -Destination (Join-Path $stage 'tdrive_core.dll') -Force

$cliExe = Join-Path $repoRoot 'target\x86_64-pc-windows-msvc\release\tdrive.exe'
if (Test-Path -LiteralPath $cliExe) {
    Copy-Item -LiteralPath $cliExe -Destination (Join-Path $stage 'tdrive.exe') -Force
}

$zipPath = Join-Path $repoRoot "tdrive-windows-x86_64-winui-$Version.zip"
if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
}
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zipPath -Force

$makensis = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe'
if (-not (Test-Path -LiteralPath $makensis)) {
    $makensisCommand = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($null -eq $makensisCommand) {
        throw 'makensis.exe not found; install NSIS first'
    }
    $makensis = $makensisCommand.Source
}

$setupPath = Join-Path $repoRoot "tdrive-windows-x86_64-winui-setup-$Version.exe"
& $makensis "/DAPPVERSION=$Version" (Join-Path $PSScriptRoot 'tdrive-winui.nsi')
if ($LASTEXITCODE -ne 0) {
    throw "NSIS compilation failed with exit code $LASTEXITCODE"
}

foreach ($artifact in @($zipPath, $setupPath)) {
    if (-not (Test-Path -LiteralPath $artifact)) {
        throw "Expected artifact was not created: $artifact"
    }
    $item = Get-Item -LiteralPath $artifact
    Write-Output ("created {0} ({1:N1} MiB)" -f $item.FullName, ($item.Length / 1MB))
}
