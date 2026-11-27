struct MaterialData_std140_0
{
    @align(16) base_factor_0 : vec4<f32>,
    @align(16) emissive_normal_scale_0 : vec4<f32>,
    @align(16) metallic_roughness_occlusion_cutoff_0 : vec4<f32>,
    @align(16) flags_0 : vec4<u32>,
};

@binding(1) @group(0) var<uniform> material_0 : MaterialData_std140_0;
@binding(0) @group(1) var base_image_0 : texture_2d<f32>;

@binding(4) @group(1) var material_sampler_0 : sampler;

@binding(2) @group(1) var orm_image_0 : texture_2d<f32>;

@binding(6) @group(1) var orm_sampler_0 : sampler;

@binding(1) @group(1) var normal_image_0 : texture_2d<f32>;

@binding(5) @group(1) var normal_sampler_0 : sampler;

@binding(3) @group(1) var emissive_image_0 : texture_2d<f32>;

@binding(7) @group(1) var emissive_sampler_0 : sampler;

struct _MatrixStorage_float4x4std140_0
{
    @align(16) data_0 : array<vec4<f32>, i32(4)>,
};

struct _Array_std140_matrixx3Cfloatx2C4x2C4x3E4_0
{
    @align(16) data_1 : array<_MatrixStorage_float4x4std140_0, i32(4)>,
};

struct CameraData_std140_0
{
    @align(16) view_projection_0 : _MatrixStorage_float4x4std140_0,
    @align(16) eye_0 : vec4<f32>,
    @align(16) light_direction_0 : vec4<f32>,
    @align(16) light_color_0 : vec4<f32>,
    @align(16) environment_0 : vec4<f32>,
    @align(16) shadow_view_projection_0 : _Array_std140_matrixx3Cfloatx2C4x2C4x3E4_0,
    @align(16) shadow_splits_0 : vec4<f32>,
    @align(16) shadow_texel_world_0 : vec4<f32>,
    @align(16) shadow_depth_scale_0 : vec4<f32>,
    @align(16) camera_forward_blend_0 : vec4<f32>,
    @align(16) shadow_bias_0 : vec4<f32>,
};

@binding(0) @group(0) var<uniform> camera_0 : CameraData_std140_0;
@binding(0) @group(2) var irradiance_image_0 : texture_2d<f32>;

@binding(3) @group(2) var environment_sampler_0 : sampler;

@binding(1) @group(2) var reflection_image_0 : texture_2d<f32>;

@binding(2) @group(2) var brdf_image_0 : texture_2d<f32>;

@binding(4) @group(2) var reflection_probes_0 : texture_2d<f32>;

struct SLANG_ParameterGroup_ReflectionMetadata_std140_0
{
    @align(16) grid_origin_0 : vec3<f32>,
    @align(4) probe_count_0 : u32,
    @align(16) grid_scale_0 : vec3<f32>,
    @align(4) scenario_count_0 : u32,
    @align(16) candidate_masks_0 : array<vec4<u32>, i32(16)>,
    @align(16) probe_records_0 : array<vec4<f32>, i32(96)>,
};

@binding(5) @group(2) var<uniform> ReflectionMetadata_0 : SLANG_ParameterGroup_ReflectionMetadata_std140_0;
@binding(0) @group(3) var shadow_image0_0 : texture_depth_2d;

@binding(1) @group(3) var shadow_image1_0 : texture_2d<f32>;

@binding(4) @group(3) var shadow_sampler_0 : sampler_comparison;

@binding(2) @group(3) var shadow_image2_0 : texture_2d<f32>;

@binding(3) @group(3) var shadow_image3_0 : texture_2d<f32>;

fn isnan_0( x_0 : f32) -> bool
{
    var _S1 : u32 = (bitcast<u32>((x_0)));
    var _S2 : u32 = (_S1 & (u32(8388607)));
    var _S3 : bool;
    if(((((_S1 >> (u32(23)))) & (u32(255)))) == u32(255))
    {
        _S3 = _S2 != u32(0);
    }
    else
    {
        _S3 = false;
    }
    return _S3;
}

fn rsqrt_0( x_1 : f32) -> f32
{
    return 1.0f / sqrt(x_1);
}

fn unit_or_0( value_0 : vec3<f32>,  fallback_0 : vec3<f32>) -> vec3<f32>
{
    var squared_0 : f32 = dot(value_0, value_0);
    var _S4 : vec3<f32>;
    if(squared_0 > 9.99999968265522539e-21f)
    {
        _S4 = value_0 * vec3<f32>(rsqrt_0(squared_0));
    }
    else
    {
        _S4 = fallback_0;
    }
    return _S4;
}

struct Surface_0
{
     base_color_0 : vec3<f32>,
     normal_0 : vec3<f32>,
     geometric_normal_0 : vec3<f32>,
     emissive_0 : vec3<f32>,
     metallic_0 : f32,
     roughness_0 : f32,
     occlusion_0 : f32,
};

struct MaterialSample_0
{
     surface_0 : Surface_0,
     output_alpha_0 : f32,
};

fn sample_material_0( world_normal_0 : vec3<f32>,  world_tangent_0 : vec4<f32>,  texcoord_0 : vec2<f32>,  front_facing_0 : bool) -> MaterialSample_0
{
    var dx_0 : vec2<f32> = dpdx(texcoord_0);
    var dy_0 : vec2<f32> = dpdy(texcoord_0);
    var _S5 : bool;
    if(!front_facing_0)
    {
        _S5 = (material_0.flags_0.y) == u32(0);
    }
    else
    {
        _S5 = false;
    }
    if(_S5)
    {
        discard;
    }
    var base_0 : vec4<f32> = material_0.base_factor_0 * (textureSampleGrad((base_image_0), (material_sampler_0), (texcoord_0), (dx_0), (dy_0)));
    var alpha_0 : f32 = clamp(base_0.w, 0.0f, 1.0f);
    var alpha_mode_0 : u32 = material_0.flags_0.x;
    if(alpha_mode_0 == u32(1))
    {
        _S5 = alpha_0 < (material_0.metallic_roughness_occlusion_cutoff_0.w);
    }
    else
    {
        _S5 = false;
    }
    if(_S5)
    {
        _S5 = true;
    }
    else
    {
        if(alpha_mode_0 == u32(2))
        {
            _S5 = alpha_0 <= 0.0f;
        }
        else
        {
            _S5 = false;
        }
    }
    if(_S5)
    {
        discard;
    }
    var result_0 : MaterialSample_0;
    result_0.surface_0.base_color_0 = base_0.xyz;
    var _S6 : f32;
    if(alpha_mode_0 == u32(2))
    {
        _S6 = alpha_0;
    }
    else
    {
        _S6 = 1.0f;
    }
    result_0.output_alpha_0 = _S6;
    var orm_0 : vec3<f32> = (textureSampleGrad((orm_image_0), (orm_sampler_0), (texcoord_0), (dx_0), (dy_0))).xyz;
    result_0.surface_0.metallic_0 = clamp(material_0.metallic_roughness_occlusion_cutoff_0.x * orm_0.z, 0.0f, 1.0f);
    result_0.surface_0.roughness_0 = clamp(material_0.metallic_roughness_occlusion_cutoff_0.y * orm_0.y, 0.0f, 1.0f);
    result_0.surface_0.occlusion_0 = mix(1.0f, orm_0.x, clamp(material_0.metallic_roughness_occlusion_cutoff_0.z, 0.0f, 1.0f));
    const _S7 : vec3<f32> = vec3<f32>(0.0f, 0.0f, 1.0f);
    var n_0 : vec3<f32> = unit_or_0(world_normal_0, _S7);
    var _S8 : vec3<f32>;
    if(front_facing_0)
    {
        _S8 = n_0;
    }
    else
    {
        _S8 = (vec3<f32>(0) - n_0);
    }
    result_0.surface_0.geometric_normal_0 = _S8;
    var _S9 : vec3<f32> = world_tangent_0.xyz;
    var _S10 : vec3<f32> = _S9 - n_0 * vec3<f32>(dot(n_0, _S9));
    if((abs(n_0.z)) < 0.89999997615814209f)
    {
        _S8 = unit_or_0(cross(_S7, n_0), vec3<f32>(1.0f, 0.0f, 0.0f));
    }
    else
    {
        _S8 = unit_or_0(cross(vec3<f32>(0.0f, 1.0f, 0.0f), n_0), vec3<f32>(1.0f, 0.0f, 0.0f));
    }
    var t_0 : vec3<f32> = unit_or_0(_S10, _S8);
    var _S11 : vec3<f32> = cross(n_0, t_0);
    if((world_tangent_0.w) < 0.0f)
    {
        _S6 = -1.0f;
    }
    else
    {
        _S6 = 1.0f;
    }
    var b_0 : vec3<f32> = _S11 * vec3<f32>(_S6);
    var mapped_0 : vec3<f32> = (textureSampleGrad((normal_image_0), (normal_sampler_0), (texcoord_0), (dx_0), (dy_0))).xyz * vec3<f32>(2.0f) - vec3<f32>(1.0f);
    if((material_0.flags_0.z) != u32(0))
    {
        if((material_0.flags_0.z) == u32(2))
        {
            mapped_0[i32(1)] = - mapped_0.y;
        }
        mapped_0[i32(2)] = sqrt(saturate(1.0f - dot(mapped_0.xy, mapped_0.xy)));
    }
    var _S12 : vec2<f32> = mapped_0.xy * vec2<f32>(material_0.emissive_normal_scale_0.w);
    mapped_0.x = _S12.x;
    mapped_0.y = _S12.y;
    var n_1 : vec3<f32> = unit_or_0(t_0 * vec3<f32>(mapped_0.x) + b_0 * vec3<f32>(mapped_0.y) + n_0 * vec3<f32>(mapped_0.z), n_0);
    if(front_facing_0)
    {
        _S8 = n_1;
    }
    else
    {
        _S8 = (vec3<f32>(0) - n_1);
    }
    result_0.surface_0.normal_0 = _S8;
    result_0.surface_0.emissive_0 = material_0.emissive_normal_scale_0.xyz * (textureSampleGrad((emissive_image_0), (emissive_sampler_0), (texcoord_0), (dx_0), (dy_0))).xyz;
    return result_0;
}

fn baked_debug_0() -> u32
{
    return u32(clamp(floor((camera_0.shadow_bias_0.w - 1.0f) * 0.5f), 0.0f, 3.0f));
}

fn probe_load_0( index_0 : u32,  dims_0 : vec2<u32>) -> vec4<f32>
{
    var _S13 : u32 = dims_0.x;
    var _S14 : u32 = min(index_0, _S13 * dims_0.y - u32(1));
    var _S15 : u32 = _S14 % _S13;
    var _S16 : i32 = i32(_S15);
    var _S17 : u32 = _S14 / _S13;
    var _S18 : vec3<i32> = vec3<i32>(_S16, i32(_S17), i32(0));
    return (textureLoad((irradiance_image_0), ((_S18)).xy, ((_S18)).z));
}

fn probe_index_0( value_1 : f32,  maximum_0 : u32) -> u32
{
    var _S19 : f32;
    if(value_1 >= 0.0f)
    {
        _S19 = min(value_1, f32(maximum_0));
    }
    else
    {
        _S19 = 0.0f;
    }
    return u32(_S19);
}

fn baked_blend_0() -> f32
{
    return saturate(camera_0.shadow_bias_0.w - 1.0f - 2.0f * f32(baked_debug_0()));
}

fn surface_texel_0( index_1 : u32,  dims_1 : vec2<u32>,  keys_0 : u32,  phase_0 : f32,  start_0 : u32) -> vec3<f32>
{
    var at_0 : u32 = start_0 + index_1 * keys_0 + u32(phase_0);
    var _S20 : vec4<f32> = probe_load_0(at_0, dims_1);
    var value_2 : vec3<f32> = _S20.xyz;
    var _S21 : f32 = fract(phase_0);
    var value_3 : vec3<f32>;
    if(_S21 > 0.0f)
    {
        var _S22 : vec4<f32> = probe_load_0(at_0 + u32(1), dims_1);
        value_3 = mix(value_2, _S22.xyz, vec3<f32>(_S21));
    }
    else
    {
        value_3 = value_2;
    }
    return value_3;
}

fn apply_lightmap_0( s_0 : ptr<function, Surface_0>,  slot_0 : u32,  front_0 : bool,  uv_0 : vec2<f32>)
{
    var _S23 : u32 = baked_debug_0();
    if(_S23 > u32(0))
    {
        (*s_0).occlusion_0 = 1.0f;
        (*s_0).emissive_0 = vec3<f32>(0.0f);
    }
    var _S24 : bool;
    if(slot_0 == u32(0))
    {
        _S24 = true;
    }
    else
    {
        _S24 = (camera_0.shadow_bias_0.w) == 0.0f;
    }
    if(_S24)
    {
        return;
    }
    var dims_2 : vec2<u32>;
    var _S25 : u32 = dims_2[i32(0)];
    var _S26 : u32 = dims_2[i32(1)];
    {var dim = textureDimensions((irradiance_image_0));((_S25)) = dim.x;((_S26)) = dim.y;};
    dims_2[i32(0)] = _S25;
    dims_2[i32(1)] = _S26;
    var last_0 : u32 = dims_2.x * dims_2.y - u32(1);
    var header_0 : vec4<f32> = probe_load_0(u32(0), dims_2);
    if(slot_0 > (probe_index_0(header_0.x, last_0 / u32(2))))
    {
        return;
    }
    var keys_1 : u32 = clamp(probe_index_0(header_0.y, u32(16)), u32(1), u32(16));
    var start_1 : u32 = probe_index_0(header_0.z, last_0);
    var _S27 : u32 = u32(2) * slot_0;
    var rect_0 : vec4<f32> = probe_load_0(_S27 - u32(1), dims_2);
    var blend_0 : f32;
    if(front_0)
    {
        blend_0 = rect_0.x;
    }
    else
    {
        blend_0 = rect_0.z;
    }
    var origin_0 : u32 = probe_index_0(blend_0, last_0);
    if(front_0)
    {
        blend_0 = rect_0.y;
    }
    else
    {
        blend_0 = rect_0.w;
    }
    var pitch_0 : u32 = probe_index_0(blend_0, u32(2048));
    var _S28 : vec4<f32> = probe_load_0(_S27, dims_2);
    var coord_0 : vec2<f32> = saturate(uv_0) * clamp(_S28.xy, vec2<f32>(1.0f), vec2<f32>(2048.0f)) - vec2<f32>(0.5f);
    var _S29 : vec2<i32> = vec2<i32>(floor(coord_0));
    var i_0 : u32 = u32(i32(origin_0) + _S29.x + _S29.y * i32(pitch_0));
    if((camera_0.shadow_bias_0.w) < 0.0f)
    {
        blend_0 = saturate(-1.0f - camera_0.shadow_bias_0.w);
    }
    else
    {
        blend_0 = baked_blend_0();
    }
    var phase_1 : f32 = blend_0 * f32(keys_1 - u32(1));
    var _S30 : vec3<f32> = surface_texel_0(i_0, dims_2, keys_1, phase_1, start_1);
    var _S31 : vec3<f32> = surface_texel_0(i_0 + u32(1), dims_2, keys_1, phase_1, start_1);
    var _S32 : vec3<f32> = vec3<f32>(fract(coord_0.x));
    var a_0 : vec3<f32> = mix(_S30, _S31, _S32);
    var _S33 : u32 = i_0 + pitch_0;
    var _S34 : vec3<f32> = surface_texel_0(_S33, dims_2, keys_1, phase_1, start_1);
    var _S35 : vec3<f32> = surface_texel_0(_S33 + u32(1), dims_2, keys_1, phase_1, start_1);
    var irradiance_0 : vec3<f32> = mix(a_0, mix(_S34, _S35, _S32), vec3<f32>(fract(coord_0.y)));
    if(_S23 == u32(0))
    {
        (*s_0).emissive_0 = (*s_0).emissive_0 + irradiance_0 * (*s_0).base_color_0 * vec3<f32>((1.0f - (*s_0).metallic_0)) * vec3<f32>((*s_0).occlusion_0) / vec3<f32>(3.14159274101257324f);
    }
    else
    {
        var _S36 : vec3<f32>;
        if(_S23 == u32(1))
        {
            _S36 = irradiance_0 / vec3<f32>(3.14159274101257324f);
        }
        else
        {
            _S36 = vec3<f32>(0.89999997615814209f, 0.30000001192092896f, 0.03999999910593033f);
        }
        (*s_0).emissive_0 = _S36;
    }
    (*s_0).occlusion_0 = 0.0f;
    return;
}

struct BrdfInputs_0
{
     view_0 : vec3<f32>,
     f0_0 : vec3<f32>,
     diffuse_color_0 : vec3<f32>,
};

fn make_brdf_inputs_0( position_0 : vec3<f32>,  surface_1 : Surface_0) -> BrdfInputs_0
{
    var result_1 : BrdfInputs_0;
    var to_eye_0 : vec3<f32>;
    if((camera_0.light_color_0.w) > 0.0f)
    {
        to_eye_0 = (vec3<f32>(0) - camera_0.camera_forward_blend_0.xyz);
    }
    else
    {
        to_eye_0 = camera_0.eye_0.xyz - position_0;
    }
    result_1.view_0 = unit_or_0(to_eye_0, surface_1.normal_0);
    result_1.f0_0 = mix(vec3<f32>(0.03999999910593033f), surface_1.base_color_0, vec3<f32>(surface_1.metallic_0));
    result_1.diffuse_color_0 = surface_1.base_color_0 * vec3<f32>((1.0f - surface_1.metallic_0));
    return result_1;
}

fn lighting_keys_0( tag_0 : f32) -> u32
{
    var _S37 : u32;
    if(tag_0 == 0.0f)
    {
        _S37 = u32(2);
    }
    else
    {
        _S37 = clamp(probe_index_0(abs(tag_0), u32(16)), u32(1), u32(16));
    }
    return _S37;
}

fn probe_coefficient_0( at_key_0 : u32,  coefficients_0 : u32,  mix_0 : f32,  dims_3 : vec2<u32>) -> vec3<f32>
{
    var _S38 : vec4<f32> = probe_load_0(at_key_0, dims_3);
    var value_4 : vec3<f32> = _S38.xyz;
    var value_5 : vec3<f32>;
    if(mix_0 > 0.0f)
    {
        var _S39 : vec4<f32> = probe_load_0(at_key_0 + coefficients_0, dims_3);
        value_5 = mix(value_4, _S39.xyz, vec3<f32>(mix_0));
    }
    else
    {
        value_5 = value_4;
    }
    return value_5;
}

fn sample_probe_volume_0( dims_4 : vec2<u32>,  bounds_0 : vec4<f32>,  f_0 : vec3<f32>,  n_2 : vec3<f32>,  layout_0 : vec4<f32>,  header_1 : u32,  stride_0 : u32,  coefficients_1 : u32,  key_0 : u32,  mix_1 : f32) -> vec3<f32>
{
    var maximum_1 : u32 = dims_4.x * dims_4.y - u32(1);
    var _S40 : bool;
    if((any((f_0 < vec3<f32>(0.0f)))))
    {
        _S40 = true;
    }
    else
    {
        _S40 = (any((f_0 > vec3<f32>(1.0f))));
    }
    var sky_0 : vec3<f32>;
    if(_S40)
    {
        var _S41 : vec4<f32> = probe_load_0(u32(2) + key_0, dims_4);
        var sky_1 : vec3<f32> = _S41.xyz;
        if(mix_1 > 0.0f)
        {
            var _S42 : vec4<f32> = probe_load_0(u32(3) + key_0, dims_4);
            sky_0 = mix(sky_1, _S42.xyz, vec3<f32>(mix_1));
        }
        else
        {
            sky_0 = sky_1;
        }
        return sky_0;
    }
    var node_count_0 : u32 = probe_index_0(layout_0.x, maximum_1 / u32(3));
    var _S43 : u32 = header_1 + node_count_0 * u32(3);
    var _S44 : f32 = bounds_0.w;
    var node_0 : u32 = u32(0);
    sky_0 = f_0;
    var depth_0 : u32 = u32(0);
    var size_0 : f32 = _S44;
    for(;;)
    {
        if(depth_0 <= u32(10))
        {
        }
        else
        {
            break;
        }
        var at_1 : u32 = header_1 + node_0 * u32(3);
        var _S45 : vec4<f32> = probe_load_0(at_1, dims_4);
        var first_0 : u32 = probe_index_0(_S45.x, node_count_0);
        if(first_0 == u32(0))
        {
            var _S46 : vec4<f32> = probe_load_0(at_1 + u32(1), dims_4);
            var _S47 : vec4<f32> = probe_load_0(at_1 + u32(2), dims_4);
            var _S48 : vec3<f32> = vec3<f32>(0.0f);
            var i_1 : u32 = u32(0);
            var sum_0 : vec3<f32> = _S48;
            var weights_0 : f32 = 0.0f;
            for(;;)
            {
                if(i_1 < u32(8))
                {
                }
                else
                {
                    break;
                }
                var corner_0 : vec3<f32> = vec3<f32>(f32((i_1 & (u32(1)))), f32((((i_1 >> (u32(1)))) & (u32(1)))), f32((i_1 >> (u32(2)))));
                var _S49 : vec3<f32> = vec3<f32>(1.0f);
                var trilinear_0 : vec3<f32> = mix(_S49 - sky_0, sky_0, corner_0);
                var weight_0 : f32 = trilinear_0.x * trilinear_0.y * trilinear_0.z;
                var _S50 : f32;
                if(i_1 < u32(4))
                {
                    _S50 = _S46[(i_1 & (u32(3)))];
                }
                else
                {
                    _S50 = _S47[(i_1 & (u32(3)))];
                }
                var _S51 : u32 = maximum_1 / stride_0;
                var p_0 : u32 = _S43 + stride_0 * probe_index_0(_S50, _S51);
                var delta_0 : vec3<f32> = (sky_0 - corner_0) * vec3<f32>(size_0);
                var _S52 : u32 = p_0 + u32(2);
                var _S53 : vec4<f32> = probe_load_0(_S52, dims_4);
                var _S54 : f32 = _S53.w;
                var _S55 : vec4<f32> = probe_load_0(p_0 + u32(3), dims_4);
                var _S56 : f32 = _S55.w;
                var _S57 : vec4<f32> = probe_load_0(p_0 + u32(4), dims_4);
                var delta_1 : vec3<f32> = delta_0 - vec3<f32>(_S54, _S56, _S57.w);
                var minus_0 : vec4<f32> = probe_load_0(p_0, dims_4);
                var weight_1 : f32 = weight_0 * saturate(minus_0.w);
                var neg_0 : vec3<f32> = minus_0.xyz;
                var _S58 : vec4<f32> = probe_load_0(p_0 + u32(1), dims_4);
                var pos_0 : vec3<f32> = _S58.xyz;
                var _S59 : f32;
                if((delta_1.x) >= 0.0f)
                {
                    _S59 = pos_0.x;
                }
                else
                {
                    _S59 = neg_0.x;
                }
                var _S60 : f32;
                if((delta_1.y) >= 0.0f)
                {
                    _S60 = pos_0.y;
                }
                else
                {
                    _S60 = neg_0.y;
                }
                var _S61 : f32;
                if((delta_1.z) >= 0.0f)
                {
                    _S61 = pos_0.z;
                }
                else
                {
                    _S61 = neg_0.z;
                }
                var visibility_0 : vec3<f32> = saturate(_S49 + (vec3<f32>(_S59, _S60, _S61) - abs(delta_1)) / vec3<f32>(max(size_0 * 0.05000000074505806f, 9.99999997475242708e-07f)));
                var weight_2 : f32 = weight_1 * min(visibility_0.x, min(visibility_0.y, visibility_0.z));
                var _S62 : u32 = _S52 + coefficients_1 * key_0;
                var k_0 : u32 = u32(0);
                var sh_0 : vec3<f32> = _S48;
                for(;;)
                {
                    if(k_0 < u32(4))
                    {
                    }
                    else
                    {
                        break;
                    }
                    var _S63 : vec3<f32> = probe_coefficient_0(_S62 + k_0, coefficients_1, mix_1, dims_4);
                    var sh_1 : vec3<f32> = sh_0 + _S63 * vec3<f32>(vec4<f32>(1.0f, n_2)[k_0]);
                    k_0 = k_0 + u32(1);
                    sh_0 = sh_1;
                }
                var sh_2 : vec3<f32>;
                if(coefficients_1 == u32(9))
                {
                    var _S64 : f32 = n_2.x;
                    var _S65 : f32 = n_2.y;
                    var _S66 : f32 = n_2.z;
                    var _S67 : array<f32, i32(5)> = array<f32, i32(5)>( _S64 * _S65, _S65 * _S66, _S64 * _S66, _S64 * _S64 - _S65 * _S65, 3.0f * _S66 * _S66 - 1.0f );
                    var k_1 : u32 = u32(0);
                    sh_2 = sh_0;
                    for(;;)
                    {
                        if(k_1 < u32(5))
                        {
                        }
                        else
                        {
                            break;
                        }
                        var _S68 : vec3<f32> = probe_coefficient_0(_S62 + u32(4) + k_1, coefficients_1, mix_1, dims_4);
                        var sh_3 : vec3<f32> = sh_2 + _S68 * vec3<f32>(_S67[k_1]);
                        k_1 = k_1 + u32(1);
                        sh_2 = sh_3;
                    }
                }
                else
                {
                    sh_2 = sh_0;
                }
                var sum_1 : vec3<f32> = sum_0 + sh_2 * vec3<f32>(weight_2);
                var weights_1 : f32 = weights_0 + weight_2;
                i_1 = i_1 + u32(1);
                sum_0 = sum_1;
                weights_0 = weights_1;
            }
            var _S69 : u32 = baked_debug_0();
            if(_S69 == u32(2))
            {
                return vec3<f32>(0.07999999821186066f, 0.25f + 0.07000000029802322f * f32(depth_0), 0.80000001192092896f);
            }
            if(_S69 == u32(3))
            {
                return vec3<f32>(saturate(weights_0));
            }
            return max(sum_0 / vec3<f32>(max(weights_0, 9.99999997475242708e-07f)), _S48);
        }
        var _S70 : vec3<u32> = vec3<u32>(step(vec3<f32>(0.5f), sky_0));
        var _S71 : u32 = min(first_0 + _S70.x + u32(2) * _S70.y + u32(4) * _S70.z, node_count_0);
        var _S72 : vec3<f32> = saturate(vec3<f32>(2.0f) * sky_0 - vec3<f32>(_S70));
        var size_1 : f32 = size_0 * 0.5f;
        var depth_1 : u32 = depth_0 + u32(1);
        node_0 = _S71;
        sky_0 = _S72;
        depth_0 = depth_1;
        size_0 = size_1;
    }
    return vec3<f32>(0.0f);
}

fn probe_irradiance_0( position_1 : vec3<f32>,  n_3 : vec3<f32>,  blend_1 : f32) -> vec3<f32>
{
    var dims_5 : vec2<u32>;
    var _S73 : u32 = dims_5[i32(0)];
    var _S74 : u32 = dims_5[i32(1)];
    {var dim = textureDimensions((irradiance_image_0));((_S73)) = dim.x;((_S74)) = dim.y;};
    dims_5[i32(0)] = _S73;
    dims_5[i32(1)] = _S74;
    var bounds_1 : vec4<f32> = probe_load_0(u32(0), dims_5);
    var f_1 : vec3<f32> = (position_1 - bounds_1.xyz) / vec3<f32>(max(bounds_1.w, 9.99999968265522539e-21f));
    var layout_1 : vec4<f32> = probe_load_0(u32(1), dims_5);
    var _S75 : f32 = layout_1.w;
    var count_0 : u32 = lighting_keys_0(_S75);
    var phase_2 : f32 = saturate(blend_1) * f32(count_0 - u32(1));
    var key_1 : u32 = u32(phase_2);
    var mix_2 : f32 = fract(phase_2);
    var header_2 : u32 = u32(2) + max(count_0, u32(2));
    var coefficients_2 : u32;
    if(_S75 < 0.0f)
    {
        coefficients_2 = u32(9);
    }
    else
    {
        coefficients_2 = u32(4);
    }
    var stride_1 : u32 = u32(2) + coefficients_2 * count_0;
    if(phase_2 == 0.0f)
    {
        var _S76 : vec3<f32> = sample_probe_volume_0(dims_5, bounds_1, f_1, n_3, layout_1, header_2, stride_1, coefficients_2, u32(0), 0.0f);
        return _S76;
    }
    var _S77 : vec3<f32> = sample_probe_volume_0(dims_5, bounds_1, f_1, n_3, layout_1, header_2, stride_1, coefficients_2, key_1, mix_2);
    return _S77;
}

fn evaluate_brdf_0( nl_0 : f32,  nv_0 : f32,  nh_0 : f32,  vh_0 : f32,  f0_1 : vec3<f32>,  diffuse_color_1 : vec3<f32>,  roughness_1 : f32) -> vec3<f32>
{
    var _S78 : f32 = max(roughness_1 * roughness_1, 0.00100000004749745f);
    var a2_0 : f32 = _S78 * _S78;
    var denominator_0 : f32 = nh_0 * nh_0 * (a2_0 - 1.0f) + 1.0f;
    var _S79 : f32 = 1.0f - a2_0;
    var _S80 : vec3<f32> = vec3<f32>(1.0f);
    var fresnel_0 : vec3<f32> = f0_1 + (_S80 - f0_1) * vec3<f32>(pow(clamp(1.0f - vh_0, 0.0f, 1.0f), 5.0f));
    return (_S80 - fresnel_0) * diffuse_color_1 / vec3<f32>(3.14159274101257324f) + fresnel_0 * vec3<f32>((a2_0 / max(3.14159274101257324f * denominator_0 * denominator_0, 9.999999960041972e-13f))) * vec3<f32>((0.5f / max(nv_0 * sqrt(a2_0 + _S79 * nl_0 * nl_0) + nl_0 * sqrt(a2_0 + _S79 * nv_0 * nv_0), 9.99999997475242708e-07f)));
}

fn environment_uv_0( direction_0 : vec3<f32>) -> vec2<f32>
{
    var c_0 : f32 = cos(camera_0.environment_0.y);
    var s_1 : f32 = sin(camera_0.environment_0.y);
    var _S81 : f32 = direction_0.x;
    var _S82 : f32 = direction_0.z;
    var _S83 : f32 = c_0 * _S81 - s_1 * _S82;
    var _S84 : f32 = direction_0.y;
    var _S85 : f32 = s_1 * _S81 + c_0 * _S82;
    var _S86 : vec2<f32> = vec2<f32>(_S83, _S85);
    var u_0 : f32;
    if((dot(_S86, _S86)) > 9.99999968265522539e-21f)
    {
        u_0 = fract(0.5f + atan2(_S85, _S83) / 6.28318548202514648f);
    }
    else
    {
        u_0 = 0.5f;
    }
    return vec2<f32>(u_0, acos(clamp(_S84, -1.0f, 1.0f)) / 3.14159274101257324f);
}

fn texture_size_0( image_0 : texture_2d<f32>) -> vec2<f32>
{
    var width_0 : u32;
    var height_0 : u32;
    var levels_0 : u32;
    {var dim = textureDimensions((image_0), (u32(0)));((width_0)) = dim.x;((height_0)) = dim.y;((levels_0)) = textureNumLevels((image_0));};
    return vec2<f32>(f32(width_0), f32(height_0));
}

fn isinf_0( x_2 : f32) -> bool
{
    var _S87 : u32 = (bitcast<u32>((x_2)));
    var _S88 : u32 = (_S87 & (u32(8388607)));
    var _S89 : bool;
    if(((((_S87 >> (u32(23)))) & (u32(255)))) == u32(255))
    {
        _S89 = _S88 == u32(0);
    }
    else
    {
        _S89 = false;
    }
    return _S89;
}

fn isfinite_0( x_3 : f32) -> bool
{
    var _S90 : bool;
    if(isinf_0(x_3))
    {
        _S90 = true;
    }
    else
    {
        _S90 = isnan_0(x_3);
    }
    return !_S90;
}

fn isfinite_1( x_4 : vec3<f32>) -> vec3<bool>
{
    var result_2 : vec3<bool>;
    var i_2 : i32 = i32(0);
    for(;;)
    {
        if(i_2 < i32(3))
        {
        }
        else
        {
            break;
        }
        result_2[i_2] = isfinite_0(x_4[i_2]);
        i_2 = i_2 + i32(1);
    }
    return result_2;
}

fn probe_level_0( uv_1 : vec2<f32>,  level_0 : u32,  first_row_0 : u32,  atlas_height_0 : f32) -> vec3<f32>
{
    var rows_0 : u32 = (u32(64) >> (level_0));
    var _S91 : u32;
    if(level_0 == u32(6))
    {
        _S91 = u32(128);
    }
    else
    {
        _S91 = u32(128) - u32(2) * rows_0;
    }
    var _S92 : f32 = f32(rows_0);
    return (textureSampleLevel((reflection_probes_0), (environment_sampler_0), (vec2<f32>(uv_1.x, (f32(first_row_0 + _S91) + clamp(uv_1.y * _S92, 0.5f, _S92 - 0.5f)) / atlas_height_0)), (0.0f))).xyz;
}

fn probe_specular_0( row_0 : u32,  ray_0 : vec3<f32>,  roughness_2 : f32,  blend_2 : f32,  height_1 : f32,  single_0 : bool) -> vec3<f32>
{
    var _S93 : vec2<f32> = ray_0.xz;
    var _S94 : f32;
    if((dot(_S93, _S93)) > 9.99999968265522539e-21f)
    {
        _S94 = 0.5f + atan2(ray_0.z, ray_0.x) / 6.28318548202514648f;
    }
    else
    {
        _S94 = 0.5f;
    }
    var uv_2 : vec2<f32> = vec2<f32>(_S94, acos(clamp(ray_0.y, -1.0f, 1.0f)) / 3.14159274101257324f);
    var lod_0 : f32 = saturate(roughness_2) * 6.0f;
    var a_1 : u32 = u32(lod_0);
    var _S95 : u32 = min(a_1 + u32(1), u32(6));
    var _S96 : bool;
    if(!single_0)
    {
        _S96 = blend_2 == 1.0f;
    }
    else
    {
        _S96 = false;
    }
    var _S97 : u32;
    if(_S96)
    {
        _S97 = row_0 + u32(132);
    }
    else
    {
        _S97 = row_0;
    }
    var _S98 : vec3<f32> = vec3<f32>(fract(lod_0));
    var day_0 : vec3<f32> = mix(probe_level_0(uv_2, a_1, _S97, height_1), probe_level_0(uv_2, _S95, _S97, height_1), _S98);
    if(single_0)
    {
        _S96 = true;
    }
    else
    {
        _S96 = blend_2 == 0.0f;
    }
    if(_S96)
    {
        _S96 = true;
    }
    else
    {
        _S96 = blend_2 == 1.0f;
    }
    if(_S96)
    {
        return day_0;
    }
    var _S99 : u32 = _S97 + u32(132);
    return mix(day_0, mix(probe_level_0(uv_2, a_1, _S99, height_1), probe_level_0(uv_2, _S95, _S99, height_1), _S98), vec3<f32>(blend_2));
}

fn local_specular_0( world_0 : vec3<f32>,  reflected_0 : vec3<f32>,  roughness_3 : f32,  global_0 : vec3<f32>,  parameter_0 : f32,  weights_2 : ptr<function, vec3<f32>>) -> vec3<f32>
{
    var width_1 : u32;
    var height_2 : u32;
    {var dim = textureDimensions((reflection_probes_0));((width_1)) = dim.x;((height_2)) = dim.y;};
    var _S100 : vec3<f32> = vec3<f32>(0.0f);
    (*weights_2) = _S100;
    var _S101 : f32 = saturate(parameter_0 - floor(parameter_0 * 0.5f) * 2.0f);
    var single_1 : bool = (ReflectionMetadata_0.scenario_count_0) == u32(1);
    var rows_1 : u32;
    if(single_1)
    {
        rows_1 = u32(132);
    }
    else
    {
        rows_1 = u32(264);
    }
    var count_1 : u32;
    if(width_1 == u32(128))
    {
        var _S102 : u32 = height_2 / rows_1;
        count_1 = min(min(_S102, ReflectionMetadata_0.probe_count_0), u32(32));
    }
    else
    {
        count_1 = u32(0);
    }
    var candidates_0 : u32;
    if(count_1 == u32(32))
    {
        candidates_0 = u32(4294967295);
    }
    else
    {
        candidates_0 = ((u32(1) << (count_1))) - u32(1);
    }
    var _S103 : bool;
    if(count_1 > u32(4))
    {
        _S103 = (ReflectionMetadata_0.grid_scale_0.x) > 0.0f;
    }
    else
    {
        _S103 = false;
    }
    if(_S103)
    {
        var coord_1 : vec3<f32> = (world_0 - ReflectionMetadata_0.grid_origin_0) * ReflectionMetadata_0.grid_scale_0;
        if((all((isfinite_1(coord_1)))))
        {
            var _S104 : vec3<u32> = vec3<u32>(clamp(coord_1, _S100, vec3<f32>(3.0f)));
            var index_2 : u32 = _S104.x + u32(4) * _S104.y + u32(16) * _S104.z;
            candidates_0 = (candidates_0 & ((ReflectionMetadata_0.candidate_masks_0[(index_2 >> (u32(2)))][(index_2 & (u32(3)))])));
        }
    }
    var radiance_0 : vec3<f32> = _S100;
    var total_0 : f32 = 0.0f;
    for(;;)
    {
        if(candidates_0 != u32(0))
        {
        }
        else
        {
            break;
        }
        var i_3 : u32 = firstTrailingBit(candidates_0);
        var candidates_1 : u32 = (candidates_0 & ((candidates_0 - u32(1))));
        var row_1 : u32 = i_3 * rows_1;
        var _S105 : u32 = u32(3) * i_3;
        var capture_0 : vec4<f32> = ReflectionMetadata_0.probe_records_0[_S105];
        var _S106 : vec3<f32> = world_0 - ReflectionMetadata_0.probe_records_0[_S105 + u32(1)].xyz;
        var _S107 : vec3<f32> = ReflectionMetadata_0.probe_records_0[_S105 + u32(2)].xyz - world_0;
        var inside_0 : vec3<f32> = min(_S106, _S107);
        var _S108 : f32 = min(inside_0.x, min(inside_0.y, inside_0.z));
        if(_S108 > 0.0f)
        {
            _S103 = (capture_0.w) > 0.0f;
        }
        else
        {
            _S103 = false;
        }
        if(!_S103)
        {
            candidates_0 = candidates_1;
            continue;
        }
        var w_0 : f32 = saturate(_S108 / capture_0.w);
        var t_1 : vec3<f32> = mix(_S106, _S107, step(_S100, reflected_0)) / max(abs(reflected_0), vec3<f32>(9.99999968265522539e-21f));
        var radiance_1 : vec3<f32> = radiance_0 + vec3<f32>(w_0) * probe_specular_0(row_1, unit_or_0(world_0 - capture_0.xyz + vec3<f32>(min(t_1.x, min(t_1.y, t_1.z))) * reflected_0, reflected_0), roughness_3, _S101, f32(height_2), single_1);
        var total_1 : f32 = total_0 + w_0;
        (*weights_2)[i_3 % u32(2)] = (*weights_2)[i_3 % u32(2)] + w_0;
        candidates_0 = candidates_1;
        radiance_0 = radiance_1;
        total_0 = total_1;
    }
    var _S109 : f32 = max(1.0f, total_0);
    var _S110 : vec2<f32> = (*weights_2).xy / vec2<f32>(_S109);
    (*weights_2).x = _S110.x;
    (*weights_2).y = _S110.y;
    (*weights_2)[i32(2)] = saturate(1.0f - total_0);
    return radiance_0 / vec3<f32>(_S109) + vec3<f32>((*weights_2).z) * global_0;
}

fn virtual_receiver_level_0( distance_0 : f32) -> u32
{
    var level_1 : u32 = u32(0);
    for(;;)
    {
        var _S111 : bool;
        if(level_1 < u32(7))
        {
            _S111 = distance_0 >= (1.27999997138977051f * f32((u32(1) << (level_1))));
        }
        else
        {
            _S111 = false;
        }
        if(_S111)
        {
        }
        else
        {
            break;
        }
        level_1 = level_1 + u32(1);
    }
    return level_1;
}

fn virtual_origin_0( level_2 : u32) -> vec2<i32>
{
    return (bitcast<vec2<i32>>((camera_0.shadow_view_projection_0.data_1[u32(1) + level_2 / u32(4)].data_0[level_2 % u32(4)].xy)));
}

fn virtual_page_texel_0( light_uv_0 : vec2<f32>,  level_3 : u32,  origin_1 : vec2<i32>) -> vec2<f32>
{
    return light_uv_0 * vec2<f32>((3200.0f / f32((u32(1) << (level_3))))) - vec2<f32>(origin_1) * vec2<f32>(128.0f);
}

fn virtual_page_entry_0( level_4 : u32,  page_0 : vec2<u32>) -> u32
{
    var _S112 : u32 = page_0.x;
    var _S113 : vec3<i32> = vec3<i32>(i32(_S112 / u32(4)), i32(level_4 * u32(128) + page_0.y), i32(0));
    return u32((textureLoad((shadow_image1_0), ((_S113)).xy, ((_S113)).z))[_S112 % u32(4)]);
}

fn virtual_tile_origin_0( slot_1 : u32) -> vec2<u32>
{
    return vec2<u32>(slot_1 % u32(31), slot_1 / u32(31)) * vec2<u32>(u32(132));
}

fn virtual_address_0( light_uv_1 : vec2<f32>,  level_5 : u32,  atlas_texel_0 : ptr<function, vec2<f32>>) -> bool
{
    (*atlas_texel_0) = vec2<f32>(vec2<i32>(i32(0)));
    var texel_0 : vec2<f32> = virtual_page_texel_0(light_uv_1, level_5, virtual_origin_0(level_5));
    var _S114 : bool;
    if(!(all((texel_0 >= vec2<f32>(0.0f)))))
    {
        _S114 = true;
    }
    else
    {
        _S114 = !(all((texel_0 < vec2<f32>(16384.0f))));
    }
    if(_S114)
    {
        return false;
    }
    var page_1 : vec2<u32> = (vec2<u32>(texel_0) >> (vec2<u32>(u32(7))));
    var entry_0 : u32 = virtual_page_entry_0(level_5, page_1);
    if(entry_0 == u32(0))
    {
        _S114 = true;
    }
    else
    {
        _S114 = entry_0 > u32(961);
    }
    if(_S114)
    {
        return false;
    }
    (*atlas_texel_0) = vec2<f32>(virtual_tile_origin_0(entry_0 - u32(1)) + vec2<u32>(u32(2))) + (texel_0 - vec2<f32>(page_1 * vec2<u32>(u32(128))));
    return true;
}

fn virtual_compare_0( uv_3 : vec2<f32>,  reference_0 : f32) -> f32
{
    return (textureSampleCompareLevel((shadow_image0_0), (shadow_sampler_0), (uv_3), (reference_0)));
}

fn virtual_visibility_0( world_1 : vec3<f32>,  normal_1 : vec3<f32>,  eye_distance_0 : vec4<f32>,  axis_x_0 : vec3<f32>,  axis_y_0 : vec3<f32>,  depth_plane_0 : vec4<f32>,  shadow_bias_1 : vec4<f32>) -> f32
{
    var distance_1 : f32 = length(world_1 - eye_distance_0.xyz);
    if(!(distance_1 <= (eye_distance_0.w)))
    {
        return 1.0f;
    }
    var _S115 : vec2<f32> = vec2<f32>(dot(axis_x_0, world_1), dot(axis_y_0, world_1));
    var _S116 : vec2<f32> = vec2<f32>(dot(axis_x_0, normal_1), dot(axis_y_0, normal_1));
    var _S117 : f32 = 1.0f - saturate(dot(normal_1, cross(axis_x_0, axis_y_0)));
    var _S118 : f32 = dot(depth_plane_0.xyz, normal_1);
    var depth_2 : f32 = dot(vec4<f32>(world_1, 1.0f), depth_plane_0);
    var _S119 : bool;
    if(depth_2 >= 0.0f)
    {
        _S119 = depth_2 <= 1.0f;
    }
    else
    {
        _S119 = false;
    }
    if(!_S119)
    {
        return 1.0f;
    }
    var level_6 : u32 = virtual_receiver_level_0(distance_1);
    for(;;)
    {
        if(level_6 < u32(8))
        {
        }
        else
        {
            break;
        }
        var pitch_1 : f32 = f32((u32(1) << (level_6))) / 3200.0f;
        var offset_0 : f32 = pitch_1 * shadow_bias_1.z * _S117;
        var center_address_0 : vec2<f32>;
        var _S120 : bool = virtual_address_0(_S115 + _S116 * vec2<f32>(offset_0), level_6, &(center_address_0));
        if(!_S120)
        {
            level_6 = level_6 + u32(1);
            continue;
        }
        var _S121 : f32 = depth_2 + _S118 * offset_0 - pitch_1 * shadow_bias_1.w * (shadow_bias_1.x + _S117 * shadow_bias_1.y);
        var y_0 : i32 = i32(-1);
        var visible_0 : f32 = 0.0f;
        for(;;)
        {
            if(y_0 <= i32(1))
            {
            }
            else
            {
                break;
            }
            var x_5 : i32 = i32(-1);
            for(;;)
            {
                if(x_5 <= i32(1))
                {
                }
                else
                {
                    break;
                }
                var visible_1 : f32 = visible_0 + virtual_compare_0((center_address_0 + vec2<f32>(f32(x_5), f32(y_0))) / vec2<f32>(4096.0f), _S121);
                x_5 = x_5 + i32(1);
                visible_0 = visible_1;
            }
            y_0 = y_0 + i32(1);
        }
        return visible_0 / 9.0f;
    }
    return 1.0f;
}

fn shadow_depth_0( cascade_0 : u32,  uv_4 : vec2<f32>) -> f32
{
    var size_2 : vec2<u32>;
    var _S122 : bool = cascade_0 == u32(0);
    if(_S122)
    {
        var _S123 : u32 = size_2[i32(0)];
        var _S124 : u32 = size_2[i32(1)];
        {var dim = textureDimensions((shadow_image0_0));((_S123)) = dim.x;((_S124)) = dim.y;};
        size_2[i32(0)] = _S123;
        size_2[i32(1)] = _S124;
    }
    else
    {
        if(cascade_0 == u32(1))
        {
            var _S125 : u32 = size_2[i32(0)];
            var _S126 : u32 = size_2[i32(1)];
            {var dim = textureDimensions((shadow_image1_0));((_S125)) = dim.x;((_S126)) = dim.y;};
            size_2[i32(0)] = _S125;
            size_2[i32(1)] = _S126;
        }
        else
        {
            if(cascade_0 == u32(2))
            {
                var _S127 : u32 = size_2[i32(0)];
                var _S128 : u32 = size_2[i32(1)];
                {var dim = textureDimensions((shadow_image2_0));((_S127)) = dim.x;((_S128)) = dim.y;};
                size_2[i32(0)] = _S127;
                size_2[i32(1)] = _S128;
            }
            else
            {
                var _S129 : u32 = size_2[i32(0)];
                var _S130 : u32 = size_2[i32(1)];
                {var dim = textureDimensions((shadow_image3_0));((_S129)) = dim.x;((_S130)) = dim.y;};
                size_2[i32(0)] = _S129;
                size_2[i32(1)] = _S130;
            }
        }
    }
    var texel_1 : vec3<i32> = vec3<i32>(vec2<i32>(min(vec2<u32>(saturate(uv_4) * vec2<f32>(size_2)), size_2 - vec2<u32>(u32(1)))), i32(0));
    if(_S122)
    {
        return (textureLoad((shadow_image0_0), ((texel_1)).xy, ((texel_1)).z));
    }
    if(cascade_0 == u32(1))
    {
        return (textureLoad((shadow_image1_0), ((texel_1)).xy, ((texel_1)).z)).x;
    }
    if(cascade_0 == u32(2))
    {
        return (textureLoad((shadow_image2_0), ((texel_1)).xy, ((texel_1)).z)).x;
    }
    return (textureLoad((shadow_image3_0), ((texel_1)).xy, ((texel_1)).z)).x;
}

fn cascade_visibility_0( cascade_1 : u32,  position_2 : vec3<f32>,  geometric_normal_1 : vec3<f32>,  light_0 : vec3<f32>) -> f32
{
    var _S131 : u32 = min(cascade_1, u32(3));
    var slope_0 : f32 = 1.0f - max(dot(geometric_normal_1, light_0), 0.0f);
    var texel_world_0 : f32 = camera_0.shadow_texel_world_0[_S131];
    var clip_0 : vec4<f32> = (((mat4x4<f32>(camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(0)][i32(0)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(0)][i32(1)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(0)][i32(2)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(0)][i32(3)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(1)][i32(0)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(1)][i32(1)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(1)][i32(2)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(1)][i32(3)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(2)][i32(0)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(2)][i32(1)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(2)][i32(2)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(2)][i32(3)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(3)][i32(0)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(3)][i32(1)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(3)][i32(2)], camera_0.shadow_view_projection_0.data_1[_S131].data_0[i32(3)][i32(3)])) * (vec4<f32>(position_2 + geometric_normal_1 * vec3<f32>((camera_0.shadow_texel_world_0[_S131] * camera_0.shadow_bias_0.z * slope_0)), 1.0f))));
    var _S132 : f32 = clip_0.w;
    if(!(_S132 > 9.99999968265522539e-21f))
    {
        return 1.0f;
    }
    var ndc_0 : vec3<f32> = clip_0.xyz / vec3<f32>(_S132);
    var _S133 : vec2<f32> = vec2<f32>(0.5f);
    var uv_5 : vec2<f32> = ndc_0.xy * _S133 + _S133;
    var _S134 : vec2<f32> = vec2<f32>(0.0f);
    var _S135 : bool;
    if((any((uv_5 < _S134))))
    {
        _S135 = true;
    }
    else
    {
        _S135 = (any((uv_5 > vec2<f32>(1.0f))));
    }
    if(_S135)
    {
        _S135 = true;
    }
    else
    {
        _S135 = (ndc_0.z) < 0.0f;
    }
    if(_S135)
    {
        _S135 = true;
    }
    else
    {
        _S135 = (ndc_0.z) > 1.0f;
    }
    if(_S135)
    {
        return 1.0f;
    }
    var _S136 : f32 = ndc_0.z - texel_world_0 * camera_0.shadow_depth_scale_0[_S131] * (camera_0.shadow_bias_0.x + camera_0.shadow_bias_0.y * slope_0);
    var extent_0 : vec2<u32>;
    var _S137 : u32 = extent_0[i32(0)];
    var _S138 : u32 = extent_0[i32(1)];
    {var dim = textureDimensions((shadow_image0_0));((_S137)) = dim.x;((_S138)) = dim.y;};
    extent_0[i32(0)] = _S137;
    extent_0[i32(1)] = _S138;
    var _S139 : vec2<f32> = vec2<f32>(1.0f);
    var _S140 : vec2<f32> = _S139 / vec2<f32>(extent_0);
    var y_1 : i32 = i32(-1);
    var visible_2 : f32 = 0.0f;
    for(;;)
    {
        if(y_1 <= i32(1))
        {
        }
        else
        {
            break;
        }
        var x_6 : i32 = i32(-1);
        for(;;)
        {
            if(x_6 <= i32(1))
            {
            }
            else
            {
                break;
            }
            var tap_0 : vec2<f32> = uv_5 + vec2<f32>(f32(x_6), f32(y_1)) * _S140;
            if((any((tap_0 < _S134))))
            {
                _S135 = true;
            }
            else
            {
                _S135 = (any((tap_0 > _S139)));
            }
            var visible_3 : f32;
            if(_S135)
            {
                visible_3 = visible_2 + 1.0f;
            }
            else
            {
                if(_S136 <= (shadow_depth_0(_S131, tap_0)))
                {
                    visible_3 = 1.0f;
                }
                else
                {
                    visible_3 = 0.0f;
                }
                visible_3 = visible_2 + visible_3;
            }
            x_6 = x_6 + i32(1);
            visible_2 = visible_3;
        }
        y_1 = y_1 + i32(1);
    }
    return visible_2 / 9.0f;
}

fn sun_visibility_0( position_3 : vec3<f32>,  geometric_normal_2 : vec3<f32>,  light_1 : vec3<f32>) -> f32
{
    if(!((camera_0.environment_0.w) > 0.0f))
    {
        return 1.0f;
    }
    var _S141 : bool;
    if((camera_0.shadow_splits_0.w) < 0.0f)
    {
        var atlas_size_0 : vec2<u32>;
        var _S142 : u32 = atlas_size_0[i32(0)];
        var _S143 : u32 = atlas_size_0[i32(1)];
        {var dim = textureDimensions((shadow_image0_0));((_S142)) = dim.x;((_S143)) = dim.y;};
        atlas_size_0[i32(0)] = _S142;
        atlas_size_0[i32(1)] = _S143;
        var table_size_0 : vec2<u32>;
        var _S144 : u32 = table_size_0[i32(0)];
        var _S145 : u32 = table_size_0[i32(1)];
        {var dim = textureDimensions((shadow_image1_0));((_S144)) = dim.x;((_S145)) = dim.y;};
        table_size_0[i32(0)] = _S144;
        table_size_0[i32(1)] = _S145;
        if((any((atlas_size_0 != vec2<u32>(u32(4096))))))
        {
            _S141 = true;
        }
        else
        {
            _S141 = (any((table_size_0 != vec2<u32>(u32(32), u32(1024)))));
        }
        if(_S141)
        {
            return 1.0f;
        }
        return virtual_visibility_0(position_3, geometric_normal_2, vec4<f32>(camera_0.eye_0.xyz, - camera_0.shadow_splits_0.w), camera_0.shadow_view_projection_0.data_1[i32(0)].data_0[i32(0)].xyz, camera_0.shadow_view_projection_0.data_1[i32(0)].data_0[i32(1)].xyz, camera_0.shadow_view_projection_0.data_1[i32(0)].data_0[i32(2)], camera_0.shadow_view_projection_0.data_1[i32(0)].data_0[i32(3)]);
    }
    var distance_2 : f32 = dot(position_3 - camera_0.eye_0.xyz, camera_0.camera_forward_blend_0.xyz);
    if(distance_2 < (camera_0.eye_0.w))
    {
        _S141 = true;
    }
    else
    {
        _S141 = distance_2 > (camera_0.shadow_splits_0.w);
    }
    if(_S141)
    {
        return 1.0f;
    }
    var cascade_2 : u32;
    if(distance_2 <= (camera_0.shadow_splits_0.x))
    {
        cascade_2 = u32(0);
    }
    else
    {
        if(distance_2 <= (camera_0.shadow_splits_0.y))
        {
            cascade_2 = u32(1);
        }
        else
        {
            if(distance_2 <= (camera_0.shadow_splits_0.z))
            {
                cascade_2 = u32(2);
            }
            else
            {
                cascade_2 = u32(3);
            }
        }
    }
    var visible_4 : f32 = cascade_visibility_0(cascade_2, position_3, geometric_normal_2, light_1);
    var start_2 : f32;
    if(cascade_2 == u32(0))
    {
        start_2 = camera_0.eye_0.w;
    }
    else
    {
        start_2 = camera_0.shadow_splits_0[max(cascade_2, u32(1)) - u32(1)];
    }
    var _S146 : f32 = max((camera_0.shadow_splits_0[cascade_2] - start_2) * clamp(camera_0.camera_forward_blend_0.w, 0.0f, 1.0f), 9.99999997475242708e-07f);
    var weight_3 : f32 = clamp((distance_2 - (camera_0.shadow_splits_0[cascade_2] - _S146)) / _S146, 0.0f, 1.0f);
    var visible_5 : f32;
    if(weight_3 > 0.0f)
    {
        if(cascade_2 < u32(3))
        {
            visible_5 = cascade_visibility_0(cascade_2 + u32(1), position_3, geometric_normal_2, light_1);
        }
        else
        {
            visible_5 = 1.0f;
        }
        visible_5 = mix(visible_4, visible_5, weight_3);
    }
    else
    {
        visible_5 = visible_4;
    }
    return visible_5;
}

fn evaluate_lighting_0( position_4 : vec3<f32>,  surface_2 : Surface_0,  brdf_inputs_0 : BrdfInputs_0) -> vec3<f32>
{
    var _S147 : u32 = baked_debug_0();
    var irradiance_1 : vec3<f32>;
    if(_S147 > u32(0))
    {
        if((surface_2.occlusion_0) == 0.0f)
        {
            return surface_2.emissive_0;
        }
        var value_6 : vec3<f32> = probe_irradiance_0(position_4, surface_2.normal_0, baked_blend_0());
        if(_S147 == u32(1))
        {
            irradiance_1 = value_6 / vec3<f32>(3.14159274101257324f);
        }
        else
        {
            irradiance_1 = value_6;
        }
        return irradiance_1;
    }
    var l_0 : vec3<f32> = unit_or_0(camera_0.light_direction_0.xyz, surface_2.normal_0);
    var h_0 : vec3<f32> = unit_or_0(brdf_inputs_0.view_0 + l_0, surface_2.normal_0);
    var _S148 : f32 = max(dot(surface_2.normal_0, l_0), 0.0f);
    var _S149 : f32 = max(dot(surface_2.normal_0, brdf_inputs_0.view_0), 0.0f);
    var direct_brdf_0 : vec3<f32> = evaluate_brdf_0(_S148, _S149, max(dot(surface_2.normal_0, h_0), 0.0f), max(dot(brdf_inputs_0.view_0, h_0), 0.0f), brdf_inputs_0.f0_0, brdf_inputs_0.diffuse_color_0, surface_2.roughness_0);
    var _S150 : f32 = max(camera_0.environment_0.x, 0.0f);
    var _S151 : vec3<f32> = vec3<f32>(0.0f);
    var _S152 : bool;
    if(_S150 > 0.0f)
    {
        _S152 = true;
    }
    else
    {
        _S152 = (camera_0.shadow_bias_0.w) >= 1.0f;
    }
    if(_S152)
    {
        _S152 = true;
    }
    else
    {
        _S152 = (abs(camera_0.environment_0.w)) >= 2.0f;
    }
    var indirect_0 : vec3<f32>;
    var shadow_visibility_0 : f32;
    if(_S152)
    {
        if((camera_0.shadow_bias_0.w) >= 1.0f)
        {
            if((surface_2.occlusion_0) > 0.0f)
            {
                var _S153 : vec3<f32> = probe_irradiance_0(position_4, surface_2.normal_0, baked_blend_0());
                irradiance_1 = _S153;
            }
            else
            {
                irradiance_1 = _S151;
            }
        }
        else
        {
            irradiance_1 = (textureSampleLevel((irradiance_image_0), (environment_sampler_0), (environment_uv_0(surface_2.normal_0)), (0.0f))).xyz;
        }
        var reflected_1 : vec3<f32> = reflect((vec3<f32>(0) - brdf_inputs_0.view_0), surface_2.normal_0);
        var prefiltered_0 : vec3<f32> = (textureSampleLevel((reflection_image_0), (environment_sampler_0), (environment_uv_0(reflected_1)), (surface_2.roughness_0 * max(camera_0.environment_0.z, 0.0f)))).xyz;
        var lut_size_0 : vec2<f32> = texture_size_0(brdf_image_0);
        var _S154 : vec2<f32> = vec2<f32>(0.5f);
        var brdf_0 : vec2<f32> = (textureSampleLevel((brdf_image_0), (environment_sampler_0), (clamp(vec2<f32>(_S149, surface_2.roughness_0), _S154 / lut_size_0, vec2<f32>(1.0f) - _S154 / lut_size_0)), (0.0f))).xy;
        var indirect_diffuse_0 : vec3<f32> = irradiance_1 * brdf_inputs_0.diffuse_color_0 / vec3<f32>(3.14159274101257324f) * vec3<f32>(surface_2.occlusion_0);
        var split_sum_0 : vec3<f32> = brdf_inputs_0.f0_0 * vec3<f32>(brdf_0.x) + vec3<f32>(brdf_0.y);
        var indirect_specular_0 : vec3<f32> = prefiltered_0 * split_sum_0;
        if((camera_0.shadow_bias_0.w) >= 1.0f)
        {
            irradiance_1 = indirect_diffuse_0 + indirect_specular_0 * vec3<f32>(_S150);
        }
        else
        {
            irradiance_1 = (indirect_diffuse_0 + indirect_specular_0) * vec3<f32>(_S150);
        }
        var probe_parameter_0 : f32 = abs(camera_0.environment_0.w);
        if(probe_parameter_0 >= 2.0f)
        {
            var weights_3 : vec3<f32>;
            var radiance_2 : vec3<f32> = local_specular_0(position_4, reflected_1, surface_2.roughness_0, prefiltered_0 * vec3<f32>(_S150), probe_parameter_0, &(weights_3));
            if(probe_parameter_0 >= 6.0f)
            {
                return radiance_2;
            }
            if(probe_parameter_0 >= 4.0f)
            {
                return weights_3;
            }
            if((weights_3.z) < 1.0f)
            {
                if((camera_0.shadow_bias_0.w) >= 1.0f)
                {
                    shadow_visibility_0 = 1.0f;
                }
                else
                {
                    shadow_visibility_0 = _S150;
                }
                indirect_0 = indirect_diffuse_0 * vec3<f32>(shadow_visibility_0) + radiance_2 * split_sum_0;
            }
            else
            {
                indirect_0 = irradiance_1;
            }
        }
        else
        {
            indirect_0 = irradiance_1;
        }
    }
    else
    {
        indirect_0 = _S151;
    }
    if(_S148 != 0.0f)
    {
        shadow_visibility_0 = sun_visibility_0(position_4, surface_2.geometric_normal_0, l_0);
    }
    else
    {
        shadow_visibility_0 = 1.0f;
    }
    return direct_brdf_0 * camera_0.light_color_0.xyz * vec3<f32>(_S148) * vec3<f32>(shadow_visibility_0) + indirect_0 + surface_2.emissive_0;
}

struct pixelOutput_0
{
    @location(0) output_0 : vec4<f32>,
};

struct pixelInput_0
{
    @location(0) world_position_0 : vec3<f32>,
    @location(1) world_normal_1 : vec3<f32>,
    @location(2) world_tangent_1 : vec4<f32>,
    @location(3) texcoord_1 : vec2<f32>,
    @location(5) lightmap_uv_0 : vec2<f32>,
    @interpolate(flat) @location(6) lightmap_slot_0 : u32,
};

@fragment
fn main( _S155 : pixelInput_0, @builtin(front_facing) front_facing_1 : bool) -> pixelOutput_0
{
    var sampled_0 : MaterialSample_0 = sample_material_0(_S155.world_normal_1, _S155.world_tangent_1, _S155.texcoord_1, front_facing_1);
    var surface_3 : Surface_0 = sampled_0.surface_0;
    apply_lightmap_0(&(surface_3), _S155.lightmap_slot_0, front_facing_1, _S155.lightmap_uv_0);
    var color_0 : vec3<f32> = evaluate_lighting_0(_S155.world_position_0, surface_3, make_brdf_inputs_0(_S155.world_position_0, surface_3));
    var _S156 : pixelOutput_0 = pixelOutput_0( vec4<f32>(clamp(color_0, vec3<f32>(0.0f), vec3<f32>(65504.0f)), sampled_0.output_alpha_0) );
    return _S156;
}
