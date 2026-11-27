param([string]$SdkRoot = $env:VULKAN_SDK, [switch]$Verify)
$ErrorActionPreference = 'Stop'
$variants = foreach ($name in 'requests.comp', 'count.comp', 'assign.comp', 'page_state.comp', 'clear_tile.vert', 'resolve.comp') {
    @{ Name = $name; Source = "$name.hlsl"; Defines = @() }
}
$variants += @{ Name = 'caster.vert'; Source = 'caster.vert.hlsl'; Defines = @(); HasInline = $true }
$variants += @{ Name = 'caster.frag'; Source = 'caster.frag.hlsl'; Defines = @() }
$variants += @{ Name = 'camera_depth.vert'; Source = 'camera_depth.vert.hlsl'; Defines = @(); HasInline = $true }
$shared = @("$PSScriptRoot/../raster_abi.hlsli", "$PSScriptRoot/../raster_vertex.hlsli", "$PSScriptRoot/../material_sampling.hlsli")
& "$PSScriptRoot/../../../../tools/shaders/compile_hlsl.ps1" -ShaderRoot $PSScriptRoot -Variants $variants -SharedIncludes $shared -SdkRoot $SdkRoot -Verify:$Verify
