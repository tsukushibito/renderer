# CPU-only regression checks against real checked-in compiler reflection.
# No Vulkan SDK, GPU, DXC, or PowerShell test-module dependency is required.
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'managed-abi.ps1')
$verify = ${function:Assert-BufferCopyAbi}
$json = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/uniform.comp.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/uniform.comp.reflection.txt') -Raw
Assert-BufferCopyAbi ($json | ConvertFrom-Json) $reflection
# Native tools use LF; a Windows text reader must not affect ABI interpretation.
Assert-BufferCopyAbi ($json | ConvertFrom-Json) ($reflection.Replace("`r`n", "`n").Replace("`n", "`r`n"))

function Expect-Rejected([string]$Name, [scriptblock]$Mutate) {
    $spirv = $json | ConvertFrom-Json
    $changedDxil = & $Mutate $spirv $reflection
    $dxil = if ($null -eq $changedDxil) { $reflection } else { $changedDxil }
    $rejected = $false
    try { & $verify $spirv $dxil } catch { $rejected = $true }
    if (-not $rejected) { throw "ABI mismatch was accepted: $Name" }
}

# The contract must reject each independently changed resource identity and ABI;
# a matching artifact hash alone is not a semantic interface check.
Expect-Rejected 'uniform descriptor set' { param($s) $s.ubos[0].set = 1 }
Expect-Rejected 'uniform binding slot' { param($s) $s.ubos[0].binding = 1 }
Expect-Rejected 'uniform byte size' { param($s) $s.ubos[0].block_size = 32 }
Expect-Rejected 'uniform member offset' {
    param($s) $s.types.PSObject.Properties[$s.ubos[0].type].Value.members[0].offset = 16
}
Expect-Rejected 'uniform scalar type' {
    param($s) $s.types.PSObject.Properties[$s.ubos[0].type].Value.members[0].type = 'vec4'
}
Expect-Rejected 'descriptor array' {
    param($s) $s.ubos[0] | Add-Member -NotePropertyName array -NotePropertyValue @(2)
}
Expect-Rejected 'private output descriptor set' { param($s) $s.ssbos[0].set = 0 }
Expect-Rejected 'private output mutability' { param($s) $s.ssbos[0].writeonly = $false }
Expect-Rejected 'extra uniform resource' { param($s) $s.ubos = @($s.ubos[0], $s.ubos[0]) }
Expect-Rejected 'extra image resource' {
    param($s) $s | Add-Member -NotePropertyName images -NotePropertyValue @([pscustomobject]@{ set = 2; binding = 0 })
}
Expect-Rejected 'specialized local size' { param($s) $s.entryPoints[0].workgroup_size_is_spec_constant_id[0] = $true }
Expect-Rejected 'DXIL uniform register' { param($s, $d) $d.Replace('cb0', 'cb1') }
Expect-Rejected 'DXIL output register space' { param($s, $d) $d.Replace('u0,space1', 'u0,space0') }
Expect-Rejected 'DXIL uniform member offset' {
    param($s, $d) [regex]::Replace($d, '(uint4[^\r\n]*Offset:\s*)0', '${1}4')
}
Expect-Rejected 'DXIL uniform byte size' {
    param($s, $d) [regex]::Replace($d, 'Size:\s*16', 'Size: 32')
}
Expect-Rejected 'DXIL uniform scalar type' { param($s, $d) $d.Replace('uint4', 'float4') }
Expect-Rejected 'DXIL local size' { param($s, $d) $d.Replace('NumThreads=(1,1,1)', 'NumThreads=(2,1,1)') }
Expect-Rejected 'extra DXIL resource' {
    param($s, $d) $d + "`n; extra UAV byte r/w U1 u1,space1 1`n"
}

# The sampled asset has a genuinely active separate sampler, not texelFetch
# with a sampler declaration removed by optimization. Reuse the rejection
# helper, while keeping each asset's ABI checks explicit and narrowly scoped.
$verify = ${function:Assert-SampledAbi}
$json = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/sampled.comp.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/sampled.comp.reflection.txt') -Raw
Assert-SampledAbi ($json | ConvertFrom-Json) $reflection
Assert-SampledAbi ($json | ConvertFrom-Json) ($reflection.Replace("`r`n", "`n").Replace("`n", "`r`n"))
Expect-Rejected 'sampled image slot' { param($s) $s.separate_images[0].binding = 1 }
Expect-Rejected 'sampled image scalar type' { param($s) $s.separate_images[0].type = 'utexture2D' }
Expect-Rejected 'sampled image array' {
    param($s) $s.separate_images[0] | Add-Member -NotePropertyName array -NotePropertyValue @(2)
}
Expect-Rejected 'separate sampler slot' { param($s) $s.separate_samplers[0].binding = 0 }
Expect-Rejected 'missing separate sampler' { param($s) $s.separate_samplers = @() }
Expect-Rejected 'sampled output set' { param($s) $s.ssbos[0].set = 0 }
Expect-Rejected 'sampled output byte size' { param($s) $s.ssbos[0].block_size = 32 }
Expect-Rejected 'sampled output member offset' {
    param($s) $s.types.PSObject.Properties[$s.ssbos[0].type].Value.members[0].offset = 16
}
Expect-Rejected 'sampled output scalar type' {
    param($s) $s.types.PSObject.Properties[$s.ssbos[0].type].Value.members[0].type = 'vec4'
}
Expect-Rejected 'sampled output mutability' { param($s) $s.ssbos[0].writeonly = $false }
Expect-Rejected 'sampled unexpected uniform' {
    param($s) $s | Add-Member -NotePropertyName ubos -NotePropertyValue @([pscustomobject]@{ set = 2; binding = 0 })
}
Expect-Rejected 'DXIL sampler optimized away' {
    param($s, $d) [regex]::Replace($d, '(?m)^;[ \t]+[^\r\n]+[ \t]+sampler[^\r\n]*\r?\n', '')
}
Expect-Rejected 'DXIL separate sampler register' { param($s, $d) $d.Replace('s1', 's0') }
Expect-Rejected 'DXIL sampled image scalar type' {
    param($s, $d) [regex]::Replace($d, '(?m)(^;[^\r\n]+\s+texture\s+)f32', '${1}u32')
}
Expect-Rejected 'DXIL sampled image dimension' {
    param($s, $d) [regex]::Replace($d, '(?m)(^;[^\r\n]+\s+texture\s+f32\s+)2d', '${1}3d')
}
Expect-Rejected 'DXIL sampled output register space' { param($s, $d) $d.Replace('u0,space1', 'u0,space0') }
Expect-Rejected 'DXIL extra sampled resource' {
    param($s, $d) $d + "`n; extra sampler NA NA S1 s2 1`n"
}

# Keep the documented decimal output and the exact byte fixture consistent.
# Sampling/filter/address correctness still requires the native GPU round-trip.
$contracts = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil-contracts.json') -Raw | ConvertFrom-Json
$sampled = $contracts.shaders.'sampled.comp'
$rgba = $sampled.input_rgba8_row_major
for ($index = 0; $index -lt 4; $index++) {
    $offset = $index * 4
    $packed = [uint32]$rgba[$offset] -bor ([uint32]$rgba[$offset + 1] -shl 8) -bor
        ([uint32]$rgba[$offset + 2] -shl 16) -bor ([uint32]$rgba[$offset + 3] -shl 24)
    if ($packed -ne $sampled.output_u32[$index]) { throw "Sampled RGBA/uint packing differs at texel $index" }
}

# The float asset reuses the sampled interface but stores four complete bitcast
# vectors. Check the new 64-byte array ABI rather than repeating the unchanged
# image/sampler register matrix already exercised above.
$verify = { param($s, $d) Assert-SampledAbi $s $d -FloatOutput }
$json = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/sampled_float.comp.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/sampled_float.comp.reflection.txt') -Raw
Assert-SampledAbi ($json | ConvertFrom-Json) $reflection -FloatOutput
Expect-Rejected 'float output uses old packed byte size' { param($s) $s.ssbos[0].block_size = 16 }
Expect-Rejected 'float output array length' {
    param($s) $s.types.PSObject.Properties[$s.ssbos[0].type].Value.members[0].array = @(3)
}
Expect-Rejected 'float output array stride' {
    param($s) $s.types.PSObject.Properties[$s.ssbos[0].type].Value.members[0].array_stride = 32
}
Expect-Rejected 'float output specialized array count' {
    param($s) $s.types.PSObject.Properties[$s.ssbos[0].type].Value.members[0].array_size_is_literal = @($false)
}
# Indexed indirect output must match both native argument encodings.
$verify = ${function:Assert-IndexedIndirectAbi}
$json = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/indexed_indirect.comp.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/indexed_indirect.comp.reflection.txt') -Raw
Assert-IndexedIndirectAbi ($json | ConvertFrom-Json) $reflection
Expect-Rejected 'indexed indirect input mutability' { param($s) $s.ssbos[0].readonly = $false }
Expect-Rejected 'indexed indirect argument stride' {
    param($s) $s.types.PSObject.Properties[$s.ssbos[2].type].Value.members[0].array_stride = 24
}
Expect-Rejected 'indexed indirect base vertex signedness' {
    param($s)
    $entries = $s.types.PSObject.Properties[$s.ssbos[2].type].Value.members[0]
    $s.types.PSObject.Properties[$entries.type].Value.members[3].type = 'uint'
}
Expect-Rejected 'indexed indirect output register' { param($s, $d) $d.Replace('u2', 'u3') }
Expect-Rejected 'indexed indirect local size' { param($s, $d) $d.Replace('NumThreads=(64,1,1)', 'NumThreads=(32,1,1)') }

# Storage input reuses the buffer-copy layout and output checks above. Exercise
# the new read-only qualifier and raw SRV branch, without duplicating that matrix.
$verify = { param($s, $d) Assert-BufferCopyAbi $s $d -StorageInput }
$json = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/storage_read.comp.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/storage_read.comp.reflection.txt') -Raw
Assert-BufferCopyAbi ($json | ConvertFrom-Json) $reflection -StorageInput
Expect-Rejected 'storage input mutability' {
    param($s) ($s.ssbos | Where-Object { $_.set -eq 0 }).readonly = $false
}
Expect-Rejected 'DXIL storage input register' { param($s, $d) $d.Replace('t0', 't1') }
Expect-Rejected 'DXIL storage input is writable' { param($s, $d) $d.Replace('r/o', 'r/w') }
Expect-Rejected 'DXIL storage input is typed' {
    param($s, $d) [regex]::Replace($d, '(?m)(^;[^\r\n]+\s+texture\s+)byte', '${1}u32')
}
# The graphics resources belong to different shader stages. Verify each real
# module independently, including the native Uniform/inline register distinction.
$verify = { param($s, $d) Assert-TexturedRasterAbi $s $d -Vertex }
$json = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/textured.vert.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/textured.vert.reflection.txt') -Raw
Assert-TexturedRasterAbi ($json | ConvertFrom-Json) $reflection -Vertex
Assert-TexturedRasterAbi ($json | ConvertFrom-Json) ($reflection.Replace("`r`n", "`n").Replace("`n", "`r`n")) -Vertex
Expect-Rejected 'vertex resource reflected as fragment' { param($s) $s.entryPoints[0].mode = 'frag' }
Expect-Rejected 'vertex Uniform slot' { param($s) $s.ubos[0].binding = 1 }
Expect-Rejected 'vertex Uniform prefix size' { param($s) $s.ubos[0].block_size = 32 }
Expect-Rejected 'vertex Uniform scalar type' {
    param($s) $s.types.PSObject.Properties[$s.ubos[0].type].Value.members[0].type = 'uvec4'
}
Expect-Rejected 'vertex inline matrix stride' {
    param($s) $s.types.PSObject.Properties[$s.push_constants[0].type].Value.members[0].matrix_stride = 32
}
Expect-Rejected 'vertex inline row-major matrix' {
    param($s) $s.types.PSObject.Properties[$s.push_constants[0].type].Value.members[0] |
        Add-Member -NotePropertyName row_major -NotePropertyValue $true
}
Expect-Rejected 'vertex additional storage side effect' {
    param($s) $s | Add-Member -NotePropertyName ssbos -NotePropertyValue @([pscustomobject]@{ set = 2; binding = 0 })
}
Expect-Rejected 'DXIL inline and Uniform register alias' { param($s, $d) $d.Replace('cb1', 'cb0') }
Expect-Rejected 'DXIL vertex Uniform prefix size' {
    param($s, $d) [regex]::Replace($d, 'Size:\s*16', 'Size: 32')
}
Expect-Rejected 'DXIL vertex attribute semantic' { param($s, $d) $d.Replace('TEXCOORD                 1', 'TEXCOORD                 7') }
Expect-Rejected 'DXIL vertex attribute type' { param($s, $d) $d.Replace('NONE   float', 'NONE   uint') }
$verify = { param($s, $d) Assert-TexturedRasterAbi $s $d }
$json = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/textured.frag.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/textured.frag.reflection.txt') -Raw
Assert-TexturedRasterAbi ($json | ConvertFrom-Json) $reflection
Expect-Rejected 'fragment image stage' { param($s) $s.entryPoints[0].mode = 'vert' }
Expect-Rejected 'fragment image group' { param($s) $s.separate_images[0].set = 0 }
Expect-Rejected 'fragment image dimension' { param($s) $s.separate_images[0].type = 'texture3D' }
Expect-Rejected 'fragment image descriptor array' {
    param($s) $s.separate_images[0] | Add-Member -NotePropertyName array -NotePropertyValue @(2)
}
Expect-Rejected 'fragment extra Uniform' {
    param($s) $s | Add-Member -NotePropertyName ubos -NotePropertyValue @([pscustomobject]@{ set = 0; binding = 0 })
}
Expect-Rejected 'DXIL fragment sampler register space' { param($s, $d) $d.Replace('s1,space1', 's1,space0') }
Expect-Rejected 'DXIL fragment sampler removed' {
    param($s, $d) [regex]::Replace($d, '(?m)^;[ \t]+[^\r\n]+[ \t]+sampler[^\r\n]*\r?\n', '')
}
Expect-Rejected 'DXIL fragment varying components' { param($s, $d) $d.Replace('1   xy ', '1   xyz') }
Expect-Rejected 'DXIL fragment color target semantic' { param($s, $d) $d.Replace('SV_Target                0', 'SV_Target                1') }
# Caller-owned programs use the same pinned compiler outputs, while remaining
# outside the Renderer Program registry. Exercise the new typed-image ABI and
# readonly-image UAV mapping with independent semantic mismatch controls.
$external = Join-Path $PSScriptRoot '../tests/support/shaders/dxil'
$verify = { param($s, $d) Assert-HdrComputeAbi $s $d }
$json = Get-Content -LiteralPath (Join-Path $external 'compute_hdr_sampled.comp.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $external 'compute_hdr_sampled.comp.reflection.txt') -Raw
Assert-HdrComputeAbi ($json | ConvertFrom-Json) $reflection
Assert-HdrComputeAbi ($json | ConvertFrom-Json) ($reflection.Replace("`r`n", "`n").Replace("`n", "`r`n"))
Expect-Rejected 'HDR local workgroup size' { param($s) $s.entryPoints[0].workgroup_size[0] = 1 }
Expect-Rejected 'HDR output typed format' { param($s) $s.images[0].format = 'rgba8' }
Expect-Rejected 'HDR output not writeonly' { param($s) $s.images[0].writeonly = $false }
Expect-Rejected 'HDR Uniform byte size' { param($s) $s.ubos[0].block_size = 32 }
Expect-Rejected 'HDR Uniform scalar type' {
    param($s) $s.types.PSObject.Properties[$s.ubos[0].type].Value.members[0].type = 'uvec4'
}
Expect-Rejected 'HDR unexpected storage buffer' {
    param($s) $s | Add-Member -NotePropertyName ssbos -NotePropertyValue @([pscustomobject]@{ set = 0; binding = 4 })
}
Expect-Rejected 'DXIL HDR output space' { param($s, $d) $d.Replace('u0,space1', 'u0,space2') }
Expect-Rejected 'DXIL HDR Uniform binding' { param($s, $d) $d.Replace('cb2', 'cb0') }
Expect-Rejected 'DXIL HDR image type' {
    param($s, $d) [regex]::Replace($d, '(?m)(^;[^\r\n]+\s+UAV\s+)f32', '${1}u32')
}
$verify = { param($s, $d) Assert-HdrComputeAbi $s $d -StorageInput }
$json = Get-Content -LiteralPath (Join-Path $external 'compute_hdr_storage.comp.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $external 'compute_hdr_storage.comp.reflection.txt') -Raw
Assert-HdrComputeAbi ($json | ConvertFrom-Json) $reflection -StorageInput
Expect-Rejected 'HDR storage input image qualifier' {
    param($s) ($s.images | Where-Object { $_.set -eq 0 }).readonly = $false
}
Expect-Rejected 'HDR storage input image slot' {
    param($s) ($s.images | Where-Object { $_.set -eq 0 }).binding = 1
}
Expect-Rejected 'HDR storage input buffer byte size' { param($s) $s.ssbos[0].block_size = 32 }
Expect-Rejected 'HDR storage input buffer mutability' { param($s) $s.ssbos[0].readonly = $false }
Expect-Rejected 'DXIL readonly storage image treated as SRV' {
    param($s, $d) [regex]::Replace($d, '(?m)(^;[^\r\n]+\s+)UAV(\s+f32\s+2d\s+\S+\s+u0\s+1\s*$)', '${1}texture${2}')
}
Expect-Rejected 'DXIL HDR storage input buffer register' { param($s, $d) $d.Replace('t1', 't0') }
$verify = { param($s, $d) Assert-SdrOutputAbi $s $d -Exposure }
$json = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/tone_neutral.frag.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/tone_neutral.frag.reflection.txt') -Raw
Assert-SdrOutputAbi ($json | ConvertFrom-Json) $reflection -Exposure
Expect-Rejected 'Neutral exposure float offset' {
    param($s) $s.types.PSObject.Properties[$s.push_constants[0].type].Value.members[0].offset = 4
}
Expect-Rejected 'DXIL Neutral exposure register' { param($s, $d) $d.Replace('cb0', 'cb2') }
foreach ($program in @('sdr.frag', 'sdr_linear.frag', 'sdr_fxaa.frag', 'sdr_fxaa_linear.frag')) {
    $outputReflection = Get-Content -LiteralPath (Join-Path $PSScriptRoot ("dxil/$program.spirv-reflection.json")) -Raw
    $outputDxil = Get-Content -LiteralPath (Join-Path $PSScriptRoot ("dxil/$program.reflection.txt")) -Raw
    Assert-SdrOutputAbi ($outputReflection | ConvertFrom-Json) $outputDxil
}
Write-Output 'Managed Compute, indexed indirect, HDR Compute, descriptor-bound Raster and standard output ABI checks passed.'
