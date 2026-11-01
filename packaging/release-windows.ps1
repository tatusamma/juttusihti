<#
.SYNOPSIS
Builds and signs the Windows release locally and adds it to the draft GitHub Release.

.DESCRIPTION
Run from the repository root with the release tag checked out and a clean working tree.
The Release workflow must already have created the draft for the tag. The script:

  1. writes Packager.local.toml: Packager.toml plus a [windows] signing section,
  2. builds the NSIS installer with cargo-packager, which signs the app binary, the
     uninstaller and the installer,
  3. checks the Authenticode signatures,
  4. puts the installer, a portable zip and SHA256SUMS-windows.txt in dist\, and
  5. uploads them to the draft release (unless -NoUpload).

See RELEASING.md.

.PARAMETER Tag
The release tag, for example v0.2.0.

.PARAMETER CertificateThumbprint
SHA-1 thumbprint of a code-signing certificate in the Windows certificate store; signs
with signtool.exe from the Windows SDK.

.PARAMETER SignCommand
A custom signing command instead, with %1 for the file, for example a cloud signing tool.
cargo-packager splits it on spaces, so arguments cannot contain spaces.

.PARAMETER TimestampUrl
RFC 3161 timestamp server used with -CertificateThumbprint.

.PARAMETER NoUpload
Build, sign and check, but do not upload.

.EXAMPLE
.\packaging\release-windows.ps1 -Tag v0.2.0 -CertificateThumbprint 0123456789ABCDEF0123456789ABCDEF01234567
#>
[CmdletBinding(DefaultParameterSetName = 'Thumbprint')]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$')]
    [string]$Tag,

    [Parameter(Mandatory, ParameterSetName = 'Thumbprint')]
    [ValidatePattern('^[0-9A-Fa-f]{40}$')]
    [string]$CertificateThumbprint,

    [Parameter(Mandatory, ParameterSetName = 'Command')]
    [string]$SignCommand,

    [Parameter(ParameterSetName = 'Thumbprint')]
    [string]$TimestampUrl = 'http://timestamp.digicert.com',

    [switch]$NoUpload
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Windows PowerShell turns a native tool's stderr (cargo's progress) into errors when output is
# redirected, so only the exit code decides failure.
function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments)
    $ErrorActionPreference = 'Continue'
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Exe $($Arguments -join ' ') failed with exit code $LASTEXITCODE" }
}

function Get-TomlVersion([string]$Path, [string]$Section) {
    $inSection = $Section -eq ''
    foreach ($line in Get-Content $Path) {
        if ($line -match '^\[(.+)\]') { $inSection = $Matches[1] -eq $Section; continue }
        if ($inSection -and $line -match '^version = "(.+)"') { return $Matches[1] }
    }
    throw "No version in $Path"
}

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$version = $Tag.Substring(1)

# The build must be exactly the tagged commit.
if (Invoke-Native git @('status', '--porcelain')) { throw 'The working tree is not clean.' }
$tagCommit = "$(Invoke-Native git @('rev-parse', "$Tag^{commit}"))".Trim()
$head = "$(Invoke-Native git @('rev-parse', 'HEAD'))".Trim()
if ($tagCommit -ne $head) { throw "HEAD is not $Tag. Run: git checkout $Tag" }
foreach ($v in @((Get-TomlVersion 'Cargo.toml' 'workspace.package'), (Get-TomlVersion 'Packager.toml' ''))) {
    if ($v -ne $version) { throw "Tag $Tag does not match version $v in Cargo.toml or Packager.toml." }
}

if (-not $NoUpload) {
    try { $isDraft = "$(Invoke-Native gh @('release', 'view', $Tag, '--json', 'isDraft', '--jq', '.isDraft'))".Trim() }
    catch { throw "No GitHub release for $Tag yet. Wait for the Release workflow." }
    if ($isDraft -ne 'true') { throw "Release $Tag is already published; refusing to change it." }
}

$base = Get-Content 'Packager.toml' -Raw
if ($base -match '(?m)^\[windows\]') { throw 'Packager.toml already has a [windows] section.' }
if ($PSCmdlet.ParameterSetName -eq 'Thumbprint') {
    $signing = @"
digestAlgorithm = "sha256"
certificateThumbprint = "$CertificateThumbprint"
timestampUrl = "$TimestampUrl"
tsp = true
"@
} else {
    if ($SignCommand -notmatch '%1') { throw '-SignCommand must contain %1 for the file to sign.' }
    if ($SignCommand.Contains("'")) { throw '-SignCommand cannot contain single quotes.' }
    $signing = "signCommand = '$SignCommand'"
}
$localConfig = Join-Path $root 'Packager.local.toml'
[IO.File]::WriteAllText($localConfig, "$base`n[windows]`n$signing`n", (New-Object Text.UTF8Encoding $false))

try {
    Invoke-Native cargo @('build', '--release', '-p', 'juttusihti', '--locked')
    Remove-Item 'target\packages\*-setup.exe' -ErrorAction SilentlyContinue
    Invoke-Native cargo @('packager', '--release', '-f', 'nsis', '-c', $localConfig)
} finally {
    Remove-Item $localConfig -ErrorAction SilentlyContinue
}

$installer = @(Get-ChildItem 'target\packages\*-setup.exe')
if ($installer.Count -ne 1) { throw "Expected one installer in target\packages, found $($installer.Count)." }
$exe = Get-Item 'target\release\juttusihti.exe'

$dist = Join-Path $root 'dist'
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory $dist | Out-Null
Copy-Item $installer[0].FullName $dist

# Entries are written by hand: Compress-Archive and ZipFile in Windows PowerShell 5.1 put
# backslashes into entry names, which unzip on Linux and macOS does not treat as folders.
$portable = "juttusihti-$version-x86_64-windows"
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::Open((Join-Path $dist "$portable.zip"), 'Create')
try {
    foreach ($file in @($exe.FullName, 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE')) {
        $item = Get-Item $file
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $item.FullName, "$portable/$($item.Name)", 'Optimal') | Out-Null
    }
} finally {
    $zip.Dispose()
}

# Same format as sha256sum, so `sha256sum -c` works on Linux too.
$assets = @(Get-ChildItem $dist -File | Sort-Object Name)
$sums = $assets | ForEach-Object { "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)" }
[IO.File]::WriteAllText((Join-Path $dist 'SHA256SUMS-windows.txt'), (($sums -join "`n") + "`n"))
Get-Content (Join-Path $dist 'SHA256SUMS-windows.txt')

foreach ($file in @($exe.FullName, (Join-Path $dist $installer[0].Name))) {
    $signature = Get-AuthenticodeSignature $file
    if ($signature.Status -ne 'Valid') { throw "$file is not validly signed: $($signature.Status) $($signature.StatusMessage)" }
    Write-Host "Signed: $file ($($signature.SignerCertificate.Subject))"
}

if ($NoUpload) {
    Write-Host "Built in $dist; not uploaded."
} else {
    Invoke-Native gh (@('release', 'upload', $Tag) + @(Get-ChildItem $dist -File | ForEach-Object FullName) + @('--clobber'))
    Write-Host "Uploaded to the draft release $Tag. Review it on GitHub and publish it there."
}
