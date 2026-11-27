param([string]$SdkRoot = $env:VULKAN_SDK, [switch]$Verify)
$ErrorActionPreference = 'Stop'
$variants = foreach ($name in 'requests.comp', 'count.comp', 'assign.comp', 'page_state.comp', 'clear_tile.vert', 'resolve.comp', 'pack_table.comp', 'invalidate.comp') {
    @{ Name = $name; Source = "$name.hlsl"; Defines = @() }
}
$variants += @{ Name = 'caster.vert'; Source = 'caster.vert.hlsl'; Defines = @(); HasInline = $true }
$variants += @{ Name = 'caster.frag'; Source = 'caster.frag.hlsl'; Defines = @() }
$variants += @{ Name = 'camera_depth.vert'; Source = 'camera_depth.vert.hlsl'; Defines = @(); HasInline = $true }
$shared = @('raster_abi','raster_vertex','material_sampling','virtual_shadow_lookup','virtual_shadow_math') | ForEach-Object { "$PSScriptRoot/../$_.hlsli" }
& "$PSScriptRoot/../../../../tools/shaders/compile_hlsl.ps1" -ShaderRoot $PSScriptRoot -Variants $variants -SharedIncludes $shared -SdkRoot $SdkRoot -Verify:$Verify
