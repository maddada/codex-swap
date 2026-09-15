# Native installer regression checks: all downloads, installation directories and failures are disposable.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Binary)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT') { throw 'Run these checks on native Windows.' }
$root = Split-Path $PSScriptRoot -Parent
$installer = Join-Path $PSScriptRoot 'install.ps1'
$Binary = (Resolve-Path -LiteralPath $Binary).Path
$version = (& $Binary --version) -replace '^xswap ', ''
if ($LASTEXITCODE -ne 0 -or $version -notmatch '^\d+\.\d+\.\d+$') { throw 'Invalid fixture binary.' }
$architecture = $env:PROCESSOR_ARCHITEW6432
if (-not $architecture) { $architecture = $env:PROCESSOR_ARCHITECTURE }
$target = if ($architecture -eq 'ARM64') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
$archiveName = "codex-swap-$version-$target.zip"
$testDirectory = Join-Path ([IO.Path]::GetTempPath()) ('xswap-installer-test-' + [Guid]::NewGuid().ToString('N'))
$installDirectory = Join-Path $testDirectory 'installation with spaces'
# Mocks invoked by another script need state anchored to this disposable test process.
$global:XswapInstallerTest = [pscustomobject]@{
    archivePath = (Join-Path $testDirectory $archiveName)
    installDirectory = $installDirectory
    downloadDirectory = $null; failure = ''; rollbackFailure = $false; badChecksum = $false
    version = $version; archiveName = $archiveName
}
$processPath = $env:Path
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
[IO.Directory]::CreateDirectory($testDirectory) | Out-Null
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Assert-Condition($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Invoke-RestMethod {
    param($Headers, [string]$Uri)
    Assert-Condition ($Uri.StartsWith('https://api.github.com/repos/maddada/codex-swap/releases/')) 'Unexpected API request.'
    return [pscustomobject]@{
        tag_name = ("v" + $global:XswapInstallerTest.version); draft = $false; prerelease = $false
        assets = @([pscustomobject]@{ name = $global:XswapInstallerTest.archiveName }, [pscustomobject]@{ name = 'SHA256SUMS' })
    }
}

function Invoke-WebRequest {
    param([switch]$UseBasicParsing, $Headers, [string]$Uri, [string]$OutFile)
    $global:XswapInstallerTest.downloadDirectory = Split-Path $OutFile -Parent
    if ($Uri.EndsWith("/" + $global:XswapInstallerTest.archiveName)) {
        [IO.File]::Copy($global:XswapInstallerTest.archivePath, $OutFile)
    } elseif ($Uri.EndsWith('/SHA256SUMS')) {
        $digest = (Get-FileHash -LiteralPath $global:XswapInstallerTest.archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($global:XswapInstallerTest.badChecksum) { $digest = '0' * 64 }
        [IO.File]::WriteAllText($OutFile, "$digest  $($global:XswapInstallerTest.archiveName)`n")
    } else { throw "Unexpected download request: $Uri" }
}

function Copy-Item {
    [CmdletBinding()]
    param([string]$LiteralPath, [string]$Destination)
    $name = [IO.Path]::GetFileName($LiteralPath)
    if ($global:XswapInstallerTest.failure -eq ('stage-' + $name)) { throw 'Injected staging failure.' }
    if ($global:XswapInstallerTest.rollbackFailure -and (Split-Path $LiteralPath -Parent).EndsWith('.previous') -and $name -eq 'xswap.exe') {
        throw 'Injected rollback failure.'
    }
    Microsoft.PowerShell.Management\Copy-Item @PSBoundParameters
}

function Move-Item {
    [CmdletBinding()]
    param([string]$LiteralPath, [string]$Destination)
    $name = [IO.Path]::GetFileName($LiteralPath)
    $sourceDirectory = Split-Path $LiteralPath -Parent
    $destinationDirectory = Split-Path $Destination -Parent
    if (($sourceDirectory.EndsWith('.new') -and $global:XswapInstallerTest.failure -eq "install-$name") -or
        ($destinationDirectory.EndsWith('.previous') -and $global:XswapInstallerTest.failure -eq "retire-$name")) {
        throw 'Injected replacement failure.'
    }
    Microsoft.PowerShell.Management\Move-Item @PSBoundParameters
}

function Remove-Item {
    [CmdletBinding()]
    param([string]$LiteralPath, [switch]$Force, [switch]$Recurse)
    $name = [IO.Path]::GetFileName($LiteralPath)
    if ($global:XswapInstallerTest.failure -eq "retire-$name" -and
        (Split-Path $LiteralPath -Parent) -eq $global:XswapInstallerTest.installDirectory) {
        throw 'Injected replacement failure.'
    }
    Microsoft.PowerShell.Management\Remove-Item @PSBoundParameters
}

function New-Archive([string]$Fault = '') {
    if (Test-Path -LiteralPath $global:XswapInstallerTest.archivePath) { [IO.File]::Delete($global:XswapInstallerTest.archivePath) }
    $zip = [IO.Compression.ZipFile]::Open($global:XswapInstallerTest.archivePath, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($name in @('xswap.exe', 'LICENSE', 'README.md', 'THIRD_PARTY_NOTICES.md')) {
            if ($Fault -eq 'missing' -and $name -eq 'THIRD_PARTY_NOTICES.md') { continue }
            $entryName = if ($Fault -eq 'escaping' -and $name -eq 'THIRD_PARTY_NOTICES.md') { '../THIRD_PARTY_NOTICES.md' } else { $name }
            $entry = $zip.CreateEntry($entryName)
            if ($Fault -eq 'link' -and $name -eq 'THIRD_PARTY_NOTICES.md') { $entry.ExternalAttributes = 0xA1FF -shl 16 }
            if ($Fault -eq 'directory' -and $name -eq 'THIRD_PARTY_NOTICES.md') { $entry.ExternalAttributes = 0x10 }
            $source = if ($name -eq 'xswap.exe') { $Binary } else { Join-Path $root $name }
            $bytes = [IO.File]::ReadAllBytes($source)
            if ($Fault -eq 'empty' -and $name -eq 'THIRD_PARTY_NOTICES.md') { $bytes = [byte[]]@() }
            $stream = $entry.Open()
            try { $stream.Write($bytes, 0, $bytes.Length) } finally { $stream.Dispose() }
        }
        if ($Fault -in @('extra', 'duplicate')) {
            $entry = $zip.CreateEntry($(if ($Fault -eq 'extra') { 'extra.txt' } else { 'README.md' }))
            $stream = $entry.Open()
            try { $stream.WriteByte(1) } finally { $stream.Dispose() }
        }
    } finally { $zip.Dispose() }
}

function Reset-Installation([bool]$WithPrior) {
    if (Test-Path -LiteralPath $installDirectory) {
        Microsoft.PowerShell.Management\Remove-Item -LiteralPath $installDirectory -Recurse -Force
    }
    $global:XswapInstallerTest.failure = ''
    $global:XswapInstallerTest.rollbackFailure = $false
    $global:XswapInstallerTest.badChecksum = $false
    $global:XswapInstallerTest.downloadDirectory = $null
    if ($WithPrior) {
        [IO.Directory]::CreateDirectory($installDirectory) | Out-Null
        [IO.File]::WriteAllBytes((Join-Path $installDirectory 'xswap.exe'), ([IO.File]::ReadAllBytes($Binary) + [byte[]](1, 2, 3)))
        [IO.File]::WriteAllText((Join-Path $installDirectory 'LICENSE'), 'prior project license')
        [IO.File]::WriteAllText((Join-Path $installDirectory 'THIRD_PARTY_NOTICES.md'), 'prior dependency notices')
    }
}

function Get-InstalledHashes {
    $hashes = @{}
    foreach ($name in @('xswap.exe', 'LICENSE', 'THIRD_PARTY_NOTICES.md')) {
        $hashes[$name] = (Get-FileHash -LiteralPath (Join-Path $installDirectory $name)).Hash
    }
    return $hashes
}

function Invoke-TestInstaller([bool]$ExpectFailure) {
    $errorMessage = $null
    try { & $installer -Version $version -InstallDir $installDirectory -NoPathUpdate } catch { $errorMessage = $_.ToString() }
    Assert-Condition (($null -ne $errorMessage) -eq $ExpectFailure) "Unexpected installer result: $errorMessage"
    Assert-Condition ($global:XswapInstallerTest.downloadDirectory -and -not (Test-Path -LiteralPath $global:XswapInstallerTest.downloadDirectory)) 'Download directory survived cleanup.'
    if (Test-Path -LiteralPath $installDirectory) {
        Assert-Condition (@(Get-ChildItem -LiteralPath $installDirectory -Filter 'xswap.*.new').Count -eq 0) 'Staging directory survived cleanup.'
    }
    return $errorMessage
}

try {
    New-Archive
    foreach ($upgrade in @($false, $true)) {
        Reset-Installation $upgrade
        Invoke-TestInstaller $false | Out-Null
        $hashes = Get-InstalledHashes
        foreach ($name in $hashes.Keys) {
            $source = if ($name -eq 'xswap.exe') { $Binary } else { Join-Path $root $name }
            Assert-Condition ($hashes[$name] -eq (Get-FileHash -LiteralPath $source).Hash) "Wrong installed payload: $name"
        }
        Assert-Condition (@(Get-ChildItem -LiteralPath $installDirectory).Count -eq 3) 'Unexpected installed files.'
    }
    # Installations made by the old installer have only an executable.
    Reset-Installation $true
    foreach ($name in @('LICENSE', 'THIRD_PARTY_NOTICES.md')) { [IO.File]::Delete((Join-Path $installDirectory $name)) }
    Invoke-TestInstaller $false | Out-Null
    Assert-Condition ((Get-InstalledHashes).Count -eq 3) 'Legacy upgrade omitted documents.'

    # A locked retired executable must keep its own documents until later cleanup.
    $oldDirectory = Join-Path $installDirectory 'xswap.retired.previous'
    [IO.Directory]::CreateDirectory($oldDirectory) | Out-Null
    foreach ($name in @('xswap.exe', 'LICENSE', 'THIRD_PARTY_NOTICES.md')) {
        [IO.File]::Copy((Join-Path $installDirectory $name), (Join-Path $oldDirectory $name))
    }
    $lockedBinary = [IO.File]::Open((Join-Path $oldDirectory 'xswap.exe'), [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
    try {
        Invoke-TestInstaller $false | Out-Null
        Assert-Condition (@(Get-ChildItem -LiteralPath $oldDirectory).Count -eq 3) 'Cleanup discarded notices for a locked executable.'
    } finally { $lockedBinary.Dispose() }
    Invoke-TestInstaller $false | Out-Null
    Assert-Condition (-not (Test-Path -LiteralPath $oldDirectory)) 'Unlocked retired installation survived later cleanup.'

    foreach ($failureCase in @('stage-LICENSE', 'stage-THIRD_PARTY_NOTICES.md', 'retire-LICENSE', 'retire-THIRD_PARTY_NOTICES.md', 'install-xswap.exe', 'install-LICENSE', 'install-THIRD_PARTY_NOTICES.md')) {
        foreach ($upgrade in @($false, $true)) {
            if (-not $upgrade -and $failureCase.StartsWith('retire-')) { continue }
            Reset-Installation $upgrade
            $prior = if ($upgrade) { Get-InstalledHashes } else { @{} }
            $global:XswapInstallerTest.failure = $failureCase
            Invoke-TestInstaller $true | Out-Null
            if ($upgrade) {
                $after = Get-InstalledHashes
                foreach ($name in $prior.Keys) { Assert-Condition ($prior[$name] -eq $after[$name]) "Rollback changed $name after $failureCase" }
            } else {
                Assert-Condition (@(Get-ChildItem -LiteralPath $installDirectory -File).Count -eq 0) "Failed fresh install left files after $failureCase"
            }
        }
    }
    foreach ($failureCase in @('retire-LICENSE', 'retire-THIRD_PARTY_NOTICES.md', 'install-THIRD_PARTY_NOTICES.md')) {
        Reset-Installation $true
        $prior = Get-InstalledHashes
        $global:XswapInstallerTest.failure = $failureCase
        $global:XswapInstallerTest.rollbackFailure = $true
        $message = Invoke-TestInstaller $true
        Assert-Condition ($message -match 'Rollback could not complete; previous files are retained') 'Rollback failure did not identify recovery files.'
        $backup = @(Get-ChildItem -LiteralPath $installDirectory -Filter 'xswap.*.previous' -Directory)
        Assert-Condition ($backup.Count -eq 1) 'Rollback recovery directory was discarded.'
        foreach ($name in $prior.Keys) {
            Assert-Condition ((Get-FileHash -LiteralPath (Join-Path $backup[0].FullName $name)).Hash -eq $prior[$name]) "Rollback recovery lost $name after $failureCase"
        }
        $global:XswapInstallerTest.failure = ''
        $global:XswapInstallerTest.rollbackFailure = $false
        Invoke-TestInstaller $false | Out-Null
        foreach ($name in $prior.Keys) {
            Assert-Condition ((Get-FileHash -LiteralPath (Join-Path $backup[0].FullName $name)).Hash -eq $prior[$name]) "Later cleanup discarded rollback recovery $name after $failureCase"
        }
    }
    foreach ($fault in @('missing', 'extra', 'duplicate', 'escaping', 'link', 'directory', 'empty', 'checksum')) {
        Reset-Installation $true
        $prior = Get-InstalledHashes
        New-Archive $fault
        $global:XswapInstallerTest.badChecksum = $fault -eq 'checksum'
        Invoke-TestInstaller $true | Out-Null
        $after = Get-InstalledHashes
        foreach ($name in $prior.Keys) { Assert-Condition ($prior[$name] -eq $after[$name]) "Invalid archive changed ${name}: $fault" }
    }
    Write-Host "Passed native $target installer checks with mocked downloads."
} finally {
    Microsoft.PowerShell.Management\Remove-Item -LiteralPath $testDirectory -Recurse -Force
    Remove-Variable -Name XswapInstallerTest -Scope Global
    Assert-Condition ($env:Path -eq $processPath) 'Test changed process PATH.'
    Assert-Condition ([Environment]::GetEnvironmentVariable('Path', 'User') -eq $userPath) 'Test changed user PATH.'
}
