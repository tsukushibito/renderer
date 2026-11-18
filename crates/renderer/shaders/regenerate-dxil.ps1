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
. (Join-Path $PSScriptRoot 'managed-abi.ps1')

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
    $publish = [ordered]@{}
    foreach ($shader in $lock.shaders) {
        $source = $shader.source
        $assetName = [IO.Path]::GetFileName($source)
        $destinationProperty = $shader.PSObject.Properties['destination']
        $assetDestination = if ($null -eq $destinationProperty) {
            $destination
        } else {
            Join-Path $PSScriptRoot $destinationProperty.Value
        }
        $spirv = Join-Path $PSScriptRoot ($source + '.spv')
        $hlsl = Join-Path $temporaryRoot ($assetName + '.hlsl')
        $dxil = Join-Path $temporaryRoot ($assetName + '.dxil')
        $crossArgs = @($spirv) + @($lock.spirv_cross_arguments) + @('--stage', $shader.stage) +
            @($shader.spirv_cross_arguments) + @('--output', $hlsl)
        $null = Invoke-ShaderTool $cross $crossArgs
        # Canonical UTF-8/LF HLSL makes the checked-in text independent of
        # checkout line-ending settings; there are no includes or macros.
        $text = [IO.File]::ReadAllText($hlsl).Replace("`r`n", "`n")
        if ($source -ceq 'tone_neutral.frag') {
            $unbound = '(?m)^cbuffer ([A-Za-z_][A-Za-z_0-9]*)$'
            if ([regex]::Matches($text, $unbound).Count -ne 1) {
                throw 'tone_neutral.frag must declare one inline constant block.'
            }
            $text = [regex]::Replace($text, $unbound, 'cbuffer $1 : register(b0, space0)')
        }
        Write-Utf8 $hlsl $text
        $dxcArgs = @($lock.dxc_arguments) + @('-T', $shader.profile, $hlsl, '-Fo', $dxil)
        $null = Invoke-ShaderTool $dxc $dxcArgs
        $dump = Invoke-ShaderTool $dxc @('-dumpbin', $dxil)
        $reflectionEnd = $dump.IndexOf('target datalayout = ', [StringComparison]::Ordinal)
        if ($reflectionEnd -lt 0 -or $dump -notmatch '!dx\.valver = ' -or $dump -notmatch '; shader hash: [0-9a-f]{32}') {
            throw "DXIL validation/reflection metadata missing for $source"
        }
        $reflection = [regex]::Replace($dump.Substring(0, $reflectionEnd), '(?m)[ \t]+$', '')
        Write-Utf8 (Join-Path $temporaryRoot ($assetName + '.reflection.txt')) $reflection
        $outputProgram = $assetName -cmatch '^(?:tone_neutral|sdr(?:_fxaa)?(?:_linear)?)\.frag$'
        $reflected = $outputProgram -or $assetName -cin @('uniform.comp', 'sampled.comp', 'sampled_float.comp', 'storage_read.comp', 'indexed_indirect.comp', 'textured.vert', 'textured.frag', 'compute_hdr_sampled.comp', 'compute_hdr_storage.comp')
        if ($reflected) {
            $spirvReflection = Invoke-ShaderTool $cross @($spirv, '--reflect')
            if ($source -ceq 'uniform.comp') {
                Assert-BufferCopyAbi ($spirvReflection | ConvertFrom-Json) $reflection
            } elseif ($source -ceq 'storage_read.comp') {
                Assert-BufferCopyAbi ($spirvReflection | ConvertFrom-Json) $reflection -StorageInput
            } elseif ($source -ceq 'indexed_indirect.comp') {
                Assert-IndexedIndirectAbi ($spirvReflection | ConvertFrom-Json) $reflection
            } elseif ($source -ceq 'sampled.comp') {
                Assert-SampledAbi ($spirvReflection | ConvertFrom-Json) $reflection
            } elseif ($source -ceq 'sampled_float.comp') {
                Assert-SampledAbi ($spirvReflection | ConvertFrom-Json) $reflection -FloatOutput
            } elseif ($assetName -cin @('compute_hdr_sampled.comp', 'compute_hdr_storage.comp')) {
                Assert-HdrComputeAbi ($spirvReflection | ConvertFrom-Json) $reflection -StorageInput:($assetName -ceq 'compute_hdr_storage.comp')
            } elseif ($outputProgram) {
                Assert-SdrOutputAbi ($spirvReflection | ConvertFrom-Json) $reflection -Exposure:($assetName -ceq 'tone_neutral.frag')
            } else {
                Assert-TexturedRasterAbi ($spirvReflection | ConvertFrom-Json) $reflection -Vertex:($source -ceq 'textured.vert')
            }
            Write-Utf8 (Join-Path $temporaryRoot ($assetName + '.spirv-reflection.json')) $spirvReflection
        }
        $sourceFile = $source -creplace '^(sdr(?:_fxaa)?)_linear\.frag$', '$1.frag'
        $hashes = [ordered]@{
            source_utf8_lf = Get-SourceSha256 (Join-Path $PSScriptRoot $sourceFile)
            spirv = Get-Sha256 $spirv
            hlsl = Get-Sha256 $hlsl
            dxil = Get-Sha256 $dxil
            reflection = Get-Sha256 (Join-Path $temporaryRoot ($assetName + '.reflection.txt'))
        }
        if ($reflected) {
            $hashes.spirv_reflection = Get-Sha256 (Join-Path $temporaryRoot ($assetName + '.spirv-reflection.json'))
        }
        $extensions = @('.hlsl', '.dxil', '.reflection.txt')
        if ($reflected) { $extensions += '.spirv-reflection.json' }
        foreach ($extension in $extensions) {
            $temporary = Join-Path $temporaryRoot ($assetName + $extension)
            if ($publish.Contains($temporary)) { throw "Duplicate shader artifact basename: $assetName" }
            $publish[$temporary] = Join-Path $assetDestination ($assetName + $extension)
        }
        $artifacts[$source] = $hashes
    }
    $licenses = [ordered]@{}
    foreach ($license in @('FXAA-notices.txt', 'Godot-MIT.txt', 'Khronos-ToneMapping-Apache-2.0.txt')) {
        $licenses[$license] = Get-SourceSha256 (Join-Path $PSScriptRoot ('licenses/' + $license))
    }
    $manifest = [ordered]@{
        schema = 3
        toolchain_sha256 = Get-Sha256 $lockPath
        contracts_sha256 = Get-Sha256 (Join-Path $PSScriptRoot 'dxil-contracts.json')
        managed_abi_verifier_utf8_lf_sha256 = Get-SourceSha256 (Join-Path $PSScriptRoot 'managed-abi.ps1')
        artifacts = $artifacts
        licenses = $licenses
    }
    Write-Utf8 (Join-Path $temporaryRoot 'manifest.json') (($manifest | ConvertTo-Json -Depth 5 -Compress) + "`n")
    $publish[(Join-Path $temporaryRoot 'manifest.json')] = Join-Path $destination 'manifest.json'
    # Validate the whole set before publishing any changed fixture.
    foreach ($artifact in $publish.GetEnumerator()) {
        $target = $artifact.Value
        if ($Verify -and (-not (Test-Path -LiteralPath $target -PathType Leaf) -or
            (Get-Sha256 $artifact.Key) -cne (Get-Sha256 $target))) {
            throw "DXIL fixture differs or is missing: $target"
        }
    }
    if (-not $Verify) {
        foreach ($artifact in $publish.GetEnumerator()) {
            $null = New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($artifact.Value)) -Force
            Copy-Item -LiteralPath $artifact.Key -Destination $artifact.Value -Force
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
