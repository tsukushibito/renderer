# Regenerate the offline probe and surface bake shaders, or compare their bytes with -Verify.
[CmdletBinding()]
param([string]$SdkRoot = $env:VULKAN_SDK, [switch]$Verify)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$source = 'bake_transport.comp.hlsl'
$variants = @(
    @{ Name = 'bake_transport.comp'; Source = $source; Defines = @() }
    @{ Name = 'bake_transport_l2.comp'; Source = $source; Defines = @('BAKE_L2=1') }
    @{ Name = 'bake_surface.comp'; Source = $source; Defines = @('BAKE_SURFACE=1') }
    @{
        Name = 'bake_surface_rt.comp'
        Source = $source
        Defines = @('BAKE_SURFACE=1', 'BAKE_RAY_QUERY=1')
        Profile = 'cs_6_5'
        SpirvTarget = 'vulkan1.2'
        SpirvOptions = @('-fspv-extension=SPV_KHR_ray_query', '-fvk-use-dx-layout')
        SpirvVersion = 0x00010500
    }
    @{
        Name = 'film_primary.comp'
        Source = 'film_primary.comp.hlsl'
        Defines = @()
        Profile = 'cs_6_5'
        SpirvTarget = 'vulkan1.2'
        SpirvOptions = @('-fspv-extension=SPV_KHR_ray_query', '-fvk-use-dx-layout')
        SpirvVersion = 0x00010500
    }
)
& "$PSScriptRoot/../../../tools/shaders/compile_hlsl.ps1" -ShaderRoot $PSScriptRoot -Variants $variants `
    -SdkRoot $SdkRoot -Verify:$Verify
