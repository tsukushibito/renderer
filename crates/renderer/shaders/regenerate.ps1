# Regenerate trusted fixtures with the Vulkan SDK 1.4.341.1 tools.
# Normal Rust builds use the checked-in SPIR-V and require no shader compiler.
# Run ./regenerate.ps1 -Verify to validate and compare without replacing files.
[CmdletBinding()]
param(
    [string]$SdkRoot = $env:VULKAN_SDK,
    [switch]$Verify
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($SdkRoot)) {
    throw 'Set VULKAN_SDK or pass -SdkRoot pointing to Vulkan SDK 1.4.341.1.'
}
$compiler = Join-Path $SdkRoot 'Bin/glslc.exe'
$validator = Join-Path $SdkRoot 'Bin/spirv-val.exe'
if (-not (Test-Path -LiteralPath $compiler -PathType Leaf) -or
    -not (Test-Path -LiteralPath $validator -PathType Leaf)) {
    throw 'Vulkan SDK glslc.exe and spirv-val.exe are required.'
}
$compilerVersion = & $compiler --version
if ($LASTEXITCODE -ne 0 -or
    $compilerVersion[0] -cne 'shaderc v2026.1 v2026.1' -or
    $compilerVersion[1] -cne 'spirv-tools v2026.1 v2022.4-1089-gfbe4f3ad' -or
    $compilerVersion[2] -cne 'glslang 11.1.0-1366-gf0bd0257') {
    throw "Unexpected glslc version: $compilerVersion"
}
$validatorVersion = & $validator --version
if ($LASTEXITCODE -ne 0 -or
    $validatorVersion[0] -cne 'SPIRV-Tools v2026.1 v2026.1-0-gfbe4f3ad') {
    throw "Unexpected spirv-val version: $validatorVersion"
}

# Explicit SPIR-V 1.3 avoids LocalSizeId/maintenance4 requirements. The runtime
# enables synchronization2/dynamicRendering, but does not enable maintenance4.
# Use entry point main, GLSL 450 core and optimized code without debug info.
# Compile every GLSL variant in the shared toolchain manifest, including
# external test fixtures and renderer-pipeline's standard output programs.
$toolchain = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil-toolchain.json') -Raw | ConvertFrom-Json
$sources = $toolchain.shaders.source
$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ('renderer-spirv-' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $temporaryRoot
try {
    foreach ($source in $sources) {
        $variant = @()
        $sourceFile = $source
        if ([IO.Path]::GetFileName($source) -cmatch '^sdr(?:_fxaa)?_linear\.frag$') {
            $sourceFile = $source -creplace '_linear\.frag$', '.frag'
            $variant += '-DRENDERER_LINEAR_OUTPUT=1'
        }
        $sourcePath = Join-Path $PSScriptRoot $sourceFile
        $outputPath = Join-Path $PSScriptRoot ($source + '.spv')
        # External caller-owned sources have relative path components. Only the
        # basename is used inside the task-owned temporary directory.
        $temporaryPath = Join-Path $temporaryRoot ([IO.Path]::GetFileName($source) + '.spv')
        & $compiler --target-env=vulkan1.3 --target-spv=spv1.3 -std=450core `
            -fentry-point=main -O -Werror @variant $sourcePath -o $temporaryPath
        if ($LASTEXITCODE -ne 0) { throw "glslc failed for $source" }
        & $validator --target-env vulkan1.3 $temporaryPath
        if ($LASTEXITCODE -ne 0) { throw "spirv-val failed for $source" }
        $generatedHash = (Get-FileHash -LiteralPath $temporaryPath -Algorithm SHA256).Hash
        if ($Verify) {
            if (-not (Test-Path -LiteralPath $outputPath -PathType Leaf)) {
                throw "Missing checked-in fixture: $outputPath"
            }
            $existingHash = (Get-FileHash -LiteralPath $outputPath -Algorithm SHA256).Hash
            if ($existingHash -cne $generatedHash) {
                throw "Fixture differs from regenerated shader: $source"
            }
        }
        Write-Output "$source $generatedHash"
    }
    # A later compiler/validator failure must leave the entire fixture set intact.
    # Publish only after every source has compiled and validated successfully.
    if (-not $Verify) {
        foreach ($source in $sources) {
            Copy-Item -LiteralPath (Join-Path $temporaryRoot ([IO.Path]::GetFileName($source) + '.spv')) `
                -Destination (Join-Path $PSScriptRoot ($source + '.spv')) -Force
        }
    }
} finally {
    $resolvedTemporary = [IO.Path]::GetFullPath($temporaryRoot)
    $resolvedTempParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if ([IO.Path]::GetDirectoryName($resolvedTemporary).TrimEnd('\') -cne $resolvedTempParent.TrimEnd('\') -or
        [IO.Path]::GetFileName($resolvedTemporary) -notmatch '^renderer-spirv-[0-9a-f]{32}$') {
        throw "Refusing to remove unexpected temporary path: $resolvedTemporary"
    }
    Remove-Item -LiteralPath $resolvedTemporary -Recurse -Force
}
