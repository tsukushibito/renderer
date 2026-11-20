# Compile the shared PBR shader assets to SPIR-V and DXIL with the repository's pinned SDK.
# -Verify regenerates and compares every artifact without rewriting the checkout.
[CmdletBinding()]
param([string]$SdkRoot = $env:VULKAN_SDK, [switch]$Verify)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Hash([string]$Path) {
    $hasher = [Security.Cryptography.SHA256]::Create()
    try {
        $stream = [IO.File]::OpenRead($Path)
        try { ([BitConverter]::ToString($hasher.ComputeHash($stream))).Replace('-', '').ToLowerInvariant() }
        finally { $stream.Dispose() }
    } finally { $hasher.Dispose() }
}
function Write-Text([string]$Path, [string]$Value) {
    [IO.File]::WriteAllText($Path, $Value.Replace("`r`n", "`n"), (New-Object Text.UTF8Encoding $false))
}
function Test-ClipArithmetic([string]$Name, [string]$Dxil, [string]$Spirv) {
    # Both depth passes use the same sequence of separate FP32 operations.
    if ([regex]::Matches($Dxil, '(?m)^\s+%[0-9]+ = fmul float ').Count -ne 12 -or
        [regex]::Matches($Dxil, '(?m)^\s+%[0-9]+ = fadd float ').Count -ne 12 -or
        $Dxil -match '(?m)^\s+%[0-9]+ = f(?:mul|add) fast ' -or
        $Dxil -match '@dx\.op\.(?:dot[234]|tertiary)\.f32') {
        throw "$Name DXIL lost separate, non-refactorable FP32 clip products/additions."
    }
    $products = [regex]::Matches($Spirv, '(?m)^\s*(%\S+)\s+= OpVectorTimesScalar ')
    $sums = [regex]::Matches($Spirv, '(?m)^\s*(%\S+)\s+= OpFAdd ')
    if ($products.Count -ne 3 -or $sums.Count -ne 3) {
        throw "$Name SPIR-V lost separate FP32 clip products/additions."
    }
    foreach ($operation in (@($products) + @($sums))) {
        $result = [regex]::Escape($operation.Groups[1].Value)
        if ($Spirv -cnotmatch "(?m)^\s*OpDecorate $result NoContraction\s*$") {
            throw "$Name SPIR-V lost NoContraction on a clip operation."
        }
    }
}
function Run([string]$Executable, [string[]]$Arguments, [switch]$Revision) {
    $quoted = foreach ($argument in $Arguments) {
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
        ($output + $errors).Replace("`r`n", "`n")
    } finally { $process.Dispose() }
}

if ([string]::IsNullOrWhiteSpace($SdkRoot)) { throw 'Vulkan SDK 1.4.341.1 Windows x64 is required.' }
$SdkRoot = (Resolve-Path -LiteralPath $SdkRoot).Path
$lockPath = Join-Path $PSScriptRoot '../../renderer/shaders/dxil-toolchain.json'
$lock = Get-Content -LiteralPath $lockPath -Raw -Encoding utf8 | ConvertFrom-Json
foreach ($file in @('spirv-val.exe', 'spirv-cross.exe', 'dxc.exe', 'dxcompiler.dll')) {
    $path = Join-Path $SdkRoot ('Bin/' + $file)
    if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or (Hash $path) -cne $lock.files.$file) {
        throw "Missing or changed pinned tool: $path"
    }
}
$validator = Join-Path $SdkRoot 'Bin/spirv-val.exe'
$cross = Join-Path $SdkRoot 'Bin/spirv-cross.exe'
$dxc = Join-Path $SdkRoot 'Bin/dxc.exe'
if ((Run $cross @('--revision') -Revision).Trim() -cne $lock.spirv_cross_revision -or
    (Run $dxc @('--version') -Revision).Trim() -cne $lock.dxc_version) {
    throw 'Unexpected SPIRV-Cross or DXC version.'
}
$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ('renderer-bistro-pbr-' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $temporaryRoot
try {
    $publish = [ordered]@{}
    $artifacts = [ordered]@{}
    # Vulkan 1.1 is the DXC target that emits SPIR-V 1.3. Validate it against
    # the Renderer device environment below. Use Vulkan's default block layout.
    $spirvArgs = @('-spirv', '-fspv-target-env=vulkan1.1', '-fvk-stage-io-order=decl')
    $sourceDir = Join-Path $temporaryRoot 'source'
    $null = New-Item -ItemType Directory -Path $sourceDir
    $includes = [ordered]@{}
    foreach ($file in (Get-ChildItem -LiteralPath $PSScriptRoot -File | Sort-Object Name)) {
        if ($file.Extension -cnotin @('.hlsl', '.hlsli')) { continue }
        $canonical = Join-Path $sourceDir $file.Name
        Write-Text $canonical ([IO.File]::ReadAllText($file.FullName))
        if ($file.Extension -ceq '.hlsli') { $includes[$file.Name] = Hash $canonical }
    }
    $shaders = @(
        'pbr.vert', 'pbr.frag', 'shadow.vert', 'shadow.frag', 'bloom_extract.comp',
        'bloom_downsample.comp', 'bloom_blur_tent.comp', 'bloom_blur_box.comp', 'bloom_combine.comp',
        'gbuffer.vert', 'gbuffer.frag', 'deferred_lighting.frag',
        'forward_plus.comp', 'pbr_all_lights.frag', 'pbr_forward_plus.frag',
        'frustum_visibility.comp', 'pbr_instancing.vert',
        'pbr_transformed.vert', 'pbr_transformed.frag', 'shadow_transformed.vert'
    )
    foreach ($name in $shaders) {
        $stage = $name.Split('.')[-1]
        $shaderSource = $name
        $defines = @()
        switch -CaseSensitive ($name) {
            'pbr_all_lights.frag' { $shaderSource = 'pbr.frag'; $defines = @('BISTRO_LOCAL_LIGHTS=1') }
            'pbr_forward_plus.frag' { $shaderSource = 'pbr.frag'; $defines = @('BISTRO_LOCAL_LIGHTS=2') }
            'pbr_instancing.vert' { $shaderSource = 'pbr.vert'; $defines = @('BISTRO_INSTANCING=1') }
            'pbr_transformed.frag' { $shaderSource = 'pbr.frag'; $defines = @('BISTRO_TRANSFORMED_DIRECTIONS=1') }
        }
        $shaderSource += '.hlsl'
        $source = Join-Path $sourceDir $shaderSource
        $spirv = Join-Path $temporaryRoot ($name + '.spv')
        $dxil = Join-Path $temporaryRoot ($name + '.dxil')
        $profile = switch -CaseSensitive ($stage) {
            'vert' { 'vs_6_0' }
            'frag' { 'ps_6_0' }
            'comp' { 'cs_6_0' }
            default { throw "Unexpected shader stage: $stage" }
        }
        $arguments = @($lock.dxc_arguments) + @('-T', $profile)
        foreach ($define in $defines) { $arguments += @('-D', $define) }
        $arguments += $source
        $vulkanArguments = $arguments + $spirvArgs
        $checkClip = $name -cin @('pbr.vert', 'gbuffer.vert')
        if ($checkClip) {
            $assembly = Join-Path $temporaryRoot ($name + '.spvasm')
            $vulkanArguments += @('-Fc', $assembly)
        }
        $null = Run $dxc ($vulkanArguments + @('-Fo', $spirv))
        if ([BitConverter]::ToUInt32([IO.File]::ReadAllBytes($spirv), 4) -ne 0x00010300) {
            throw "Unexpected SPIR-V version for $name; expected 1.3"
        }
        $null = Run $validator @('--target-env', 'vulkan1.3', $spirv)
        $null = Run $dxc ($arguments + @('-Fo', $dxil))
        $dump = Run $dxc @('-dumpbin', $dxil)
        if ($checkClip) { Test-ClipArithmetic $name $dump ([IO.File]::ReadAllText($assembly)) }
        $end = $dump.IndexOf('target datalayout = ', [StringComparison]::Ordinal)
        if ($end -lt 0 -or $dump -notmatch '!dx\.valver = ' -or $dump -notmatch '; shader hash: [0-9a-f]{32}') {
            throw "DXIL validator/reflection metadata missing for $name"
        }
        $reflection = [regex]::Replace($dump.Substring(0, $end), '(?m)[ \t]+$', '')
        $reflectionPath = Join-Path $temporaryRoot ($name + '.reflection.txt')
        Write-Text $reflectionPath $reflection
        $hasInline = $name -cin @(
            'shadow.vert', 'pbr_instancing.vert', 'pbr_transformed.vert', 'shadow_transformed.vert'
        )
        if (($reflection -match '\bcb7\b') -ne $hasInline) {
            throw "Unexpected inline cbuffer contract in $name reflection"
        }
        $spirvReflection = Run $cross @($spirv, '--reflect')
        $jsonPath = Join-Path $temporaryRoot ($name + '.spirv-reflection.json')
        Write-Text $jsonPath $spirvReflection
        $hashes = [ordered]@{ source = $shaderSource; source_utf8_lf = Hash $source; defines = $defines }
        foreach ($extension in @('.spv', '.dxil', '.reflection.txt', '.spirv-reflection.json')) {
            $generated = Join-Path $temporaryRoot ($name + $extension)
            $target = if ($extension -ceq '.spv') {
                Join-Path $PSScriptRoot ($name + $extension)
            } else { Join-Path $PSScriptRoot ('dxil/' + $name + $extension) }
            $publish[$generated] = $target
            $hashes[$extension.TrimStart('.')] = Hash $generated
        }
        $artifacts[$name] = $hashes
    }
    # The runner reports the hash of this manifest for both display routes.
    # Include the common vertex and standard output shader artifacts it runs.
    $rendererShaders = Join-Path $PSScriptRoot '../../renderer/shaders/dxil/manifest.json'
    $manifest = [ordered]@{
        schema = 2
        toolchain_sha256 = Hash $lockPath
        renderer_shaders_sha256 = Hash $rendererShaders
        spirv_arguments = $spirvArgs
        inline_bytes = 4
        placement_inline_bytes = 68
        artifacts = $artifacts
        includes = $includes
    }
    $manifestPath = Join-Path $temporaryRoot 'manifest.json'
    Write-Text $manifestPath (($manifest | ConvertTo-Json -Depth 5 -Compress) + "`n")
    $publish[$manifestPath] = Join-Path $PSScriptRoot 'manifest.json'
    foreach ($item in $publish.GetEnumerator()) {
        if ($Verify -and (-not (Test-Path -LiteralPath $item.Value -PathType Leaf) -or (Hash $item.Key) -cne (Hash $item.Value))) {
            throw "Bistro shader artifact differs or is missing: $($item.Value)"
        }
    }
    if (-not $Verify) {
        foreach ($item in $publish.GetEnumerator()) {
            $null = New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($item.Value)) -Force
            Copy-Item -LiteralPath $item.Key -Destination $item.Value -Force
        }
    }
    foreach ($item in $artifacts.GetEnumerator()) { Write-Output "$($item.Key) DXIL $($item.Value.dxil)" }
} finally {
    $resolved = [IO.Path]::GetFullPath($temporaryRoot)
    $tempParent = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    if ([IO.Path]::GetDirectoryName($resolved).TrimEnd('\') -cne $tempParent.TrimEnd('\') -or
        [IO.Path]::GetFileName($resolved) -notmatch '^renderer-bistro-pbr-[0-9a-f]{32}$') {
        throw "Refusing to remove unexpected temporary path: $resolved"
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
