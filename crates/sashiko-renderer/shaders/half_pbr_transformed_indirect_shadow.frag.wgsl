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

@binding(6) @group(0) var half_shadow_0 : texture_2d<f32>;

@binding(5) @group(0) var shadow_table_0 : texture_2d<f32>;

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
@binding(0) @group(3) var shadow_image0_0 : texture_2d<f32>;

@binding(4) @group(3) var shadow_sampler_0 : sampler;

@binding(1) @group(3) var shadow_image1_0 : texture_2d<f32>;

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

fn max_direction_magnitude_0( value_0 : vec3<f32>) -> u32
{
    var magnitude_0 : vec3<u32> = ((bitcast<vec3<u32>>((value_0))) & (vec3<u32>(u32(2147483647))));
    return max(max(magnitude_0.x, magnitude_0.y), magnitude_0.z);
}

fn extreme_direction_shift_0( magnitude_1 : u32) -> i32
{
    var _S4 : bool;
    if(magnitude_1 == u32(0))
    {
        _S4 = true;
    }
    else
    {
        _S4 = magnitude_1 >= u32(2139095040);
    }
    if(_S4)
    {
        _S4 = true;
    }
    else
    {
        if(magnitude_1 >= (bitcast<u32>((9.99999993922529029e-09f))))
        {
            _S4 = magnitude_1 <= (bitcast<u32>((1.0e+08f)));
        }
        else
        {
            _S4 = false;
        }
    }
    if(_S4)
    {
        return i32(0);
    }
    return clamp(i32(127) - i32((magnitude_1 >> (u32(23)))), i32(-126), i32(126));
}

fn shift_direction_0( value_1 : vec3<f32>,  shift_0 : i32) -> vec3<f32>
{
    var scaled_0 : vec3<f32> = value_1 * vec3<f32>((bitcast<f32>(((u32(shift_0 + i32(127)) << (u32(23)))))));
    if(shift_0 <= i32(22))
    {
        return scaled_0;
    }
    var bits_0 : vec3<u32> = (bitcast<vec3<u32>>((value_1)));
    return select(scaled_0, (bitcast<vec3<f32>>((((bitcast<vec3<u32>>((vec3<f32>((bits_0 & (vec3<u32>(u32(8388607))))) * vec3<f32>((bitcast<f32>(((u32(shift_0 - i32(22)) << (u32(23)))))))))) | (((bits_0 & (vec3<u32>(u32(2147483648)))))))))), ((bits_0 & (vec3<u32>(u32(2139095040))))) == vec3<u32>(u32(0)));
}

fn scale_extreme_direction_0( value_2 : vec3<f32>) -> vec3<f32>
{
    var shift_1 : i32 = extreme_direction_shift_0(max_direction_magnitude_0(value_2));
    var _S5 : vec3<f32>;
    if(shift_1 == i32(0))
    {
        _S5 = value_2;
    }
    else
    {
        _S5 = shift_direction_0(value_2, shift_1);
    }
    return _S5;
}

fn rsqrt_0( x_1 : f32) -> f32
{
    return 1.0f / sqrt(x_1);
}

fn unit_or_0( value_3 : vec3<f32>,  fallback_0 : vec3<f32>) -> vec3<f32>
{
    var squared_0 : f32 = dot(value_3, value_3);
    var _S6 : vec3<f32>;
    if(squared_0 > 9.99999968265522539e-21f)
    {
        _S6 = value_3 * vec3<f32>(rsqrt_0(squared_0));
    }
    else
    {
        _S6 = fallback_0;
    }
    return _S6;
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
    var _S7 : vec4<f32> = world_tangent_0;
    var dx_0 : vec2<f32> = dpdx(texcoord_0);
    var dy_0 : vec2<f32> = dpdy(texcoord_0);
    var _S8 : bool;
    if(!front_facing_0)
    {
        _S8 = (material_0.flags_0.y) == u32(0);
    }
    else
    {
        _S8 = false;
    }
    if(_S8)
    {
        discard;
    }
    var base_0 : vec4<f32> = material_0.base_factor_0 * (textureSampleGrad((base_image_0), (material_sampler_0), (texcoord_0), (dx_0), (dy_0)));
    var alpha_0 : f32 = clamp(base_0.w, 0.0f, 1.0f);
    var alpha_mode_0 : u32 = material_0.flags_0.x;
    if(alpha_mode_0 == u32(1))
    {
        _S8 = alpha_0 < (material_0.metallic_roughness_occlusion_cutoff_0.w);
    }
    else
    {
        _S8 = false;
    }
    if(_S8)
    {
        _S8 = true;
    }
    else
    {
        if(alpha_mode_0 == u32(2))
        {
            _S8 = alpha_0 <= 0.0f;
        }
        else
        {
            _S8 = false;
        }
    }
    if(_S8)
    {
        discard;
    }
    var result_0 : MaterialSample_0;
    result_0.surface_0.base_color_0 = base_0.xyz;
    var _S9 : f32;
    if(alpha_mode_0 == u32(2))
    {
        _S9 = alpha_0;
    }
    else
    {
        _S9 = 1.0f;
    }
    result_0.output_alpha_0 = _S9;
    var orm_0 : vec3<f32> = (textureSampleGrad((orm_image_0), (orm_sampler_0), (texcoord_0), (dx_0), (dy_0))).xyz;
    result_0.surface_0.metallic_0 = clamp(material_0.metallic_roughness_occlusion_cutoff_0.x * orm_0.z, 0.0f, 1.0f);
    result_0.surface_0.roughness_0 = clamp(material_0.metallic_roughness_occlusion_cutoff_0.y * orm_0.y, 0.0f, 1.0f);
    result_0.surface_0.occlusion_0 = mix(1.0f, orm_0.x, clamp(material_0.metallic_roughness_occlusion_cutoff_0.z, 0.0f, 1.0f));
    var _S10 : vec3<f32> = scale_extreme_direction_0(world_normal_0);
    var _S11 : vec3<f32> = scale_extreme_direction_0(_S7.xyz);
    _S7.x = _S11.x;
    _S7.y = _S11.y;
    _S7.z = _S11.z;
    const _S12 : vec3<f32> = vec3<f32>(0.0f, 0.0f, 1.0f);
    var n_0 : vec3<f32> = unit_or_0(_S10, _S12);
    var _S13 : vec3<f32>;
    if(front_facing_0)
    {
        _S13 = n_0;
    }
    else
    {
        _S13 = (vec3<f32>(0) - n_0);
    }
    result_0.surface_0.geometric_normal_0 = _S13;
    var _S14 : vec3<f32> = _S7.xyz - n_0 * vec3<f32>(dot(n_0, _S7.xyz));
    if((abs(n_0.z)) < 0.89999997615814209f)
    {
        _S13 = unit_or_0(cross(_S12, n_0), vec3<f32>(1.0f, 0.0f, 0.0f));
    }
    else
    {
        _S13 = unit_or_0(cross(vec3<f32>(0.0f, 1.0f, 0.0f), n_0), vec3<f32>(1.0f, 0.0f, 0.0f));
    }
    var t_0 : vec3<f32> = unit_or_0(_S14, _S13);
    var _S15 : vec3<f32> = cross(n_0, t_0);
    if((_S7.w) < 0.0f)
    {
        _S9 = -1.0f;
    }
    else
    {
        _S9 = 1.0f;
    }
    var b_0 : vec3<f32> = _S15 * vec3<f32>(_S9);
    var mapped_0 : vec3<f32> = (textureSampleGrad((normal_image_0), (normal_sampler_0), (texcoord_0), (dx_0), (dy_0))).xyz * vec3<f32>(2.0f) - vec3<f32>(1.0f);
    if((material_0.flags_0.z) != u32(0))
    {
        if((material_0.flags_0.z) == u32(2))
        {
            mapped_0[i32(1)] = - mapped_0.y;
        }
        mapped_0[i32(2)] = sqrt(saturate(1.0f - dot(mapped_0.xy, mapped_0.xy)));
    }
    var _S16 : vec2<f32> = mapped_0.xy * vec2<f32>(material_0.emissive_normal_scale_0.w);
    mapped_0.x = _S16.x;
    mapped_0.y = _S16.y;
    var n_1 : vec3<f32> = unit_or_0(t_0 * vec3<f32>(mapped_0.x) + b_0 * vec3<f32>(mapped_0.y) + n_0 * vec3<f32>(mapped_0.z), n_0);
    if(front_facing_0)
    {
        _S13 = n_1;
    }
    else
    {
        _S13 = (vec3<f32>(0) - n_1);
    }
    result_0.surface_0.normal_0 = _S13;
    result_0.surface_0.emissive_0 = material_0.emissive_normal_scale_0.xyz * (textureSampleGrad((emissive_image_0), (emissive_sampler_0), (texcoord_0), (dx_0), (dy_0))).xyz;
    return result_0;
}

fn baked_debug_0() -> u32
{
    return u32(clamp(floor((camera_0.shadow_bias_0.w - 1.0f) * 0.5f), 0.0f, 3.0f));
}

fn probe_load_0( index_0 : u32,  dims_0 : vec2<u32>) -> vec4<f32>
{
    var _S17 : u32 = dims_0.x;
    var _S18 : u32 = min(index_0, _S17 * dims_0.y - u32(1));
    var _S19 : u32 = _S18 % _S17;
    var _S20 : i32 = i32(_S19);
    var _S21 : u32 = _S18 / _S17;
    var _S22 : vec3<i32> = vec3<i32>(_S20, i32(_S21), i32(0));
    return (textureLoad((irradiance_image_0), ((_S22)).xy, ((_S22)).z));
}

fn probe_index_0( value_4 : f32,  maximum_0 : u32) -> u32
{
    var _S23 : f32;
    if(value_4 >= 0.0f)
    {
        _S23 = min(value_4, f32(maximum_0));
    }
    else
    {
        _S23 = 0.0f;
    }
    return u32(_S23);
}

fn baked_blend_0() -> f32
{
    return saturate(camera_0.shadow_bias_0.w - 1.0f - 2.0f * f32(baked_debug_0()));
}

fn surface_texel_0( index_1 : u32,  dims_1 : vec2<u32>,  keys_0 : u32,  phase_0 : f32,  start_0 : u32) -> vec3<f32>
{
    var at_0 : u32 = start_0 + index_1 * keys_0 + u32(phase_0);
    var _S24 : vec4<f32> = probe_load_0(at_0, dims_1);
    var value_5 : vec3<f32> = _S24.xyz;
    var _S25 : f32 = fract(phase_0);
    var value_6 : vec3<f32>;
    if(_S25 > 0.0f)
    {
        var _S26 : vec4<f32> = probe_load_0(at_0 + u32(1), dims_1);
        value_6 = mix(value_5, _S26.xyz, vec3<f32>(_S25));
    }
    else
    {
        value_6 = value_5;
    }
    return value_6;
}

fn shadow_texel_0( at_1 : u32,  width_0 : u32) -> vec4<f32>
{
    var _S27 : u32 = at_1 % width_0;
    var _S28 : i32 = i32(_S27);
    var _S29 : u32 = at_1 / width_0;
    var _S30 : vec3<i32> = vec3<i32>(_S28, i32(_S29), i32(0));
    return (textureLoad((shadow_table_0), ((_S30)).xy, ((_S30)).z));
}

fn oct_wrap_0( components_0 : vec2<f32>) -> vec2<f32>
{
    var _S31 : f32;
    if((components_0.x) >= 0.0f)
    {
        _S31 = 1.0f;
    }
    else
    {
        _S31 = -1.0f;
    }
    var _S32 : f32;
    if((components_0.y) >= 0.0f)
    {
        _S32 = 1.0f;
    }
    else
    {
        _S32 = -1.0f;
    }
    return (vec2<f32>(1.0f) - abs(components_0.yx)) * vec2<f32>(_S31, _S32);
}

fn oct_decode_0( oct_0 : vec2<f32>) -> vec3<f32>
{
    var _S33 : vec3<f32> = vec3<f32>(oct_0, 1.0f - abs(oct_0.x) - abs(oct_0.y));
    var direction_0 : vec3<f32> = _S33;
    if((_S33.z) < 0.0f)
    {
        var _S34 : vec2<f32> = oct_wrap_0(oct_0);
        direction_0.x = _S34.x;
        direction_0.y = _S34.y;
    }
    return unit_or_0(direction_0, vec3<f32>(0.0f, 0.0f, 1.0f));
}

fn unpack_geometric_normal_0( encoded_0 : f32) -> vec3<f32>
{
    var packed_0 : u32 = (((pack2x16float(vec2f((encoded_0),0.0))) - u32(1024)) & (u32(16383)));
    return oct_decode_0(vec2<f32>(f32((packed_0 & (u32(127)))), f32((packed_0 >> (u32(7))))) * vec2<f32>(0.015748031437397f) - vec2<f32>(1.0f));
}

fn indirect_visibility_0( position_0 : vec3<f32>,  normal_1 : vec3<f32>,  pixel_0 : vec2<f32>) -> f32
{
    var width_1 : u32;
    var height_0 : u32;
    {var dim = textureDimensions((shadow_table_0));((width_1)) = dim.x;((height_0)) = dim.y;};
    var texels_0 : u32 = width_1 * height_0;
    if(texels_0 < u32(260))
    {
        return 1.0f;
    }
    var _S35 : vec4<f32> = shadow_texel_0(u32(0), width_1);
    var _S36 : vec2<u32> = vec2<u32>(clamp(pixel_0 * _S35.xy, vec2<f32>(0.0f), vec2<f32>(15.0f)));
    var _S37 : vec4<f32> = shadow_texel_0(u32(1) + _S36.y * u32(16) + _S36.x, width_1);
    var range_0 : vec2<f32> = _S37.xy;
    var _S38 : u32 = texels_0 * u32(4);
    var _S39 : u32 = min(u32(range_0.x), _S38);
    var _S40 : u32 = min(u32(range_0.y), _S38 - _S39);
    var occlusion_1 : f32 = 0.0f;
    var j_0 : u32 = u32(0);
    for(;;)
    {
        if(j_0 < _S40)
        {
        }
        else
        {
            break;
        }
        var word_0 : u32 = _S39 + j_0;
        var _S41 : vec4<f32> = shadow_texel_0(word_0 / u32(4), width_1);
        var _S42 : u32 = min(u32(_S41[word_0 % u32(4)]), texels_0 - u32(3));
        var a_0 : vec4<f32> = shadow_texel_0(_S42, width_1);
        var _S43 : vec4<f32> = shadow_texel_0(_S42 + u32(1), width_1);
        var _S44 : vec3<f32> = a_0.xyz;
        var segment_0 : vec3<f32> = _S43.xyz - _S44;
        var lighting_0 : vec4<f32> = shadow_texel_0(_S42 + u32(2), width_1);
        var delta_0 : vec3<f32> = _S44 + segment_0 * vec3<f32>(saturate(dot(position_0 - _S44, segment_0) / max(dot(segment_0, segment_0), 9.99999968265522539e-21f))) - position_0;
        var distance_0 : f32 = length(delta_0);
        var _S45 : f32 = a_0.w;
        var _S46 : bool;
        if(distance_0 >= (6.0f * _S45))
        {
            _S46 = true;
        }
        else
        {
            _S46 = (dot(delta_0, normal_1)) <= (- _S45);
        }
        if(_S46)
        {
            j_0 = j_0 + u32(1);
            continue;
        }
        var _S47 : f32 = max(distance_0, 1.00000001335143196e-10f);
        var axis_0 : vec3<f32> = delta_0 / vec3<f32>(_S47);
        var ratio_0 : f32 = saturate(_S45 / _S47);
        var angle_0 : f32 = asin(ratio_0);
        occlusion_1 = max(occlusion_1, (1.0f - smoothstep(4.0f, 6.0f, distance_0 / _S45)) * mix(ratio_0 * ratio_0 * saturate(dot(axis_0, normal_1)), smoothstep(0.0f, 1.0f, (angle_0 + 0.64999997615814209f - acos(clamp(dot(axis_0, lighting_0.xyz), -1.0f, 1.0f))) / (2.0f * min(angle_0, 0.64999997615814209f))) * saturate(angle_0 * angle_0 / 0.42249995470046997f), lighting_0.w));
        j_0 = j_0 + u32(1);
    }
    return 1.0f - occlusion_1;
}

fn resolve_visibility_0( position_1 : vec3<f32>,  normal_2 : vec3<f32>,  pixel_1 : vec3<f32>,  slope_0 : vec2<f32>) -> f32
{
    var w_0 : u32;
    var h_0 : u32;
    {var dim = textureDimensions((half_shadow_0));((w_0)) = dim.x;((h_0)) = dim.y;};
    var _S48 : vec2<f32> = vec2<f32>(f32(w_0), f32(h_0));
    var _S49 : vec4<f32> = shadow_texel_0(u32(0), u32(1));
    var scale_0 : vec2<f32> = _S48 / _S49.zw;
    var _S50 : vec2<f32> = pixel_1.xy;
    var _S51 : vec2<f32> = vec2<f32>(0.5f);
    var _S52 : vec2<f32> = _S50 * scale_0 - _S51;
    var tw_0 : u32;
    var th_0 : u32;
    {var dim = textureDimensions((shadow_table_0));((tw_0)) = dim.x;((th_0)) = dim.y;};
    var _S53 : vec4<f32> = shadow_texel_0(u32(0), tw_0);
    var _S54 : vec2<u32> = vec2<u32>(clamp(_S50 * _S53.xy, vec2<f32>(0.0f), vec2<f32>(15.0f)));
    var _S55 : u32 = u32(1) + _S54.y * u32(16) + _S54.x;
    var _S56 : bool;
    if(_S55 >= (tw_0 * th_0))
    {
        _S56 = true;
    }
    else
    {
        var _S57 : vec4<f32> = shadow_texel_0(_S55, tw_0);
        _S56 = (_S57.y) == 0.0f;
    }
    if(_S56)
    {
        return 1.0f;
    }
    var lowest_0 : f32 = 1.0f;
    var highest_0 : f32 = 0.0f;
    var y_0 : i32 = i32(0);
    var visibility_0 : f32 = 0.0f;
    var total_0 : f32 = 0.0f;
    for(;;)
    {
        if(y_0 < i32(2))
        {
        }
        else
        {
            break;
        }
        var lowest_1 : f32 = lowest_0;
        var highest_1 : f32 = highest_0;
        var x_2 : i32 = i32(0);
        var visibility_1 : f32 = visibility_0;
        var total_1 : f32 = total_0;
        for(;;)
        {
            if(x_2 < i32(2))
            {
            }
            else
            {
                break;
            }
            var texel_0 : vec2<i32> = clamp(vec2<i32>(floor(_S52)) + vec2<i32>(x_2, y_0), vec2<i32>(i32(0), i32(0)), vec2<i32>(i32(w_0 - u32(1)), i32(h_0 - u32(1))));
            var _S58 : vec3<i32> = vec3<i32>(texel_0, i32(0));
            var sample_0 : vec4<f32> = (textureLoad((half_shadow_0), ((_S58)).xy, ((_S58)).z));
            var z_0 : f32 = (sample_0.y + sample_0.z) / 1024.0f;
            var projected_0 : f32 = pixel_1.z + dot((vec2<f32>(texel_0) + _S51) / scale_0 - _S50, slope_0);
            var _S59 : f32 = sample_0.w;
            if(_S59 == 0.0f)
            {
                _S56 = true;
            }
            else
            {
                _S56 = (abs(z_0 - projected_0)) > 9.99999997475242708e-07f;
            }
            var _S60 : bool;
            if(_S56)
            {
                _S60 = true;
            }
            else
            {
                _S60 = (dot(normal_2, unpack_geometric_normal_0(_S59))) < 0.98000001907348633f;
            }
            if(_S60)
            {
                x_2 = x_2 + i32(1);
                continue;
            }
            var _S61 : vec2<f32> = fract(_S52);
            var weights_0 : vec2<f32> = mix(vec2<f32>(1.0f) - _S61, _S61, vec2<f32>(f32(x_2), f32(y_0)));
            var weight_0 : f32 = weights_0.x * weights_0.y;
            var _S62 : f32 = sample_0.x;
            var visibility_2 : f32 = visibility_1 + _S62 * weight_0;
            var total_2 : f32 = total_1 + weight_0;
            var _S63 : f32 = max(highest_1, _S62);
            lowest_1 = min(lowest_1, _S62);
            highest_1 = _S63;
            visibility_1 = visibility_2;
            total_1 = total_2;
            x_2 = x_2 + i32(1);
        }
        var y_1 : i32 = y_0 + i32(1);
        lowest_0 = lowest_1;
        highest_0 = highest_1;
        y_0 = y_1;
        visibility_0 = visibility_1;
        total_0 = total_1;
    }
    if(total_0 > 0.0f)
    {
        _S56 = (highest_0 - lowest_0) < 0.10000000149011612f;
    }
    else
    {
        _S56 = false;
    }
    if(_S56)
    {
        return visibility_0 / total_0;
    }
    var _S64 : f32 = indirect_visibility_0(position_1, normal_2, _S50);
    return _S64;
}

fn apply_lightmap_0( s_0 : ptr<function, Surface_0>,  slot_0 : u32,  front_0 : bool,  uv_0 : vec2<f32>,  position_2 : vec3<f32>,  pixel_2 : vec3<f32>,  depth_slope_0 : vec2<f32>)
{
    var _S65 : u32 = baked_debug_0();
    if(_S65 > u32(0))
    {
        (*s_0).occlusion_0 = 1.0f;
        (*s_0).emissive_0 = vec3<f32>(0.0f);
    }
    var _S66 : bool;
    if(slot_0 == u32(0))
    {
        _S66 = true;
    }
    else
    {
        _S66 = (camera_0.shadow_bias_0.w) == 0.0f;
    }
    if(_S66)
    {
        return;
    }
    var dims_2 : vec2<u32>;
    var _S67 : u32 = dims_2[i32(0)];
    var _S68 : u32 = dims_2[i32(1)];
    {var dim = textureDimensions((irradiance_image_0));((_S67)) = dim.x;((_S68)) = dim.y;};
    dims_2[i32(0)] = _S67;
    dims_2[i32(1)] = _S68;
    var last_0 : u32 = dims_2.x * dims_2.y - u32(1);
    var header_0 : vec4<f32> = probe_load_0(u32(0), dims_2);
    if(slot_0 > (probe_index_0(header_0.x, last_0 / u32(2))))
    {
        return;
    }
    var keys_1 : u32 = clamp(probe_index_0(header_0.y, u32(16)), u32(1), u32(16));
    var start_1 : u32 = probe_index_0(header_0.z, last_0);
    var _S69 : u32 = u32(2) * slot_0;
    var rect_0 : vec4<f32> = probe_load_0(_S69 - u32(1), dims_2);
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
    var _S70 : vec4<f32> = probe_load_0(_S69, dims_2);
    var coord_0 : vec2<f32> = saturate(uv_0) * clamp(_S70.xy, vec2<f32>(1.0f), vec2<f32>(2048.0f)) - vec2<f32>(0.5f);
    var _S71 : vec2<i32> = vec2<i32>(floor(coord_0));
    var i_0 : u32 = u32(i32(origin_0) + _S71.x + _S71.y * i32(pitch_0));
    if((camera_0.shadow_bias_0.w) < 0.0f)
    {
        blend_0 = saturate(-1.0f - camera_0.shadow_bias_0.w);
    }
    else
    {
        blend_0 = baked_blend_0();
    }
    var phase_1 : f32 = blend_0 * f32(keys_1 - u32(1));
    var _S72 : vec3<f32> = surface_texel_0(i_0, dims_2, keys_1, phase_1, start_1);
    var _S73 : vec3<f32> = surface_texel_0(i_0 + u32(1), dims_2, keys_1, phase_1, start_1);
    var _S74 : vec3<f32> = vec3<f32>(fract(coord_0.x));
    var a_1 : vec3<f32> = mix(_S72, _S73, _S74);
    var _S75 : u32 = i_0 + pitch_0;
    var _S76 : vec3<f32> = surface_texel_0(_S75, dims_2, keys_1, phase_1, start_1);
    var _S77 : vec3<f32> = surface_texel_0(_S75 + u32(1), dims_2, keys_1, phase_1, start_1);
    var irradiance_0 : vec3<f32> = mix(a_1, mix(_S76, _S77, _S74), vec3<f32>(fract(coord_0.y)));
    var _S78 : f32 = resolve_visibility_0(position_2, (*s_0).geometric_normal_0, pixel_2, depth_slope_0);
    var irradiance_1 : vec3<f32> = irradiance_0 * vec3<f32>(_S78);
    if(_S65 == u32(0))
    {
        (*s_0).emissive_0 = (*s_0).emissive_0 + irradiance_1 * (*s_0).base_color_0 * vec3<f32>((1.0f - (*s_0).metallic_0)) * vec3<f32>((*s_0).occlusion_0) / vec3<f32>(3.14159274101257324f);
    }
    else
    {
        var _S79 : vec3<f32>;
        if(_S65 == u32(1))
        {
            _S79 = irradiance_1 / vec3<f32>(3.14159274101257324f);
        }
        else
        {
            _S79 = vec3<f32>(0.89999997615814209f, 0.30000001192092896f, 0.03999999910593033f);
        }
        (*s_0).emissive_0 = _S79;
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

fn make_brdf_inputs_0( position_3 : vec3<f32>,  surface_1 : Surface_0) -> BrdfInputs_0
{
    var result_1 : BrdfInputs_0;
    var to_eye_0 : vec3<f32>;
    if((camera_0.light_color_0.w) > 0.0f)
    {
        to_eye_0 = (vec3<f32>(0) - camera_0.camera_forward_blend_0.xyz);
    }
    else
    {
        to_eye_0 = camera_0.eye_0.xyz - position_3;
    }
    result_1.view_0 = unit_or_0(to_eye_0, surface_1.normal_0);
    result_1.f0_0 = mix(vec3<f32>(0.03999999910593033f), surface_1.base_color_0, vec3<f32>(surface_1.metallic_0));
    result_1.diffuse_color_0 = surface_1.base_color_0 * vec3<f32>((1.0f - surface_1.metallic_0));
    return result_1;
}

fn lighting_keys_0( tag_0 : f32) -> u32
{
    var _S80 : u32;
    if(tag_0 == 0.0f)
    {
        _S80 = u32(2);
    }
    else
    {
        _S80 = clamp(probe_index_0(abs(tag_0), u32(16)), u32(1), u32(16));
    }
    return _S80;
}

fn probe_coefficient_0( at_key_0 : u32,  coefficients_0 : u32,  mix_0 : f32,  dims_3 : vec2<u32>) -> vec3<f32>
{
    var _S81 : vec4<f32> = probe_load_0(at_key_0, dims_3);
    var value_7 : vec3<f32> = _S81.xyz;
    var value_8 : vec3<f32>;
    if(mix_0 > 0.0f)
    {
        var _S82 : vec4<f32> = probe_load_0(at_key_0 + coefficients_0, dims_3);
        value_8 = mix(value_7, _S82.xyz, vec3<f32>(mix_0));
    }
    else
    {
        value_8 = value_7;
    }
    return value_8;
}

fn sample_probe_volume_0( dims_4 : vec2<u32>,  bounds_0 : vec4<f32>,  f_0 : vec3<f32>,  n_2 : vec3<f32>,  layout_0 : vec4<f32>,  header_1 : u32,  stride_0 : u32,  coefficients_1 : u32,  key_0 : u32,  mix_1 : f32) -> vec3<f32>
{
    var maximum_1 : u32 = dims_4.x * dims_4.y - u32(1);
    var _S83 : bool;
    if((any((f_0 < vec3<f32>(0.0f)))))
    {
        _S83 = true;
    }
    else
    {
        _S83 = (any((f_0 > vec3<f32>(1.0f))));
    }
    var sky_0 : vec3<f32>;
    if(_S83)
    {
        var _S84 : vec4<f32> = probe_load_0(u32(2) + key_0, dims_4);
        var sky_1 : vec3<f32> = _S84.xyz;
        if(mix_1 > 0.0f)
        {
            var _S85 : vec4<f32> = probe_load_0(u32(3) + key_0, dims_4);
            sky_0 = mix(sky_1, _S85.xyz, vec3<f32>(mix_1));
        }
        else
        {
            sky_0 = sky_1;
        }
        return sky_0;
    }
    var node_count_0 : u32 = probe_index_0(layout_0.x, maximum_1 / u32(3));
    var _S86 : u32 = header_1 + node_count_0 * u32(3);
    var _S87 : f32 = bounds_0.w;
    var node_0 : u32 = u32(0);
    sky_0 = f_0;
    var depth_0 : u32 = u32(0);
    var size_0 : f32 = _S87;
    for(;;)
    {
        if(depth_0 <= u32(10))
        {
        }
        else
        {
            break;
        }
        var at_2 : u32 = header_1 + node_0 * u32(3);
        var _S88 : vec4<f32> = probe_load_0(at_2, dims_4);
        var first_0 : u32 = probe_index_0(_S88.x, node_count_0);
        if(first_0 == u32(0))
        {
            var _S89 : vec4<f32> = probe_load_0(at_2 + u32(1), dims_4);
            var _S90 : vec4<f32> = probe_load_0(at_2 + u32(2), dims_4);
            var _S91 : vec3<f32> = vec3<f32>(0.0f);
            var i_1 : u32 = u32(0);
            var sum_0 : vec3<f32> = _S91;
            var weights_1 : f32 = 0.0f;
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
                var _S92 : vec3<f32> = vec3<f32>(1.0f);
                var trilinear_0 : vec3<f32> = mix(_S92 - sky_0, sky_0, corner_0);
                var weight_1 : f32 = trilinear_0.x * trilinear_0.y * trilinear_0.z;
                var _S93 : f32;
                if(i_1 < u32(4))
                {
                    _S93 = _S89[(i_1 & (u32(3)))];
                }
                else
                {
                    _S93 = _S90[(i_1 & (u32(3)))];
                }
                var _S94 : u32 = maximum_1 / stride_0;
                var p_0 : u32 = _S86 + stride_0 * probe_index_0(_S93, _S94);
                var delta_1 : vec3<f32> = (sky_0 - corner_0) * vec3<f32>(size_0);
                var _S95 : u32 = p_0 + u32(2);
                var _S96 : vec4<f32> = probe_load_0(_S95, dims_4);
                var _S97 : f32 = _S96.w;
                var _S98 : vec4<f32> = probe_load_0(p_0 + u32(3), dims_4);
                var _S99 : f32 = _S98.w;
                var _S100 : vec4<f32> = probe_load_0(p_0 + u32(4), dims_4);
                var delta_2 : vec3<f32> = delta_1 - vec3<f32>(_S97, _S99, _S100.w);
                var minus_0 : vec4<f32> = probe_load_0(p_0, dims_4);
                var weight_2 : f32 = weight_1 * saturate(minus_0.w);
                var neg_0 : vec3<f32> = minus_0.xyz;
                var _S101 : vec4<f32> = probe_load_0(p_0 + u32(1), dims_4);
                var pos_0 : vec3<f32> = _S101.xyz;
                var _S102 : f32;
                if((delta_2.x) >= 0.0f)
                {
                    _S102 = pos_0.x;
                }
                else
                {
                    _S102 = neg_0.x;
                }
                var _S103 : f32;
                if((delta_2.y) >= 0.0f)
                {
                    _S103 = pos_0.y;
                }
                else
                {
                    _S103 = neg_0.y;
                }
                var _S104 : f32;
                if((delta_2.z) >= 0.0f)
                {
                    _S104 = pos_0.z;
                }
                else
                {
                    _S104 = neg_0.z;
                }
                var visibility_3 : vec3<f32> = saturate(_S92 + (vec3<f32>(_S102, _S103, _S104) - abs(delta_2)) / vec3<f32>(max(size_0 * 0.05000000074505806f, 9.99999997475242708e-07f)));
                var weight_3 : f32 = weight_2 * min(visibility_3.x, min(visibility_3.y, visibility_3.z));
                var _S105 : u32 = _S95 + coefficients_1 * key_0;
                var k_0 : u32 = u32(0);
                var sh_0 : vec3<f32> = _S91;
                for(;;)
                {
                    if(k_0 < u32(4))
                    {
                    }
                    else
                    {
                        break;
                    }
                    var _S106 : vec3<f32> = probe_coefficient_0(_S105 + k_0, coefficients_1, mix_1, dims_4);
                    var sh_1 : vec3<f32> = sh_0 + _S106 * vec3<f32>(vec4<f32>(1.0f, n_2)[k_0]);
                    k_0 = k_0 + u32(1);
                    sh_0 = sh_1;
                }
                var sh_2 : vec3<f32>;
                if(coefficients_1 == u32(9))
                {
                    var _S107 : f32 = n_2.x;
                    var _S108 : f32 = n_2.y;
                    var _S109 : f32 = n_2.z;
                    var _S110 : array<f32, i32(5)> = array<f32, i32(5)>( _S107 * _S108, _S108 * _S109, _S107 * _S109, _S107 * _S107 - _S108 * _S108, 3.0f * _S109 * _S109 - 1.0f );
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
                        var _S111 : vec3<f32> = probe_coefficient_0(_S105 + u32(4) + k_1, coefficients_1, mix_1, dims_4);
                        var sh_3 : vec3<f32> = sh_2 + _S111 * vec3<f32>(_S110[k_1]);
                        k_1 = k_1 + u32(1);
                        sh_2 = sh_3;
                    }
                }
                else
                {
                    sh_2 = sh_0;
                }
                var sum_1 : vec3<f32> = sum_0 + sh_2 * vec3<f32>(weight_3);
                var weights_2 : f32 = weights_1 + weight_3;
                i_1 = i_1 + u32(1);
                sum_0 = sum_1;
                weights_1 = weights_2;
            }
            var _S112 : u32 = baked_debug_0();
            if(_S112 == u32(2))
            {
                return vec3<f32>(0.07999999821186066f, 0.25f + 0.07000000029802322f * f32(depth_0), 0.80000001192092896f);
            }
            if(_S112 == u32(3))
            {
                return vec3<f32>(saturate(weights_1));
            }
            return max(sum_0 / vec3<f32>(max(weights_1, 9.99999997475242708e-07f)), _S91);
        }
        var _S113 : vec3<u32> = vec3<u32>(step(vec3<f32>(0.5f), sky_0));
        var _S114 : u32 = min(first_0 + _S113.x + u32(2) * _S113.y + u32(4) * _S113.z, node_count_0);
        var _S115 : vec3<f32> = saturate(vec3<f32>(2.0f) * sky_0 - vec3<f32>(_S113));
        var size_1 : f32 = size_0 * 0.5f;
        var depth_1 : u32 = depth_0 + u32(1);
        node_0 = _S114;
        sky_0 = _S115;
        depth_0 = depth_1;
        size_0 = size_1;
    }
    return vec3<f32>(0.0f);
}

fn probe_irradiance_0( position_4 : vec3<f32>,  n_3 : vec3<f32>,  blend_1 : f32) -> vec3<f32>
{
    var dims_5 : vec2<u32>;
    var _S116 : u32 = dims_5[i32(0)];
    var _S117 : u32 = dims_5[i32(1)];
    {var dim = textureDimensions((irradiance_image_0));((_S116)) = dim.x;((_S117)) = dim.y;};
    dims_5[i32(0)] = _S116;
    dims_5[i32(1)] = _S117;
    var bounds_1 : vec4<f32> = probe_load_0(u32(0), dims_5);
    var f_1 : vec3<f32> = (position_4 - bounds_1.xyz) / vec3<f32>(max(bounds_1.w, 9.99999968265522539e-21f));
    var layout_1 : vec4<f32> = probe_load_0(u32(1), dims_5);
    var _S118 : f32 = layout_1.w;
    var count_0 : u32 = lighting_keys_0(_S118);
    var phase_2 : f32 = saturate(blend_1) * f32(count_0 - u32(1));
    var key_1 : u32 = u32(phase_2);
    var mix_2 : f32 = fract(phase_2);
    var header_2 : u32 = u32(2) + max(count_0, u32(2));
    var coefficients_2 : u32;
    if(_S118 < 0.0f)
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
        var _S119 : vec3<f32> = sample_probe_volume_0(dims_5, bounds_1, f_1, n_3, layout_1, header_2, stride_1, coefficients_2, u32(0), 0.0f);
        return _S119;
    }
    var _S120 : vec3<f32> = sample_probe_volume_0(dims_5, bounds_1, f_1, n_3, layout_1, header_2, stride_1, coefficients_2, key_1, mix_2);
    return _S120;
}

fn evaluate_brdf_0( nl_0 : f32,  nv_0 : f32,  nh_0 : f32,  vh_0 : f32,  f0_1 : vec3<f32>,  diffuse_color_1 : vec3<f32>,  roughness_1 : f32) -> vec3<f32>
{
    var _S121 : f32 = max(roughness_1 * roughness_1, 0.00100000004749745f);
    var a2_0 : f32 = _S121 * _S121;
    var denominator_0 : f32 = nh_0 * nh_0 * (a2_0 - 1.0f) + 1.0f;
    var _S122 : f32 = 1.0f - a2_0;
    var _S123 : vec3<f32> = vec3<f32>(1.0f);
    var fresnel_0 : vec3<f32> = f0_1 + (_S123 - f0_1) * vec3<f32>(pow(clamp(1.0f - vh_0, 0.0f, 1.0f), 5.0f));
    return (_S123 - fresnel_0) * diffuse_color_1 / vec3<f32>(3.14159274101257324f) + fresnel_0 * vec3<f32>((a2_0 / max(3.14159274101257324f * denominator_0 * denominator_0, 9.999999960041972e-13f))) * vec3<f32>((0.5f / max(nv_0 * sqrt(a2_0 + _S122 * nl_0 * nl_0) + nl_0 * sqrt(a2_0 + _S122 * nv_0 * nv_0), 9.99999997475242708e-07f)));
}

fn environment_uv_0( direction_1 : vec3<f32>) -> vec2<f32>
{
    var c_0 : f32 = cos(camera_0.environment_0.y);
    var s_1 : f32 = sin(camera_0.environment_0.y);
    var _S124 : f32 = direction_1.x;
    var _S125 : f32 = direction_1.z;
    var _S126 : f32 = c_0 * _S124 - s_1 * _S125;
    var _S127 : f32 = direction_1.y;
    var _S128 : f32 = s_1 * _S124 + c_0 * _S125;
    var _S129 : vec2<f32> = vec2<f32>(_S126, _S128);
    var u_0 : f32;
    if((dot(_S129, _S129)) > 9.99999968265522539e-21f)
    {
        u_0 = fract(0.5f + atan2(_S128, _S126) / 6.28318548202514648f);
    }
    else
    {
        u_0 = 0.5f;
    }
    return vec2<f32>(u_0, acos(clamp(_S127, -1.0f, 1.0f)) / 3.14159274101257324f);
}

fn texture_size_0( image_0 : texture_2d<f32>) -> vec2<f32>
{
    var width_2 : u32;
    var height_1 : u32;
    var levels_0 : u32;
    {var dim = textureDimensions((image_0), (u32(0)));((width_2)) = dim.x;((height_1)) = dim.y;((levels_0)) = textureNumLevels((image_0));};
    return vec2<f32>(f32(width_2), f32(height_1));
}

fn isinf_0( x_3 : f32) -> bool
{
    var _S130 : u32 = (bitcast<u32>((x_3)));
    var _S131 : u32 = (_S130 & (u32(8388607)));
    var _S132 : bool;
    if(((((_S130 >> (u32(23)))) & (u32(255)))) == u32(255))
    {
        _S132 = _S131 == u32(0);
    }
    else
    {
        _S132 = false;
    }
    return _S132;
}

fn isfinite_0( x_4 : f32) -> bool
{
    var _S133 : bool;
    if(isinf_0(x_4))
    {
        _S133 = true;
    }
    else
    {
        _S133 = isnan_0(x_4);
    }
    return !_S133;
}

fn isfinite_1( x_5 : vec3<f32>) -> vec3<bool>
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
        result_2[i_2] = isfinite_0(x_5[i_2]);
        i_2 = i_2 + i32(1);
    }
    return result_2;
}

fn probe_level_0( uv_1 : vec2<f32>,  level_0 : u32,  first_row_0 : u32,  atlas_height_0 : f32) -> vec3<f32>
{
    var rows_0 : u32 = (u32(64) >> (level_0));
    var _S134 : u32;
    if(level_0 == u32(6))
    {
        _S134 = u32(128);
    }
    else
    {
        _S134 = u32(128) - u32(2) * rows_0;
    }
    var _S135 : f32 = f32(rows_0);
    return (textureSampleLevel((reflection_probes_0), (environment_sampler_0), (vec2<f32>(uv_1.x, (f32(first_row_0 + _S134) + clamp(uv_1.y * _S135, 0.5f, _S135 - 0.5f)) / atlas_height_0)), (0.0f))).xyz;
}

fn probe_specular_0( row_0 : u32,  ray_0 : vec3<f32>,  roughness_2 : f32,  blend_2 : f32,  height_2 : f32,  single_0 : bool) -> vec3<f32>
{
    var _S136 : vec2<f32> = ray_0.xz;
    var _S137 : f32;
    if((dot(_S136, _S136)) > 9.99999968265522539e-21f)
    {
        _S137 = 0.5f + atan2(ray_0.z, ray_0.x) / 6.28318548202514648f;
    }
    else
    {
        _S137 = 0.5f;
    }
    var uv_2 : vec2<f32> = vec2<f32>(_S137, acos(clamp(ray_0.y, -1.0f, 1.0f)) / 3.14159274101257324f);
    var lod_0 : f32 = saturate(roughness_2) * 6.0f;
    var a_2 : u32 = u32(lod_0);
    var _S138 : u32 = min(a_2 + u32(1), u32(6));
    var _S139 : bool;
    if(!single_0)
    {
        _S139 = blend_2 == 1.0f;
    }
    else
    {
        _S139 = false;
    }
    var _S140 : u32;
    if(_S139)
    {
        _S140 = row_0 + u32(132);
    }
    else
    {
        _S140 = row_0;
    }
    var _S141 : vec3<f32> = vec3<f32>(fract(lod_0));
    var day_0 : vec3<f32> = mix(probe_level_0(uv_2, a_2, _S140, height_2), probe_level_0(uv_2, _S138, _S140, height_2), _S141);
    if(single_0)
    {
        _S139 = true;
    }
    else
    {
        _S139 = blend_2 == 0.0f;
    }
    if(_S139)
    {
        _S139 = true;
    }
    else
    {
        _S139 = blend_2 == 1.0f;
    }
    if(_S139)
    {
        return day_0;
    }
    var _S142 : u32 = _S140 + u32(132);
    return mix(day_0, mix(probe_level_0(uv_2, a_2, _S142, height_2), probe_level_0(uv_2, _S138, _S142, height_2), _S141), vec3<f32>(blend_2));
}

fn local_specular_0( world_0 : vec3<f32>,  reflected_0 : vec3<f32>,  roughness_3 : f32,  global_0 : vec3<f32>,  parameter_0 : f32,  weights_3 : ptr<function, vec3<f32>>) -> vec3<f32>
{
    var width_3 : u32;
    var height_3 : u32;
    {var dim = textureDimensions((reflection_probes_0));((width_3)) = dim.x;((height_3)) = dim.y;};
    var _S143 : vec3<f32> = vec3<f32>(0.0f);
    (*weights_3) = _S143;
    var _S144 : f32 = saturate(parameter_0 - floor(parameter_0 * 0.5f) * 2.0f);
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
    if(width_3 == u32(128))
    {
        var _S145 : u32 = height_3 / rows_1;
        count_1 = min(min(_S145, ReflectionMetadata_0.probe_count_0), u32(32));
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
    var _S146 : bool;
    if(count_1 > u32(4))
    {
        _S146 = (ReflectionMetadata_0.grid_scale_0.x) > 0.0f;
    }
    else
    {
        _S146 = false;
    }
    if(_S146)
    {
        var coord_1 : vec3<f32> = (world_0 - ReflectionMetadata_0.grid_origin_0) * ReflectionMetadata_0.grid_scale_0;
        if((all((isfinite_1(coord_1)))))
        {
            var _S147 : vec3<u32> = vec3<u32>(clamp(coord_1, _S143, vec3<f32>(3.0f)));
            var index_2 : u32 = _S147.x + u32(4) * _S147.y + u32(16) * _S147.z;
            candidates_0 = (candidates_0 & ((ReflectionMetadata_0.candidate_masks_0[(index_2 >> (u32(2)))][(index_2 & (u32(3)))])));
        }
    }
    var radiance_0 : vec3<f32> = _S143;
    var total_3 : f32 = 0.0f;
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
        var _S148 : u32 = u32(3) * i_3;
        var capture_0 : vec4<f32> = ReflectionMetadata_0.probe_records_0[_S148];
        var _S149 : vec3<f32> = world_0 - ReflectionMetadata_0.probe_records_0[_S148 + u32(1)].xyz;
        var _S150 : vec3<f32> = ReflectionMetadata_0.probe_records_0[_S148 + u32(2)].xyz - world_0;
        var inside_0 : vec3<f32> = min(_S149, _S150);
        var _S151 : f32 = min(inside_0.x, min(inside_0.y, inside_0.z));
        if(_S151 > 0.0f)
        {
            _S146 = (capture_0.w) > 0.0f;
        }
        else
        {
            _S146 = false;
        }
        if(!_S146)
        {
            candidates_0 = candidates_1;
            continue;
        }
        var w_1 : f32 = saturate(_S151 / capture_0.w);
        var t_1 : vec3<f32> = mix(_S149, _S150, step(_S143, reflected_0)) / max(abs(reflected_0), vec3<f32>(9.99999968265522539e-21f));
        var radiance_1 : vec3<f32> = radiance_0 + vec3<f32>(w_1) * probe_specular_0(row_1, unit_or_0(world_0 - capture_0.xyz + vec3<f32>(min(t_1.x, min(t_1.y, t_1.z))) * reflected_0, reflected_0), roughness_3, _S144, f32(height_3), single_1);
        var total_4 : f32 = total_3 + w_1;
        (*weights_3)[i_3 % u32(2)] = (*weights_3)[i_3 % u32(2)] + w_1;
        candidates_0 = candidates_1;
        radiance_0 = radiance_1;
        total_3 = total_4;
    }
    var _S152 : f32 = max(1.0f, total_3);
    var _S153 : vec2<f32> = (*weights_3).xy / vec2<f32>(_S152);
    (*weights_3).x = _S153.x;
    (*weights_3).y = _S153.y;
    (*weights_3)[i32(2)] = saturate(1.0f - total_3);
    return radiance_0 / vec3<f32>(_S152) + vec3<f32>((*weights_3).z) * global_0;
}

fn shadow_depth_0( cascade_0 : u32,  uv_3 : vec2<f32>) -> f32
{
    if(cascade_0 == u32(0))
    {
        return (textureSampleLevel((shadow_image0_0), (shadow_sampler_0), (uv_3), (0.0f))).x;
    }
    if(cascade_0 == u32(1))
    {
        return (textureSampleLevel((shadow_image1_0), (shadow_sampler_0), (uv_3), (0.0f))).x;
    }
    if(cascade_0 == u32(2))
    {
        return (textureSampleLevel((shadow_image2_0), (shadow_sampler_0), (uv_3), (0.0f))).x;
    }
    return (textureSampleLevel((shadow_image3_0), (shadow_sampler_0), (uv_3), (0.0f))).x;
}

fn cascade_visibility_0( cascade_1 : u32,  position_5 : vec3<f32>,  geometric_normal_1 : vec3<f32>,  light_0 : vec3<f32>) -> f32
{
    var _S154 : u32 = min(cascade_1, u32(3));
    var slope_1 : f32 = 1.0f - max(dot(geometric_normal_1, light_0), 0.0f);
    var texel_world_0 : f32 = camera_0.shadow_texel_world_0[_S154];
    var clip_0 : vec4<f32> = (((mat4x4<f32>(camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(0)][i32(0)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(0)][i32(1)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(0)][i32(2)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(0)][i32(3)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(1)][i32(0)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(1)][i32(1)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(1)][i32(2)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(1)][i32(3)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(2)][i32(0)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(2)][i32(1)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(2)][i32(2)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(2)][i32(3)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(3)][i32(0)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(3)][i32(1)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(3)][i32(2)], camera_0.shadow_view_projection_0.data_1[_S154].data_0[i32(3)][i32(3)])) * (vec4<f32>(position_5 + geometric_normal_1 * vec3<f32>((camera_0.shadow_texel_world_0[_S154] * camera_0.shadow_bias_0.z * slope_1)), 1.0f))));
    var _S155 : f32 = clip_0.w;
    if(!(_S155 > 9.99999968265522539e-21f))
    {
        return 1.0f;
    }
    var ndc_0 : vec3<f32> = clip_0.xyz / vec3<f32>(_S155);
    var _S156 : vec2<f32> = vec2<f32>(0.5f);
    var uv_4 : vec2<f32> = ndc_0.xy * _S156 + _S156;
    var _S157 : vec2<f32> = vec2<f32>(0.0f);
    var _S158 : bool;
    if((any((uv_4 < _S157))))
    {
        _S158 = true;
    }
    else
    {
        _S158 = (any((uv_4 > vec2<f32>(1.0f))));
    }
    if(_S158)
    {
        _S158 = true;
    }
    else
    {
        _S158 = (ndc_0.z) < 0.0f;
    }
    if(_S158)
    {
        _S158 = true;
    }
    else
    {
        _S158 = (ndc_0.z) > 1.0f;
    }
    if(_S158)
    {
        return 1.0f;
    }
    var _S159 : f32 = ndc_0.z - texel_world_0 * camera_0.shadow_depth_scale_0[_S154] * (camera_0.shadow_bias_0.x + camera_0.shadow_bias_0.y * slope_1);
    var _S160 : vec2<f32> = vec2<f32>(1.0f);
    var _S161 : vec2<f32> = _S160 / texture_size_0(shadow_image0_0);
    var y_2 : i32 = i32(-1);
    var visible_0 : f32 = 0.0f;
    for(;;)
    {
        if(y_2 <= i32(1))
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
            var tap_0 : vec2<f32> = uv_4 + vec2<f32>(f32(x_6), f32(y_2)) * _S161;
            if((any((tap_0 < _S157))))
            {
                _S158 = true;
            }
            else
            {
                _S158 = (any((tap_0 > _S160)));
            }
            var visible_1 : f32;
            if(_S158)
            {
                visible_1 = visible_0 + 1.0f;
            }
            else
            {
                if(_S159 <= (shadow_depth_0(_S154, tap_0)))
                {
                    visible_1 = 1.0f;
                }
                else
                {
                    visible_1 = 0.0f;
                }
                visible_1 = visible_0 + visible_1;
            }
            x_6 = x_6 + i32(1);
            visible_0 = visible_1;
        }
        y_2 = y_2 + i32(1);
    }
    return visible_0 / 9.0f;
}

fn sun_visibility_0( position_6 : vec3<f32>,  geometric_normal_2 : vec3<f32>,  light_1 : vec3<f32>) -> f32
{
    if(!((camera_0.environment_0.w) > 0.0f))
    {
        return 1.0f;
    }
    var distance_1 : f32 = dot(position_6 - camera_0.eye_0.xyz, camera_0.camera_forward_blend_0.xyz);
    var _S162 : bool;
    if(distance_1 < (camera_0.eye_0.w))
    {
        _S162 = true;
    }
    else
    {
        _S162 = distance_1 > (camera_0.shadow_splits_0.w);
    }
    if(_S162)
    {
        return 1.0f;
    }
    var cascade_2 : u32;
    if(distance_1 <= (camera_0.shadow_splits_0.x))
    {
        cascade_2 = u32(0);
    }
    else
    {
        if(distance_1 <= (camera_0.shadow_splits_0.y))
        {
            cascade_2 = u32(1);
        }
        else
        {
            if(distance_1 <= (camera_0.shadow_splits_0.z))
            {
                cascade_2 = u32(2);
            }
            else
            {
                cascade_2 = u32(3);
            }
        }
    }
    var visible_2 : f32 = cascade_visibility_0(cascade_2, position_6, geometric_normal_2, light_1);
    var start_2 : f32;
    if(cascade_2 == u32(0))
    {
        start_2 = camera_0.eye_0.w;
    }
    else
    {
        start_2 = camera_0.shadow_splits_0[max(cascade_2, u32(1)) - u32(1)];
    }
    var _S163 : f32 = max((camera_0.shadow_splits_0[cascade_2] - start_2) * clamp(camera_0.camera_forward_blend_0.w, 0.0f, 1.0f), 9.99999997475242708e-07f);
    var weight_4 : f32 = clamp((distance_1 - (camera_0.shadow_splits_0[cascade_2] - _S163)) / _S163, 0.0f, 1.0f);
    var visible_3 : f32;
    if(weight_4 > 0.0f)
    {
        if(cascade_2 < u32(3))
        {
            visible_3 = cascade_visibility_0(cascade_2 + u32(1), position_6, geometric_normal_2, light_1);
        }
        else
        {
            visible_3 = 1.0f;
        }
        visible_3 = mix(visible_2, visible_3, weight_4);
    }
    else
    {
        visible_3 = visible_2;
    }
    return visible_3;
}

fn evaluate_lighting_0( position_7 : vec3<f32>,  surface_2 : Surface_0,  brdf_inputs_0 : BrdfInputs_0) -> vec3<f32>
{
    var _S164 : u32 = baked_debug_0();
    var irradiance_2 : vec3<f32>;
    if(_S164 > u32(0))
    {
        if((surface_2.occlusion_0) == 0.0f)
        {
            return surface_2.emissive_0;
        }
        var value_9 : vec3<f32> = probe_irradiance_0(position_7, surface_2.normal_0, baked_blend_0());
        if(_S164 == u32(1))
        {
            irradiance_2 = value_9 / vec3<f32>(3.14159274101257324f);
        }
        else
        {
            irradiance_2 = value_9;
        }
        return irradiance_2;
    }
    var l_0 : vec3<f32> = unit_or_0(camera_0.light_direction_0.xyz, surface_2.normal_0);
    var h_1 : vec3<f32> = unit_or_0(brdf_inputs_0.view_0 + l_0, surface_2.normal_0);
    var _S165 : f32 = max(dot(surface_2.normal_0, l_0), 0.0f);
    var _S166 : f32 = max(dot(surface_2.normal_0, brdf_inputs_0.view_0), 0.0f);
    var direct_brdf_0 : vec3<f32> = evaluate_brdf_0(_S165, _S166, max(dot(surface_2.normal_0, h_1), 0.0f), max(dot(brdf_inputs_0.view_0, h_1), 0.0f), brdf_inputs_0.f0_0, brdf_inputs_0.diffuse_color_0, surface_2.roughness_0);
    var _S167 : f32 = max(camera_0.environment_0.x, 0.0f);
    var _S168 : vec3<f32> = vec3<f32>(0.0f);
    var _S169 : bool;
    if(_S167 > 0.0f)
    {
        _S169 = true;
    }
    else
    {
        _S169 = (camera_0.shadow_bias_0.w) >= 1.0f;
    }
    if(_S169)
    {
        _S169 = true;
    }
    else
    {
        _S169 = (abs(camera_0.environment_0.w)) >= 2.0f;
    }
    var indirect_0 : vec3<f32>;
    var shadow_visibility_0 : f32;
    if(_S169)
    {
        if((camera_0.shadow_bias_0.w) >= 1.0f)
        {
            if((surface_2.occlusion_0) > 0.0f)
            {
                var _S170 : vec3<f32> = probe_irradiance_0(position_7, surface_2.normal_0, baked_blend_0());
                irradiance_2 = _S170;
            }
            else
            {
                irradiance_2 = _S168;
            }
        }
        else
        {
            irradiance_2 = (textureSampleLevel((irradiance_image_0), (environment_sampler_0), (environment_uv_0(surface_2.normal_0)), (0.0f))).xyz;
        }
        var reflected_1 : vec3<f32> = reflect((vec3<f32>(0) - brdf_inputs_0.view_0), surface_2.normal_0);
        var prefiltered_0 : vec3<f32> = (textureSampleLevel((reflection_image_0), (environment_sampler_0), (environment_uv_0(reflected_1)), (surface_2.roughness_0 * max(camera_0.environment_0.z, 0.0f)))).xyz;
        var lut_size_0 : vec2<f32> = texture_size_0(brdf_image_0);
        var _S171 : vec2<f32> = vec2<f32>(0.5f);
        var brdf_0 : vec2<f32> = (textureSampleLevel((brdf_image_0), (environment_sampler_0), (clamp(vec2<f32>(_S166, surface_2.roughness_0), _S171 / lut_size_0, vec2<f32>(1.0f) - _S171 / lut_size_0)), (0.0f))).xy;
        var indirect_diffuse_0 : vec3<f32> = irradiance_2 * brdf_inputs_0.diffuse_color_0 / vec3<f32>(3.14159274101257324f) * vec3<f32>(surface_2.occlusion_0);
        var split_sum_0 : vec3<f32> = brdf_inputs_0.f0_0 * vec3<f32>(brdf_0.x) + vec3<f32>(brdf_0.y);
        var indirect_specular_0 : vec3<f32> = prefiltered_0 * split_sum_0;
        if((camera_0.shadow_bias_0.w) >= 1.0f)
        {
            irradiance_2 = indirect_diffuse_0 + indirect_specular_0 * vec3<f32>(_S167);
        }
        else
        {
            irradiance_2 = (indirect_diffuse_0 + indirect_specular_0) * vec3<f32>(_S167);
        }
        var probe_parameter_0 : f32 = abs(camera_0.environment_0.w);
        if(probe_parameter_0 >= 2.0f)
        {
            var weights_4 : vec3<f32>;
            var radiance_2 : vec3<f32> = local_specular_0(position_7, reflected_1, surface_2.roughness_0, prefiltered_0 * vec3<f32>(_S167), probe_parameter_0, &(weights_4));
            if(probe_parameter_0 >= 6.0f)
            {
                return radiance_2;
            }
            if(probe_parameter_0 >= 4.0f)
            {
                return weights_4;
            }
            if((weights_4.z) < 1.0f)
            {
                if((camera_0.shadow_bias_0.w) >= 1.0f)
                {
                    shadow_visibility_0 = 1.0f;
                }
                else
                {
                    shadow_visibility_0 = _S167;
                }
                indirect_0 = indirect_diffuse_0 * vec3<f32>(shadow_visibility_0) + radiance_2 * split_sum_0;
            }
            else
            {
                indirect_0 = irradiance_2;
            }
        }
        else
        {
            indirect_0 = irradiance_2;
        }
    }
    else
    {
        indirect_0 = _S168;
    }
    if(_S165 != 0.0f)
    {
        shadow_visibility_0 = sun_visibility_0(position_7, surface_2.geometric_normal_0, l_0);
    }
    else
    {
        shadow_visibility_0 = 1.0f;
    }
    return direct_brdf_0 * camera_0.light_color_0.xyz * vec3<f32>(_S165) * vec3<f32>(shadow_visibility_0) + indirect_0 + surface_2.emissive_0;
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
    @interpolate(flat) @location(4) mirrored_0 : u32,
    @location(5) lightmap_uv_0 : vec2<f32>,
    @interpolate(flat) @location(6) lightmap_slot_0 : u32,
};

struct PixelInput_0
{
     world_position_1 : vec3<f32>,
     world_normal_2 : vec3<f32>,
     world_tangent_2 : vec4<f32>,
     texcoord_2 : vec2<f32>,
    mirrored_1 : u32,
     lightmap_uv_1 : vec2<f32>,
    lightmap_slot_1 : u32,
     pixel_3 : vec4<f32>,
     front_facing_1 : bool,
};

@fragment
fn main( _S172 : pixelInput_0, @builtin(position) pixel_4 : vec4<f32>, @builtin(front_facing) front_facing_2 : bool) -> pixelOutput_0
{
    var _S173 : PixelInput_0;
    _S173.world_position_1 = _S172.world_position_0;
    _S173.world_normal_2 = _S172.world_normal_1;
    _S173.world_tangent_2 = _S172.world_tangent_1;
    _S173.texcoord_2 = _S172.texcoord_1;
    _S173.mirrored_1 = _S172.mirrored_0;
    _S173.lightmap_uv_1 = _S172.lightmap_uv_0;
    _S173.lightmap_slot_1 = _S172.lightmap_slot_0;
    _S173.pixel_3 = pixel_4;
    _S173.front_facing_1 = front_facing_2;
    var depth_slope_1 : vec2<f32> = vec2<f32>(dpdx(_S173.pixel_3.z), dpdy(_S173.pixel_3.z));
    var _S174 : bool = (_S173.front_facing_1) != ((_S173.mirrored_1) != u32(0));
    _S173.front_facing_1 = _S174;
    var sampled_0 : MaterialSample_0 = sample_material_0(_S173.world_normal_2, _S173.world_tangent_2, _S173.texcoord_2, _S174);
    var surface_3 : Surface_0 = sampled_0.surface_0;
    apply_lightmap_0(&(surface_3), _S173.lightmap_slot_1, _S174, _S173.lightmap_uv_1, _S173.world_position_1, _S173.pixel_3.xyz, depth_slope_1);
    var color_0 : vec3<f32> = evaluate_lighting_0(_S173.world_position_1, surface_3, make_brdf_inputs_0(_S173.world_position_1, surface_3));
    var _S175 : pixelOutput_0 = pixelOutput_0( vec4<f32>(clamp(color_0, vec3<f32>(0.0f), vec3<f32>(65504.0f)), sampled_0.output_alpha_0) );
    return _S175;
}
