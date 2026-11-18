# Generate consumer-owned Raster fixtures with the repository's pinned SDK.
# -Verify regenerates and compares every artifact without rewriting the checkout.
[CmdletBinding()]
param([string]$SdkRoot = $env:VULKAN_SDK, [switch]$Verify, [switch]$RasterBatches)
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
$lockPath = Join-Path $PSScriptRoot '../../../shaders/dxil-toolchain.json'
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
$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ('renderer-custom-raster-' + [Guid]::NewGuid().ToString('N'))
$null = New-Item -ItemType Directory -Path $temporaryRoot
try {
    $publish = [ordered]@{}
    $artifacts = [ordered]@{}
    $shaders = if ($RasterBatches) {
        [ordered]@{ 'raster_batches.vert' = $false; 'raster_batches_inline.frag' = $true; 'raster_batches_uniform.frag' = $false }
    } else {
        [ordered]@{ 'custom_raster.vert' = $true; 'custom_raster.frag' = $true }
    }
    foreach ($shader in $shaders.GetEnumerator()) {
        $name = $shader.Key
        $stage = $name.Split('.')[-1]
        $source = Join-Path $PSScriptRoot $name
        $spirv = Join-Path $temporaryRoot ($name + '.spv')
        $hlsl = Join-Path $temporaryRoot ($name + '.hlsl')
        $dxil = Join-Path $temporaryRoot ($name + '.dxil')
        $null = Run $glslc @('--target-env=vulkan1.3', '--target-spv=spv1.3', '-std=450core', '-fentry-point=main', '-O', '-Werror', $source, '-o', $spirv)
        $null = Run $validator @('--target-env', 'vulkan1.3', $spirv)
        $crossArgs = @($spirv) + @($lock.spirv_cross_arguments) + @('--stage', $stage, '--output', $hlsl)
        if ($stage -ceq 'vert') { $crossArgs += '--flip-vert-y' }
        $null = Run $cross $crossArgs
        # SPIRV-Cross leaves the push constant cbuffer unbound. Give only that
        # cbuffer one explicit common slot in both stages; descriptor cbuffers
        # already have register declarations and cannot match this expression.
        $text = [IO.File]::ReadAllText($hlsl).Replace("`r`n", "`n")
        $pattern = '(?m)^cbuffer ([A-Za-z_][A-Za-z_0-9]*)$'
        if ([regex]::Matches($text, $pattern).Count -ne [int]$shader.Value) { throw "Unexpected inline cbuffer count in $name" }
        $text = [regex]::Replace($text, $pattern, 'cbuffer $1 : register(b7, space0)')
        Write-Text $hlsl $text
        $profile = if ($stage -ceq 'vert') { 'vs_6_0' } else { 'ps_6_0' }
        $null = Run $dxc (@($lock.dxc_arguments) + @('-T', $profile, $hlsl, '-Fo', $dxil))
        $dump = Run $dxc @('-dumpbin', $dxil)
        $end = $dump.IndexOf('target datalayout = ', [StringComparison]::Ordinal)
        if ($end -lt 0 -or $dump -notmatch '!dx\.valver = ' -or $dump -notmatch '; shader hash: [0-9a-f]{32}') {
            throw "DXIL validator/reflection metadata missing for $name"
        }
        $reflection = [regex]::Replace($dump.Substring(0, $end), '(?m)[ \t]+$', '')
        $reflectionPath = Join-Path $temporaryRoot ($name + '.reflection.txt')
        Write-Text $reflectionPath $reflection
        if ($shader.Value -and $reflection -notmatch '\bcb7\b') { throw "Inline cbuffer b7 missing from $name reflection" }
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
    $manifest = [ordered]@{ schema = 1; toolchain_sha256 = Hash $lockPath; inline_dxil_register = 7; inline_dxil_space = 0; artifacts = $artifacts }
    $manifestName = if ($RasterBatches) { 'raster_batches-manifest.json' } else { 'custom_raster-manifest.json' }
    $manifestPath = Join-Path $temporaryRoot $manifestName
    Write-Text $manifestPath (($manifest | ConvertTo-Json -Depth 5 -Compress) + "`n")
    $publish[$manifestPath] = Join-Path $PSScriptRoot $manifestName
    foreach ($item in $publish.GetEnumerator()) {
        if ($Verify -and (-not (Test-Path -LiteralPath $item.Value -PathType Leaf) -or (Hash $item.Key) -cne (Hash $item.Value))) {
            throw "Custom Raster fixture differs or is missing: $($item.Value)"
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
        [IO.Path]::GetFileName($resolved) -notmatch '^renderer-custom-raster-[0-9a-f]{32}$') {
        throw "Refusing to remove unexpected temporary path: $resolved"
    }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
