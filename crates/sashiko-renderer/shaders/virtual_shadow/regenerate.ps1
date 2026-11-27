param([string]$SdkRoot = $env:VULKAN_SDK, [switch]$Verify)
$ErrorActionPreference = 'Stop'
$variants = foreach ($name in 'requests.comp', 'count.comp', 'assign.comp', 'page_state.comp', 'clear_tile.vert', 'resolve.comp') {
    @{ Name = $name; Source = "$name.hlsl"; Defines = @() }
}
& "$PSScriptRoot/../../../../tools/shaders/compile_hlsl.ps1" -ShaderRoot $PSScriptRoot -Variants $variants -SdkRoot $SdkRoot -Verify:$Verify
