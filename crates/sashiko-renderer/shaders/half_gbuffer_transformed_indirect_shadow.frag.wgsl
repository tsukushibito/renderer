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

fn oct_encode_0( direction_1 : vec3<f32>) -> vec2<f32>
{
    var _S80 : f32 = direction_1.z;
    var oct_1 : vec2<f32> = direction_1.xy / vec2<f32>(max(abs(direction_1.x) + abs(direction_1.y) + abs(_S80), 9.99999968265522539e-21f));
    var _S81 : vec2<f32>;
    if(_S80 >= 0.0f)
    {
        _S81 = oct_1;
    }
    else
    {
        _S81 = oct_wrap_0(oct_1);
    }
    return _S81;
}

fn isnan_1( x_3 : vec2<f32>) -> vec2<bool>
{
    var result_1 : vec2<bool>;
    var i_1 : i32 = i32(0);
    for(;;)
    {
        if(i_1 < i32(2))
        {
        }
        else
        {
            break;
        }
        result_1[i_1] = isnan_0(x_3[i_1]);
        i_1 = i_1 + i32(1);
    }
    return result_1;
}

fn pack_geometric_normal_0( direction_2 : vec3<f32>) -> f32
{
    var _S82 : vec2<f32> = vec2<f32>(0.5f);
    var encoded_1 : vec2<f32> = oct_encode_0(direction_2) * _S82 + _S82;
    var encoded_2 : vec2<f32>;
    if((any((isnan_1(encoded_1)))))
    {
        encoded_2 = vec2<f32>(0.5f);
    }
    else
    {
        encoded_2 = encoded_1;
    }
    var _S83 : vec2<u32> = vec2<u32>(round(clamp(encoded_2, vec2<f32>(0.0f), vec2<f32>(1.0f)) * vec2<f32>(127.0f)));
    return (unpack2x16float((u32(1024) + _S83.x + (((_S83.y) << (u32(7)))))).x);
}

struct GBuffer_0
{
    @location(0) base_metallic_0 : vec4<f32>,
    @location(1) normal_roughness_0 : vec4<f32>,
    @location(2) emissive_occlusion_0 : vec4<f32>,
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
fn main( _S84 : pixelInput_0, @builtin(position) pixel_4 : vec4<f32>, @builtin(front_facing) front_facing_2 : bool) -> GBuffer_0
{
    var _S85 : PixelInput_0;
    _S85.world_position_1 = _S84.world_position_0;
    _S85.world_normal_2 = _S84.world_normal_1;
    _S85.world_tangent_2 = _S84.world_tangent_1;
    _S85.texcoord_2 = _S84.texcoord_1;
    _S85.mirrored_1 = _S84.mirrored_0;
    _S85.lightmap_uv_1 = _S84.lightmap_uv_0;
    _S85.lightmap_slot_1 = _S84.lightmap_slot_0;
    _S85.pixel_3 = pixel_4;
    _S85.front_facing_1 = front_facing_2;
    var depth_slope_1 : vec2<f32> = vec2<f32>(dpdx(_S85.pixel_3.z), dpdy(_S85.pixel_3.z));
    var _S86 : bool = (_S85.front_facing_1) != ((_S85.mirrored_1) != u32(0));
    _S85.front_facing_1 = _S86;
    var _S87 : MaterialSample_0 = sample_material_0(_S85.world_normal_2, _S85.world_tangent_2, _S85.texcoord_2, _S86);
    var surface_1 : Surface_0 = _S87.surface_0;
    apply_lightmap_0(&(surface_1), _S85.lightmap_slot_1, _S86, _S85.lightmap_uv_1, _S85.world_position_1, _S85.pixel_3.xyz, depth_slope_1);
    var result_2 : GBuffer_0;
    result_2.base_metallic_0 = vec4<f32>(surface_1.base_color_0, surface_1.metallic_0);
    result_2.normal_roughness_0 = vec4<f32>(oct_encode_0(surface_1.normal_0), surface_1.roughness_0, pack_geometric_normal_0(surface_1.geometric_normal_0));
    result_2.emissive_occlusion_0 = vec4<f32>(min(surface_1.emissive_0, vec3<f32>(65504.0f)), surface_1.occlusion_0);
    return result_2;
}
