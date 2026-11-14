# CPU-only regression checks against real checked-in compiler reflection.
# No Vulkan SDK, GPU, DXC, or PowerShell test-module dependency is required.
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'managed-abi.ps1')
$verify = ${function:Assert-UniformAbi}
$json = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/uniform.comp.spirv-reflection.json') -Raw
$reflection = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'dxil/uniform.comp.reflection.txt') -Raw
Assert-UniformAbi ($json | ConvertFrom-Json) $reflection
# Native tools use LF; a Windows text reader must not affect ABI interpretation.
Assert-UniformAbi ($json | ConvertFrom-Json) ($reflection.Replace("`r`n", "`n").Replace("`n", "`r`n"))

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
Write-Output 'Managed uniform/sampled SPIR-V/DXIL ABI contracts and mismatch rejections passed.'
