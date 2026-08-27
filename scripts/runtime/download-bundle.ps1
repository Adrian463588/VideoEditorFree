[CmdletBinding()]
param(
    [ValidateSet('core', 'ai', 'all')]
    [string]$Profile = 'all',
    [string]$InstallRoot = '',
    [string]$ManifestPath = '',
    [switch]$VerifyManifestOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-DefaultInstallRoot {
    if ($env:LOCALAPPDATA) {
        return (Join-Path $env:LOCALAPPDATA 'VideoEditorFree\runtime')
    }
    return (Join-Path (Get-Location) '.videoeditorfree\runtime')
}

function Get-Sha256([string]$Path) {
    $lines = & certutil.exe -hashfile $Path SHA256 2>$null
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to calculate SHA-256 for $Path."
    }
    $hash = ($lines | Where-Object { $_ -match '^[0-9a-fA-F ]{64,}$' } | Select-Object -First 1)
    if (-not $hash) {
        throw "certutil returned no SHA-256 for $Path."
    }
    return ($hash -replace '\s', '').ToLowerInvariant()
}

function Write-Utf8NoBom([string]$Path, [string]$Content) {
    [IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($false))
}

function Write-Utf8NoBomAtomic([string]$Path, [string]$Content) {
    $temporary = "$Path.part"
    Write-Utf8NoBom $temporary $Content
    Move-Item -LiteralPath $temporary -Destination $Path -Force
}

function Assert-Artifact($Artifact) {
    foreach ($property in @('id', 'kind', 'destination', 'url', 'source', 'version', 'license', 'size_bytes', 'sha256')) {
        if (-not $Artifact.PSObject.Properties.Name.Contains($property) -or [string]::IsNullOrWhiteSpace([string]$Artifact.$property)) {
            throw "Bundle manifest artifact is missing $property."
        }
    }
    $uri = [Uri]$Artifact.url
    if ($uri.Scheme -ne 'https' -or @('github.com', 'huggingface.co') -notcontains $uri.Host.ToLowerInvariant()) {
        throw "Artifact $($Artifact.id) is outside the HTTPS allowlist."
    }
    if ([int64]$Artifact.size_bytes -le 0 -or [int64]$Artifact.size_bytes -gt 2GB) {
        throw "Artifact $($Artifact.id) has an invalid size."
    }
    if ([string]$Artifact.sha256 -notmatch '^[0-9a-fA-F]{64}$') {
        throw "Artifact $($Artifact.id) does not have a verified SHA-256."
    }
    $destination = [IO.Path]::GetFullPath((Join-Path $InstallRoot ([string]$Artifact.destination)))
    $root = [IO.Path]::GetFullPath($InstallRoot).TrimEnd('\') + '\'
    if (-not $destination.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Artifact $($Artifact.id) destination escapes the install root."
    }
}

function Download-Artifact($Artifact) {
    Assert-Artifact $Artifact
    $target = [IO.Path]::GetFullPath((Join-Path $InstallRoot ([string]$Artifact.destination)))
    $parent = Split-Path -Parent $target
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    $part = "$target.part"

    if (Test-Path -LiteralPath $target) {
        $existing = Get-Item -LiteralPath $target
        if ([int64]$existing.Length -eq [int64]$Artifact.size_bytes -and (Get-Sha256 $target) -eq ([string]$Artifact.sha256).ToLowerInvariant()) {
            Write-Host "verified $($Artifact.id)"
            return $target
        }
        Remove-Item -LiteralPath $target -Force
    }

    if (Test-Path -LiteralPath $part) {
        $partial = Get-Item -LiteralPath $part
        if ([int64]$partial.Length -eq [int64]$Artifact.size_bytes) {
            if ((Get-Sha256 $part) -eq ([string]$Artifact.sha256).ToLowerInvariant()) {
                Move-Item -LiteralPath $part -Destination $target -Force
                Write-Host "verified $($Artifact.id) from complete partial download"
                return $target
            }
            Remove-Item -LiteralPath $part -Force
        } elseif ([int64]$partial.Length -gt [int64]$Artifact.size_bytes) {
            Remove-Item -LiteralPath $part -Force
        }
    }

    Write-Host "download $($Artifact.id)"
    & curl.exe --fail --location --retry 4 --retry-all-errors --connect-timeout 20 --speed-limit 1024 --speed-time 60 --proto '=https' --proto-redir '=https' --continue-at - --output $part ([string]$Artifact.url)
    if ($LASTEXITCODE -ne 0) {
        throw "Download failed for $($Artifact.id); partial file retained at $part."
    }
    $downloaded = Get-Item -LiteralPath $part
    if ([int64]$downloaded.Length -ne [int64]$Artifact.size_bytes) {
        throw "Size mismatch for $($Artifact.id): expected $($Artifact.size_bytes), got $($downloaded.Length)."
    }
    $actual = Get-Sha256 $part
    if ($actual -ne ([string]$Artifact.sha256).ToLowerInvariant()) {
        throw "SHA-256 mismatch for $($Artifact.id): expected $($Artifact.sha256), got $actual."
    }
    Move-Item -LiteralPath $part -Destination $target -Force
    Write-Host "verified $($Artifact.id)"
    return $target
}

function Expand-RequiredFiles([string]$Archive, [string]$Destination, [string[]]$RequiredNames) {
    $stage = Join-Path ([IO.Path]::GetTempPath()) ("videoeditorfree-extract-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    try {
        Expand-Archive -LiteralPath $Archive -DestinationPath $stage -Force
        New-Item -ItemType Directory -Force -Path $Destination | Out-Null
        foreach ($name in $RequiredNames) {
            $candidate = Get-ChildItem -LiteralPath $stage -Recurse -File -Filter $name | Select-Object -First 1
            if (-not $candidate) {
                throw "Archive $Archive does not contain required file $name."
            }
            $target = Join-Path $Destination $name
            $temporary = "$target.part"
            Copy-Item -LiteralPath $candidate.FullName -Destination $temporary -Force
            Move-Item -LiteralPath $temporary -Destination $target -Force
        }
    } finally {
        if (Test-Path -LiteralPath $stage) {
            Remove-Item -LiteralPath $stage -Recurse -Force
        }
    }
}

function Expand-ArchiveTree([string]$Archive, [string]$Destination, [string]$RequiredName) {
    $stage = Join-Path ([IO.Path]::GetTempPath()) ("videoeditorfree-extract-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    try {
        Expand-Archive -LiteralPath $Archive -DestinationPath $stage -Force
        $required = Get-ChildItem -LiteralPath $stage -Recurse -File -Filter $RequiredName | Select-Object -First 1
        if (-not $required) {
            throw "Archive $Archive does not contain required file $RequiredName."
        }
        $sourceRoot = $required.Directory.FullName.TrimEnd('\')
        New-Item -ItemType Directory -Force -Path $Destination | Out-Null
        foreach ($sourceFile in Get-ChildItem -LiteralPath $sourceRoot -Recurse -File) {
            $relative = $sourceFile.FullName.Substring($sourceRoot.Length).TrimStart('\')
            $target = [IO.Path]::GetFullPath((Join-Path $Destination $relative))
            $destinationRoot = [IO.Path]::GetFullPath($Destination).TrimEnd('\') + '\'
            if (-not $target.StartsWith($destinationRoot, [StringComparison]::OrdinalIgnoreCase)) {
                throw "Archive entry escapes the destination: $relative"
            }
            $parent = Split-Path -Parent $target
            New-Item -ItemType Directory -Force -Path $parent | Out-Null
            $temporary = "$target.part"
            Copy-Item -LiteralPath $sourceFile.FullName -Destination $temporary -Force
            Move-Item -LiteralPath $temporary -Destination $target -Force
        }
    } finally {
        if (Test-Path -LiteralPath $stage) {
            Remove-Item -LiteralPath $stage -Recurse -Force
        }
    }
}

if ([string]::IsNullOrWhiteSpace($InstallRoot)) {
    $InstallRoot = Get-DefaultInstallRoot
}
$InstallRoot = [IO.Path]::GetFullPath($InstallRoot)
New-Item -ItemType Directory -Force -Path $InstallRoot | Out-Null
$manifestPath = $ManifestPath
if ([string]::IsNullOrWhiteSpace($manifestPath)) {
    $manifestPath = Join-Path $PSScriptRoot 'bundle-manifest.json'
}
if (-not (Test-Path -LiteralPath $manifestPath)) {
    $manifestPath = Join-Path $PSScriptRoot '..\..\resources\runtime\bundle-manifest.json'
}
if (-not (Test-Path -LiteralPath $manifestPath)) {
    $manifestPath = Join-Path $PSScriptRoot '..\..\..\resources\runtime\bundle-manifest.json'
}
if (-not (Test-Path -LiteralPath $manifestPath)) {
    throw "Bundle manifest is missing: $manifestPath"
}
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ([int]$manifest.schema_version -ne 1 -or $manifest.target.os -ne 'windows' -or $manifest.target.architecture -ne 'x86_64') {
    throw 'Unsupported bundle manifest target.'
}

$selected = @($manifest.artifacts | Where-Object { $Profile -eq 'all' -or $_.profile -eq $Profile })
if ($selected.Count -eq 0) {
    throw "No artifacts are defined for profile $Profile."
}
if ($VerifyManifestOnly) {
    foreach ($artifact in $selected) {
        Assert-Artifact $artifact
        Write-Output "manifest ok: $($artifact.id)"
    }
    exit 0
}
$downloaded = @{}
foreach ($artifact in $selected) {
    $downloaded[$artifact.id] = Download-Artifact $artifact
}

$ffmpeg = $selected | Where-Object id -eq 'ffmpeg-windows-x64-gpl'
if ($ffmpeg) {
    Expand-RequiredFiles $downloaded[$ffmpeg.id] (Join-Path $InstallRoot 'media') @('ffmpeg.exe', 'ffprobe.exe')
    $mediaManifest = [ordered]@{
        identity = 'BtbN FFmpeg Windows x64 GPL build'
        version = $ffmpeg.version
        license = $ffmpeg.license
        sha256 = $ffmpeg.sha256
        architecture = 'x86_64'
    }
    Write-Utf8NoBomAtomic (Join-Path $InstallRoot 'media-manifest.json') ($mediaManifest | ConvertTo-Json)
}

$llama = $selected | Where-Object id -eq 'llama-cpp-windows-x64-cpu'
if ($llama) {
    Expand-ArchiveTree $downloaded[$llama.id] (Join-Path $InstallRoot 'ai\llama') 'llama-cli.exe'
}

$piper = $selected | Where-Object id -eq 'piper-windows-x64-runtime'
if ($piper) {
    Expand-ArchiveTree $downloaded[$piper.id] (Join-Path $InstallRoot 'ai\piper') 'piper.exe'
}

$state = [ordered]@{
    bundle_id = $manifest.bundle_id
    version = $manifest.version
    profile = $Profile
    install_root = $InstallRoot
    verified_artifacts = @($selected | ForEach-Object { $_.id })
    generated_at_utc = [DateTime]::UtcNow.ToString('o')
}
Write-Utf8NoBomAtomic (Join-Path $InstallRoot 'bundle-state.json') ($state | ConvertTo-Json -Depth 5)
Write-Output "bundle ready: $InstallRoot"
