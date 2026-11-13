# Regenerate the trusted D3D12 fixtures from the existing SPIR-V baseline.
# Normal Rust builds use checked-in binaries and need no SDK or compiler.
# -Verify compares every derived artifact without changing the checkout.
[CmdletBinding()]
param(
    [string]$SdkRoot = $env:VULKAN_SDK,
    [switch]$Verify
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-Sha256([string]$Path) {
    $hasher = [Security.Cryptography.SHA256]::Create()
    try {
        $stream = [IO.File]::OpenRead($Path)
        try {
            return ([BitConverter]::ToString($hasher.ComputeHash($stream))).Replace('-', '').ToLowerInvariant()
        } finally {
            $stream.Dispose()
        }
    } finally {
        $hasher.Dispose()
    }
}

function Get-SourceSha256([string]$Path) {
    $hasher = [Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes([IO.File]::ReadAllText($Path).Replace("`r`n", "`n"))
        return ([BitConverter]::ToString($hasher.ComputeHash($bytes))).Replace('-', '').ToLowerInvariant()
    } finally {
        $hasher.Dispose()
    }
}

# Run without a shell so stderr is checked consistently in Windows PowerShell
# and pwsh. Quote according to the Windows native command-line argument rules.
function Invoke-ShaderTool([string]$Executable, [string[]]$ToolArguments, [switch]$Revision) {
    $quoted = foreach ($argument in $ToolArguments) {
        $escaped = [regex]::Replace($argument, '(\\*)"', '$1$1\"')
        $escaped = [regex]::Replace($escaped, '(\\+)$', '$1$1')
        '"' + $escaped + '"'
    }
    $process = New-Object Diagnostics.Process
    $process.StartInfo.FileName = $Executable
    $process.StartInfo.Arguments = $quoted -join ' '
    $process.StartInfo.UseShellExecute = $false
    $process.StartInfo.CreateNoWindow = $true
    $process.StartInfo.RedirectStandardOutput = $true
    $process.StartInfo.RedirectStandardError = $true
    try {
        $null = $process.Start()
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $output = $stdout.GetAwaiter().GetResult()
        $errors = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0 -or (-not $Revision -and -not [string]::IsNullOrWhiteSpace($errors))) {
            throw "Shader tool failed ($Executable, exit $($process.ExitCode))`n$output$errors"
        }
        return ($output + $errors).Replace("`r`n", "`n")
    } finally {
        $process.Dispose()
    }
}

function Write-Utf8([string]$Path, [string]$Value) {
    [IO.File]::WriteAllText($Path, $Value, (New-Object Text.UTF8Encoding $false))
}

if ([string]::IsNullOrWhiteSpace($SdkRoot)) {
    throw 'Set VULKAN_SDK or pass -SdkRoot pointing to Vulkan SDK 1.4.341.1 Windows x64.'
}
$SdkRoot = (Resolve-Path -LiteralPath $SdkRoot).Path
$lockPath = Join-Path $PSScriptRoot 'dxil-toolchain.json'
$lock = Get-Content -LiteralPath $lockPath -Raw -Encoding utf8 | ConvertFrom-Json
foreach ($file in $lock.files.PSObject.Properties) {
    $toolPath = Join-Path $SdkRoot ('Bin/' + $file.Name)
    if (-not (Test-Path -LiteralPath $toolPath -PathType Leaf) -or (Get-Sha256 $toolPath) -cne $file.Value) {
        throw "Missing or changed pinned tool: $toolPath"
    }
}
$cross = Join-Path $SdkRoot 'Bin/spirv-cross.exe'
$dxc = Join-Path $SdkRoot 'Bin/dxc.exe'
if ((Invoke-ShaderTool $cross @('--revision') -Revision).Trim() -cne $lock.spirv_cross_revision -or
    (Invoke-ShaderTool $dxc @('--version')).Trim() -cne $lock.dxc_version) {
    throw 'Unexpected SPIRV-Cross or DXC version.'
}

# This always verifies, never rewrites, the existing Vulkan artifacts.
& (Join-Path $PSScriptRoot 'regenerate.ps1') -SdkRoot $SdkRoot -Verify

$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ('renderer-dxil-' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $temporaryRoot
$destination = Join-Path $PSScriptRoot 'dxil'
try {
    $artifacts = [ordered]@{}
    foreach ($shader in $lock.shaders) {
        $source = $shader.source
        $spirv = Join-Path $PSScriptRoot ($source + '.spv')
        $hlsl = Join-Path $temporaryRoot ($source + '.hlsl')
        $dxil = Join-Path $temporaryRoot ($source + '.dxil')
        $crossArgs = @($spirv) + @($lock.spirv_cross_arguments) + @('--stage', $shader.stage) +
            @($shader.spirv_cross_arguments) + @('--output', $hlsl)
        $null = Invoke-ShaderTool $cross $crossArgs
        # Canonical UTF-8/LF HLSL makes the checked-in text independent of
        # checkout line-ending settings; there are no includes or macros.
        Write-Utf8 $hlsl ([IO.File]::ReadAllText($hlsl).Replace("`r`n", "`n"))
        $dxcArgs = @($lock.dxc_arguments) + @('-T', $shader.profile, $hlsl, '-Fo', $dxil)
        $null = Invoke-ShaderTool $dxc $dxcArgs
        $dump = Invoke-ShaderTool $dxc @('-dumpbin', $dxil)
        $reflectionEnd = $dump.IndexOf('target datalayout = ', [StringComparison]::Ordinal)
        if ($reflectionEnd -lt 0 -or $dump -notmatch '!dx\.valver = ' -or $dump -notmatch '; shader hash: [0-9a-f]{32}') {
            throw "DXIL validation/reflection metadata missing for $source"
        }
        $reflection = [regex]::Replace($dump.Substring(0, $reflectionEnd), '(?m)[ \t]+$', '')
        Write-Utf8 (Join-Path $temporaryRoot ($source + '.reflection.txt')) $reflection
        $hashes = [ordered]@{
            source_utf8_lf = Get-SourceSha256 (Join-Path $PSScriptRoot $source)
            spirv = Get-Sha256 $spirv
            hlsl = Get-Sha256 $hlsl
            dxil = Get-Sha256 $dxil
            reflection = Get-Sha256 (Join-Path $temporaryRoot ($source + '.reflection.txt'))
        }
        $artifacts[$source] = $hashes
    }
    $manifest = [ordered]@{
        schema = 1
        toolchain_sha256 = Get-Sha256 $lockPath
        contracts_sha256 = Get-Sha256 (Join-Path $PSScriptRoot 'dxil-contracts.json')
        artifacts = $artifacts
    }
    Write-Utf8 (Join-Path $temporaryRoot 'manifest.json') (($manifest | ConvertTo-Json -Depth 5 -Compress) + "`n")
    # Validate the whole set before publishing any changed fixture.
    foreach ($artifact in Get-ChildItem -LiteralPath $temporaryRoot -File) {
        $target = Join-Path $destination $artifact.Name
        if ($Verify -and (-not (Test-Path -LiteralPath $target -PathType Leaf) -or
            (Get-Sha256 $artifact.FullName) -cne (Get-Sha256 $target))) {
            throw "DXIL fixture differs or is missing: $($artifact.Name)"
        }
    }
    if (-not $Verify) {
        $null = New-Item -ItemType Directory -Path $destination -Force
        foreach ($artifact in Get-ChildItem -LiteralPath $temporaryRoot -File) {
            Copy-Item -LiteralPath $artifact.FullName -Destination (Join-Path $destination $artifact.Name) -Force
        }
    }
    foreach ($entry in $artifacts.GetEnumerator()) {
        Write-Output "$($entry.Key) DXIL $($entry.Value.dxil)"
    }
} finally {
    # The path is an explicitly created, task-owned direct child of TEMP.
    $resolvedTemporary = [IO.Path]::GetFullPath($temporaryRoot)
    $resolvedTempParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if ([IO.Path]::GetDirectoryName($resolvedTemporary).TrimEnd('\') -cne $resolvedTempParent.TrimEnd('\') -or
        [IO.Path]::GetFileName($resolvedTemporary) -notmatch '^renderer-dxil-[0-9a-f]{32}$') {
        throw "Refusing to remove unexpected temporary path: $resolvedTemporary"
    }
    Remove-Item -LiteralPath $resolvedTemporary -Recurse -Force
}
