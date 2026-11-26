# Regenerate the packaged rendering shaders, or compare their bytes with -Verify.
[CmdletBinding()]
param([string]$SdkRoot = $env:VULKAN_SDK, [switch]$Verify)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$shaders = @(
    'pbr_lightmap_transformed.vert', 'pbr_lightmap_transformed_small.vert', 'gbuffer_lightmap_transformed.vert', 'gbuffer_lightmap_transformed_small.vert',
    'pbr_lightmap.vert', 'gbuffer_lightmap.vert', 'pick.vert', 'pick.frag', 'selection_outline.frag',
    'pbr.vert', 'pbr.frag', 'toon.frag', 'canvas.frag', 'canvas_fill.frag', 'shadow.vert', 'shadow.frag', 'bloom_extract_downsample.comp',
    'bloom_downsample.comp', 'bloom_blur_tent.comp', 'bloom_blur_box.comp', 'bloom_combine.comp',
    'gbuffer.vert', 'gbuffer.frag', 'deferred_lighting.frag',
    'forward_plus.comp', 'pbr_all_lights.frag', 'pbr_forward_plus.frag',
    'frustum_visibility.comp', 'skin.comp', 'skin_compare.vert', 'skin_compare_vertex.vert', 'pbr_instancing.vert',
    'pbr_transformed.vert', 'pbr_transformed_small.vert',
    'gbuffer_transformed.vert', 'gbuffer_transformed_small.vert',
    'shadow_transformed.vert', 'shadow_transformed_small.vert',
    'pbr_transformed.frag', 'pbr_forward_plus_transformed.frag', 'pbr_all_lights_transformed.frag',
    'deferred_local_lights.frag',
    'gbuffer_transformed.frag', 'shadow_transformed.frag'
)
$variants = foreach ($name in $shaders) {
    $stage = $name.Split('.')[-1]
    $shaderSource = $name
    $defines = @()
    switch -CaseSensitive ($name) {
        'skin_compare_vertex.vert' { $shaderSource = 'skin_compare.vert'; $defines = @('SKIN_IN_VERTEX=1') }
        'bloom_extract_downsample.comp' { $shaderSource = 'bloom_downsample.comp'; $defines = @('BLOOM_EXTRACT=1') }
        'pbr_all_lights.frag' { $shaderSource = 'pbr.frag'; $defines = @('BISTRO_LOCAL_LIGHTS=1') }
        'pbr_forward_plus.frag' { $shaderSource = 'pbr.frag'; $defines = @('BISTRO_LOCAL_LIGHTS=2') }
        'pbr_lightmap_transformed.vert' { $shaderSource = 'pbr_transformed.vert'; $defines = @('RENDERER_LIGHTMAP=1', 'RENDERER_MODEL_LIGHTMAP=1') }
        'pbr_lightmap_transformed_small.vert' { $shaderSource = 'pbr_transformed.vert'; $defines = @('RENDERER_LIGHTMAP=1', 'RENDERER_MODEL_LIGHTMAP=1', 'RENDERER_TRANSFORM_RECORDS=64') }
        'gbuffer_lightmap_transformed.vert' { $shaderSource = 'pbr_transformed.vert'; $defines = @('RENDERER_LIGHTMAP=1', 'RENDERER_MODEL_LIGHTMAP=1', 'RENDERER_GBUFFER_VERTEX=1') }
        'gbuffer_lightmap_transformed_small.vert' { $shaderSource = 'pbr_transformed.vert'; $defines = @('RENDERER_LIGHTMAP=1', 'RENDERER_MODEL_LIGHTMAP=1', 'RENDERER_TRANSFORM_RECORDS=64', 'RENDERER_GBUFFER_VERTEX=1') }
        'pbr_lightmap.vert' { $shaderSource = 'pbr.vert'; $defines = @('RENDERER_LIGHTMAP=1') }
        'gbuffer_lightmap.vert' { $shaderSource = 'gbuffer.vert'; $defines = @('RENDERER_LIGHTMAP=1') }
        'pbr_instancing.vert' { $shaderSource = 'pbr.vert'; $defines = @('BISTRO_INSTANCING=1') }
        'pbr_transformed_small.vert' { $shaderSource = 'pbr_transformed.vert'; $defines = @('RENDERER_TRANSFORM_RECORDS=64') }
        'gbuffer_transformed.vert' { $shaderSource = 'pbr_transformed.vert'; $defines = @('RENDERER_GBUFFER_VERTEX=1') }
        'gbuffer_transformed_small.vert' { $shaderSource = 'pbr_transformed.vert'; $defines = @('RENDERER_GBUFFER_VERTEX=1', 'RENDERER_TRANSFORM_RECORDS=64') }
        'shadow_transformed_small.vert' { $shaderSource = 'shadow_transformed.vert'; $defines = @('RENDERER_TRANSFORM_RECORDS=64') }
        'pbr_transformed.frag' { $shaderSource = 'pbr.frag'; $defines = @('BISTRO_TRANSFORMED_DIRECTIONS=1') }
        'pbr_all_lights_transformed.frag' { $shaderSource = 'pbr.frag'; $defines = @('BISTRO_LOCAL_LIGHTS=1', 'BISTRO_TRANSFORMED_DIRECTIONS=1') }
        'deferred_local_lights.frag' { $shaderSource = 'deferred_lighting.frag'; $defines = @('BISTRO_LOCAL_LIGHTS=1') }
        'pbr_forward_plus_transformed.frag' { $shaderSource = 'pbr.frag'; $defines = @('BISTRO_LOCAL_LIGHTS=2', 'BISTRO_TRANSFORMED_DIRECTIONS=1') }
        'gbuffer_transformed.frag' { $shaderSource = 'gbuffer.frag'; $defines = @('BISTRO_TRANSFORMED_DIRECTIONS=1') }
        'shadow_transformed.frag' { $shaderSource = 'shadow.frag'; $defines = @('BISTRO_TRANSFORMED_DIRECTIONS=1') }
    }
    $hasInline = $name -cin @('pbr_lightmap.vert','gbuffer_lightmap.vert','shadow.vert', 'pbr_instancing.vert', 'pick.vert') -or
        ($stage -ceq 'vert' -and $name.Contains('_transformed'))
    @{
        Name = $name
        Source = $shaderSource + '.hlsl'
        Defines = $defines
        CheckClip = $name -cin @('pbr.vert', 'gbuffer.vert')
        HasInline = $hasInline
    }
}
$variants += foreach ($variant in $variants) {
    if ($variant.Source -in @('pbr.frag.hlsl', 'gbuffer.frag.hlsl')) {
        $shadow = $variant.Clone()
        $shadow.Name = $variant.Name.Replace('.frag', '_indirect_shadow.frag')
        $shadow.Defines = @($variant.Defines) + 'INDIRECT_SHADOW=1'
        $shadow
        $half = $shadow.Clone()
        $half.Name = "half_" + $shadow.Name
        $half.Defines = @($shadow.Defines) + 'SHADOW_HALF_RESOLVE=1'
        $half
    }
}
$variants += foreach ($name in @('pbr_indirect_shadow.frag', 'pbr_transformed_indirect_shadow.frag')) {
    $receiver = ($variants | Where-Object Name -eq $name).Clone()
    $receiver.Name = "receiver_" + $name
    $receiver.Defines = @($receiver.Defines) + 'SHADOW_RECEIVER=1'
    $receiver
}
# The runner reports this manifest for both display routes, including standard output.
$coreManifest = Join-Path $PSScriptRoot '../../sashiko-renderer-core/shaders/dxil/manifest.json'
$manifestFields = [ordered]@{
    inline_bytes = 4
    placement_inline_bytes = 68
}
& "$PSScriptRoot/../../../tools/shaders/compile_hlsl.ps1" -ShaderRoot $PSScriptRoot -Variants $variants `
    -SdkRoot $SdkRoot -Verify:$Verify -ManifestFiles @{ renderer_shaders_sha256 = $coreManifest } -ManifestFields $manifestFields
