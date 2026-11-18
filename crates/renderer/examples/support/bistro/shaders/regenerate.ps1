# Generate consumer-owned Bistro PBR shaders with the repository's pinned SDK.
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
$lockPath = Join-Path $PSScriptRoot '../../../../shaders/dxil-toolchain.json'
$lock = Get-Content -LiteralPath $lockPath -Raw -Encoding utf8 | ConvertFrom-Json
foreach ($file in $lock.files.PSObject.Properties) {
    $path = Join-Path $SdkRoot ('Bin/' + $file.Name)
    if (-not (Test-Path -LiteralPath $path -PathType Leaf) -or (Hash $path) -cne $file.Value) {
        throw "Missing or changed pinned tool: $path"
    }
}
$glslc = Join-Path $SdkRoot 'Bin/glslc.exe'
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
    $shaders = @(
        'pbr.vert', 'pbr.frag', 'shadow.vert', 'shadow.frag', 'resolve.vert',
        'tone.frag', 'resolve.frag', 'resolve_aa.frag', 'bloom_extract.comp',
        'bloom_downsample.comp', 'bloom_blur_tent.comp', 'bloom_blur_box.comp', 'bloom_combine.comp',
        'resolve_linear.frag', 'resolve_aa_linear.frag',
        'gbuffer.vert', 'gbuffer.frag', 'deferred_lighting.frag',
        'forward_plus.comp', 'pbr_all_lights.frag', 'pbr_forward_plus.frag',
        'frustum_visibility.comp', 'pbr_instancing.vert'
    )
    foreach ($name in $shaders) {
        $stage = $name.Split('.')[-1]
        $shaderSource = $name -creplace '_linear\.frag$', '.frag'
        if ($name -cin @('pbr_all_lights.frag', 'pbr_forward_plus.frag')) { $shaderSource = 'pbr.frag' }
        if ($name -ceq 'pbr_instancing.vert') { $shaderSource = 'pbr.vert' }
        $source = Join-Path $PSScriptRoot $shaderSource
        $spirv = Join-Path $temporaryRoot ($name + '.spv')
        $hlsl = Join-Path $temporaryRoot ($name + '.hlsl')
        $dxil = Join-Path $temporaryRoot ($name + '.dxil')
        $glsl = @('--target-env=vulkan1.3', '--target-spv=spv1.3', '-std=450core', '-fentry-point=main', '-O', '-Werror')
        if ($name -cmatch '_linear\.frag$') { $glsl += '-DBISTRO_LINEAR_OUTPUT=1' }
        if ($name -ceq 'pbr_all_lights.frag') { $glsl += '-DBISTRO_LOCAL_LIGHTS=1' }
        if ($name -ceq 'pbr_forward_plus.frag') { $glsl += '-DBISTRO_LOCAL_LIGHTS=2' }
        if ($name -ceq 'pbr_instancing.vert') { $glsl += '-DBISTRO_INSTANCING=1' }
        $null = Run $glslc ($glsl + @($source, '-o', $spirv))
        $null = Run $validator @('--target-env', 'vulkan1.3', $spirv)
        $crossArgs = @($spirv) + @($lock.spirv_cross_arguments) + @('--stage', $stage, '--output', $hlsl)
        if ($stage -ceq 'vert') { $crossArgs += '--flip-vert-y' }
        $null = Run $cross $crossArgs
        # SPIRV-Cross leaves the sole push-constant cbuffer unbound. Reserve
        # b7,space0 for the shadow cascade index and instance placement.
        $text = [IO.File]::ReadAllText($hlsl).Replace("`r`n", "`n")
        $pattern = '(?m)^cbuffer ([A-Za-z_][A-Za-z_0-9]*)$'
        $hasInline = $name -cin @('shadow.vert', 'pbr_instancing.vert')
        if ($hasInline) {
            if ([regex]::Matches($text, $pattern).Count -ne 1) { throw "Inline cbuffer missing in $name" }
            $text = [regex]::Replace($text, $pattern, 'cbuffer $1 : register(b7, space0)')
        } elseif ([regex]::IsMatch($text, $pattern)) { throw "Unexpected inline cbuffer in $name" }
        # Hash and compile the same canonical text Git checks out on every OS.
        Write-Text $hlsl $text
        $profile = switch -CaseSensitive ($stage) {
            'vert' { 'vs_6_0' }
            'frag' { 'ps_6_0' }
            'comp' { 'cs_6_0' }
            default { throw "Unexpected shader stage: $stage" }
        }
        $null = Run $dxc (@($lock.dxc_arguments) + @('-T', $profile, $hlsl, '-Fo', $dxil))
        $dump = Run $dxc @('-dumpbin', $dxil)
        if ($name -ceq 'pbr.vert' -or $name -ceq 'gbuffer.vert') {
            # Retain the shared vertex FP32 sequence through DXIL lowering.
            # This deliberately restricts clip arithmetic, not global shader
            # optimization. A compiler/toolchain change must re-establish it.
            if ([regex]::Matches($dump, '(?m)^\s+%[0-9]+ = fmul float ').Count -ne 12 -or
                [regex]::Matches($dump, '(?m)^\s+%[0-9]+ = fadd float ').Count -ne 12 -or
                $dump -match '(?m)^\s+%[0-9]+ = f(?:mul|add) fast ' -or
                $dump -match '@dx\.op\.(?:dot[234]|tertiary)\.f32') {
                throw 'PBR clip transform lost separate, non-refactorable FP32 products/additions.'
            }
        }
        $end = $dump.IndexOf('target datalayout = ', [StringComparison]::Ordinal)
        if ($end -lt 0 -or $dump -notmatch '!dx\.valver = ' -or $dump -notmatch '; shader hash: [0-9a-f]{32}') {
            throw "DXIL validator/reflection metadata missing for $name"
        }
        $reflection = [regex]::Replace($dump.Substring(0, $end), '(?m)[ \t]+$', '')
        $reflectionPath = Join-Path $temporaryRoot ($name + '.reflection.txt')
        Write-Text $reflectionPath $reflection
        if (($reflection -match '\bcb7\b') -ne $hasInline) {
            throw "Unexpected inline cbuffer contract in $name reflection"
        }
        $spirvReflection = Run $cross @($spirv, '--reflect')
        $jsonPath = Join-Path $temporaryRoot ($name + '.spirv-reflection.json')
        Write-Text $jsonPath $spirvReflection
        $sourceBytes = [Text.Encoding]::UTF8.GetBytes([IO.File]::ReadAllText($source).Replace("`r`n", "`n"))
        $sha = [Security.Cryptography.SHA256]::Create()
        try { $sourceHash = ([BitConverter]::ToString($sha.ComputeHash($sourceBytes))).Replace('-', '').ToLowerInvariant() }
        finally { $sha.Dispose() }
        $hashes = [ordered]@{ source_utf8_lf = $sourceHash }
        foreach ($extension in @('.spv', '.hlsl', '.dxil', '.reflection.txt', '.spirv-reflection.json')) {
            $generated = Join-Path $temporaryRoot ($name + $extension)
            $target = if ($extension -ceq '.spv') {
                Join-Path $PSScriptRoot ($name + $extension)
            } else { Join-Path $PSScriptRoot ('dxil/' + $name + $extension) }
            $publish[$generated] = $target
            $hashes[$extension.TrimStart('.')] = Hash $generated
        }
        $artifacts[$name] = $hashes
    }
    $licenses = [ordered]@{}
    foreach ($license in @('FXAA-notices.txt', 'Godot-MIT.txt', 'Khronos-ToneMapping-Apache-2.0.txt')) {
        $licenses[$license] = Hash (Join-Path $PSScriptRoot ('licenses/' + $license))
    }
    $includes = [ordered]@{}
    foreach ($include in @('local_lights.glsl', 'local_light_pbr.glsl')) {
        $included = Join-Path $PSScriptRoot $include
        $canonical = Join-Path $temporaryRoot $include
        Write-Text $canonical ([IO.File]::ReadAllText($included))
        $includes[$include] = Hash $canonical
    }
    $manifest = [ordered]@{ schema = 1; toolchain_sha256 = Hash $lockPath; inline_bytes = 4; placement_inline_bytes = 68; artifacts = $artifacts; includes = $includes; licenses = $licenses }
    $manifestPath = Join-Path $temporaryRoot 'manifest.json'
    Write-Text $manifestPath (($manifest | ConvertTo-Json -Depth 5 -Compress) + "`n")
    $publish[$manifestPath] = Join-Path $PSScriptRoot 'manifest.json'
    foreach ($item in $publish.GetEnumerator()) {
        if ($Verify -and (-not (Test-Path -LiteralPath $item.Value -PathType Leaf) -or (Hash $item.Key) -cne (Hash $item.Value))) {
            throw "Bistro PBR asset differs or is missing: $($item.Value)"
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
