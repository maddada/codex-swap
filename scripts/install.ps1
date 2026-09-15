# Install or upgrade native Codex Swap without Rust, Cargo, or administrator access.
[CmdletBinding()]
param(
    [string]$Version,
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\codex-swap'),
    [switch]$NoPathUpdate
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

if ($env:OS -ne 'Windows_NT') { throw 'This installer requires native Windows.' }
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
$repository = 'maddada/codex-swap'
$headers = @{ 'User-Agent' = 'codex-swap-installer'; 'Accept' = 'application/vnd.github+json' }
# PROCESSOR_ARCHITEW6432 identifies the host when invoked from a 32-bit shell.
$architecture = $env:PROCESSOR_ARCHITEW6432
if (-not $architecture) { $architecture = $env:PROCESSOR_ARCHITECTURE }
$target = switch ($architecture.ToUpperInvariant()) {
    'AMD64' { 'x86_64-pc-windows-msvc' }
    'ARM64' { 'aarch64-pc-windows-msvc' }
    default { throw "Unsupported Windows architecture: $architecture. x64 or ARM64 is required." }
}
if ($Version) {
    $tag = 'v' + $Version.TrimStart('v')
    if ($tag -notmatch '^v\d+\.\d+\.\d+$') { throw 'Version must be MAJOR.MINOR.PATCH, optionally prefixed with v.' }
    $release = Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$repository/releases/tags/$tag"
} else {
    $release = Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$repository/releases/latest"
    $tag = $release.tag_name
}
if ($tag -notmatch '^v\d+\.\d+\.\d+$') { throw "Unsupported release tag: $tag" }
if ($release.draft -or $release.prerelease) { throw 'Only published stable releases can be installed.' }
$versionNumber = $tag.Substring(1)
$archiveName = "codex-swap-$versionNumber-$target.zip"
$baseUrl = "https://github.com/$repository/releases/download/$tag"
$assetNames = @($release.assets | ForEach-Object { $_.name })
if ($archiveName -notin $assetNames -or 'SHA256SUMS' -notin $assetNames) {
    throw "Release $tag does not contain a verified $target binary. Nothing was installed."
}
$tempDir = Join-Path ([IO.Path]::GetTempPath()) ('codex-swap-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($tempDir) | Out-Null
$destination = Join-Path $InstallDir 'xswap.exe'
$previous = $null
$staged = $null
$installedFiles = @('xswap.exe', 'LICENSE', 'THIRD_PARTY_NOTICES.md')

function Remove-PreviousInstallation([string]$Directory) {
    if (Test-Path -LiteralPath (Join-Path $Directory 'INSTALLATION_INCOMPLETE')) {
        Write-Verbose "Recovery files are retained: $Directory"
        return
    }
    try {
        # Keep the old notices until its executable can be removed.
        $oldBinary = Join-Path $Directory 'xswap.exe'
        if (Test-Path -LiteralPath $oldBinary) { Remove-Item -LiteralPath $oldBinary -Force }
        Remove-Item -LiteralPath $Directory -Recurse -Force
    } catch {
        Write-Verbose "An earlier installation is still in use: $Directory"
    }
}

try {
    $archivePath = Join-Path $tempDir $archiveName
    Invoke-WebRequest -UseBasicParsing -Headers $headers -Uri "$baseUrl/$archiveName" -OutFile $archivePath
    $checksumsPath = Join-Path $tempDir 'SHA256SUMS'
    Invoke-WebRequest -UseBasicParsing -Headers $headers -Uri "$baseUrl/SHA256SUMS" -OutFile $checksumsPath
    $checksumLines = @(Get-Content -LiteralPath $checksumsPath | Where-Object { $_ -match ('^[0-9a-fA-F]{64}  ' + [regex]::Escape($archiveName) + '$') })
    if ($checksumLines.Count -ne 1) { throw 'Release checksum list does not contain exactly one matching archive.' }
    $expected = $checksumLines[0].Substring(0, 64)
    $actual = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash
    if ($actual -ne $expected) { throw 'SHA-256 verification failed. Nothing was installed.' }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archivePath)
    try {
        $expectedEntries = @('xswap.exe', 'LICENSE', 'README.md', 'THIRD_PARTY_NOTICES.md')
        $entries = @($zip.Entries | ForEach-Object { $_.FullName })
        if ($entries.Count -ne 4 -or @(Compare-Object $expectedEntries $entries -CaseSensitive).Count -ne 0) {
            throw 'Unexpected release archive contents.'
        }
        foreach ($entry in $zip.Entries) {
            $fileType = ($entry.ExternalAttributes -shr 16) -band 0xF000
            if (($entry.ExternalAttributes -band 0x10) -ne 0 -or $fileType -notin @(0, 0x8000) -or $entry.Length -le 0) {
                throw 'Release archive must contain nonempty regular files.'
            }
            if ($entry.FullName -in $installedFiles) {
                [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $tempDir $entry.FullName), $false)
            }
        }
    } finally { $zip.Dispose() }
    $downloaded = Join-Path $tempDir 'xswap.exe'
    $reportedVersion = & $downloaded --version
    if ($LASTEXITCODE -ne 0 -or $reportedVersion -ne "xswap $versionNumber") {
        throw 'Downloaded executable did not pass its version check.'
    }

    [IO.Directory]::CreateDirectory($InstallDir) | Out-Null
    # Earlier upgrades can leave an executable that was still running at replacement time.
    foreach ($old in @(Get-ChildItem -LiteralPath $InstallDir -Filter 'xswap.*.previous.exe' -File)) {
        try { Remove-Item -LiteralPath $old.FullName -Force } catch {
            Write-Verbose "An earlier executable is still in use: $($old.Name)"
        }
    }
    foreach ($old in @(Get-ChildItem -LiteralPath $InstallDir -Filter 'xswap.*.previous' -Directory)) {
        Remove-PreviousInstallation $old.FullName
    }
    $transaction = [Guid]::NewGuid().ToString('N')
    $staged = Join-Path $InstallDir "xswap.$transaction.new"
    $previous = Join-Path $InstallDir "xswap.$transaction.previous"
    [IO.Directory]::CreateDirectory($staged) | Out-Null
    foreach ($name in $installedFiles) {
        Copy-Item -LiteralPath (Join-Path $tempDir $name) -Destination (Join-Path $staged $name)
    }
    [IO.Directory]::CreateDirectory($previous) | Out-Null
    $recoveryMarker = Join-Path $previous 'INSTALLATION_INCOMPLETE'
    [IO.File]::WriteAllText($recoveryMarker, '')
    $retired = @()
    $installed = @()
    try {
        foreach ($name in $installedFiles) {
            $current = Join-Path $InstallDir $name
            if (Test-Path -LiteralPath $current) {
                # Windows permits renaming a running executable, but cannot overwrite its image.
                Move-Item -LiteralPath $current -Destination (Join-Path $previous $name)
                $retired += $name
            }
        }
        foreach ($name in $installedFiles) {
            Move-Item -LiteralPath (Join-Path $staged $name) -Destination (Join-Path $InstallDir $name)
            $installed += $name
        }
    } catch {
        $installError = $_
        $rollbackErrors = @()
        foreach ($name in $installed) {
            try { Remove-Item -LiteralPath (Join-Path $InstallDir $name) -Force } catch { $rollbackErrors += $_ }
        }
        foreach ($name in $retired) {
            try { Move-Item -LiteralPath (Join-Path $previous $name) -Destination (Join-Path $InstallDir $name) } catch { $rollbackErrors += $_ }
        }
        if ($rollbackErrors.Count) {
            throw "Installation failed: $installError. Rollback could not complete; previous files are retained in $previous. $rollbackErrors"
        }
        [IO.File]::Delete($recoveryMarker)
        Remove-PreviousInstallation $previous
        throw
    }
    [IO.File]::Delete($recoveryMarker)
    Remove-PreviousInstallation $previous

    if (-not $NoPathUpdate) {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $pathEntries = @($userPath -split ';' | Where-Object { $_ })
        if ($InstallDir.TrimEnd('\') -notin @($pathEntries | ForEach-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') })) {
            $newPath = (@($InstallDir) + $pathEntries) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        }
        if ($InstallDir.TrimEnd('\') -notin @($env:Path -split ';' | ForEach-Object { $_.TrimEnd('\') })) {
            $env:Path = "$InstallDir;$env:Path"
        }
    }
    Write-Host "Installed xswap $versionNumber to $destination"
    Write-Host 'Open a new terminal to use the updated PATH. Install the official Codex CLI separately.'
} finally {
    if ($staged -and (Test-Path -LiteralPath $staged)) { Remove-Item -LiteralPath $staged -Recurse -Force }
    Remove-Item -LiteralPath $tempDir -Recurse -Force
}
