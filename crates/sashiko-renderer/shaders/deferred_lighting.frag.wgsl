@binding(3) @group(1) var depth_image_0 : texture_2d<f32>;

@binding(4) @group(1) var gbuffer_sampler_0 : sampler;

struct _MatrixStorage_float4x4std140_0
{
    @align(16) data_0 : array<vec4<f32>, i32(4)>,
};

struct SLANG_ParameterGroup_Reconstruction_std140_0
{
    @align(16) inverse_view_projection_0 : _MatrixStorage_float4x4std140_0,
};

@binding(1) @group(0) var<uniform> Reconstruction_0 : SLANG_ParameterGroup_Reconstruction_std140_0;
@binding(0) @group(1) var base_metallic_image_0 : texture_2d<f32>;

@binding(1) @group(1) var normal_roughness_image_0 : texture_2d<f32>;

@binding(2) @group(1) var emissive_occlusion_image_0 : texture_2d<f32>;

struct _MatrixStorage_float4x4_ColMajorstd140_0
{
    @align(16) data_1 : array<vec4<f32>, i32(4)>,
};

struct _Array_std140_matrixx3Cfloatx2C4x2C4x3E4_0
{
    @align(16) data_2 : array<_MatrixStorage_float4x4_ColMajorstd140_0, i32(4)>,
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
@binding(0) @group(3) var shadow_image0_0 : texture_2d<f32>;

@binding(4) @group(3) var shadow_sampler_0 : sampler;

@binding(1) @group(3) var shadow_image1_0 : texture_2d<f32>;

@binding(2) @group(3) var shadow_image2_0 : texture_2d<f32>;

@binding(3) @group(3) var shadow_image3_0 : texture_2d<f32>;

fn texture_size_0( image_0 : texture_2d<f32>) -> vec2<f32>
{
    var width_0 : u32;
    var height_0 : u32;
    var levels_0 : u32;
    {var dim = textureDimensions((image_0), (u32(0)));((width_0)) = dim.x;((height_0)) = dim.y;((levels_0)) = textureNumLevels((image_0));};
    return vec2<f32>(f32(width_0), f32(height_0));
}

fn oct_wrap_0( components_0 : vec2<f32>) -> vec2<f32>
{
    var _S1 : f32;
    if((components_0.x) >= 0.0f)
    {
        _S1 = 1.0f;
    }
    else
    {
        _S1 = -1.0f;
    }
    var _S2 : f32;
    if((components_0.y) >= 0.0f)
    {
        _S2 = 1.0f;
    }
    else
    {
        _S2 = -1.0f;
    }
    return (vec2<f32>(1.0f) - abs(components_0.yx)) * vec2<f32>(_S1, _S2);
}

fn rsqrt_0( x_0 : f32) -> f32
{
    return 1.0f / sqrt(x_0);
}

fn unit_or_0( value_0 : vec3<f32>,  fallback_0 : vec3<f32>) -> vec3<f32>
{
    var squared_0 : f32 = dot(value_0, value_0);
    var _S3 : vec3<f32>;
    if(squared_0 > 9.99999968265522539e-21f)
    {
        _S3 = value_0 * vec3<f32>(rsqrt_0(squared_0));
    }
    else
    {
        _S3 = fallback_0;
    }
    return _S3;
}

fn oct_decode_0( oct_0 : vec2<f32>) -> vec3<f32>
{
    var _S4 : vec3<f32> = vec3<f32>(oct_0, 1.0f - abs(oct_0.x) - abs(oct_0.y));
    var direction_0 : vec3<f32> = _S4;
    if((_S4.z) < 0.0f)
    {
        var _S5 : vec2<f32> = oct_wrap_0(oct_0);
        direction_0.x = _S5.x;
        direction_0.y = _S5.y;
    }
    return unit_or_0(direction_0, vec3<f32>(0.0f, 0.0f, 1.0f));
}

fn unpack_geometric_normal_0( encoded_0 : f32) -> vec3<f32>
{
    var packed_0 : u32 = (((pack2x16float(vec2f((encoded_0),0.0))) - u32(1024)) & (u32(16383)));
    return oct_decode_0(vec2<f32>(f32((packed_0 & (u32(127)))), f32((packed_0 >> (u32(7))))) * vec2<f32>(0.015748031437397f) - vec2<f32>(1.0f));
}

fn isnan_0( x_1 : f32) -> bool
{
    var _S6 : u32 = (bitcast<u32>((x_1)));
    var _S7 : u32 = (_S6 & (u32(8388607)));
    var _S8 : bool;
    if(((((_S6 >> (u32(23)))) & (u32(255)))) == u32(255))
    {
        _S8 = _S7 != u32(0);
    }
    else
    {
        _S8 = false;
    }
    return _S8;
}

struct BrdfInputs_0
{
     view_0 : vec3<f32>,
     f0_0 : vec3<f32>,
     diffuse_color_0 : vec3<f32>,
};

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

fn make_brdf_inputs_0( position_0 : vec3<f32>,  surface_0 : Surface_0) -> BrdfInputs_0
{
    var result_0 : BrdfInputs_0;
    var to_eye_0 : vec3<f32>;
    if((camera_0.light_color_0.w) > 0.0f)
    {
        to_eye_0 = (vec3<f32>(0) - camera_0.camera_forward_blend_0.xyz);
    }
    else
    {
        to_eye_0 = camera_0.eye_0.xyz - position_0;
    }
    result_0.view_0 = unit_or_0(to_eye_0, surface_0.normal_0);
    result_0.f0_0 = mix(vec3<f32>(0.03999999910593033f), surface_0.base_color_0, vec3<f32>(surface_0.metallic_0));
    result_0.diffuse_color_0 = surface_0.base_color_0 * vec3<f32>((1.0f - surface_0.metallic_0));
    return result_0;
}

fn baked_debug_0() -> u32
{
    return u32(clamp(floor((camera_0.shadow_bias_0.w - 1.0f) * 0.5f), 0.0f, 3.0f));
}

fn baked_blend_0() -> f32
{
    return saturate(camera_0.shadow_bias_0.w - 1.0f - 2.0f * f32(baked_debug_0()));
}

fn probe_load_0( index_0 : u32,  dims_0 : vec2<u32>) -> vec4<f32>
{
    var _S9 : u32 = dims_0.x;
    var _S10 : u32 = min(index_0, _S9 * dims_0.y - u32(1));
    var _S11 : u32 = _S10 % _S9;
    var _S12 : i32 = i32(_S11);
    var _S13 : u32 = _S10 / _S9;
    var _S14 : vec3<i32> = vec3<i32>(_S12, i32(_S13), i32(0));
    return (textureLoad((irradiance_image_0), ((_S14)).xy, ((_S14)).z));
}

fn probe_index_0( value_1 : f32,  maximum_0 : u32) -> u32
{
    var _S15 : f32;
    if(value_1 >= 0.0f)
    {
        _S15 = min(value_1, f32(maximum_0));
    }
    else
    {
        _S15 = 0.0f;
    }
    return u32(_S15);
}

fn lighting_keys_0( tag_0 : f32) -> u32
{
    var _S16 : u32;
    if(tag_0 == 0.0f)
    {
        _S16 = u32(2);
    }
    else
    {
        _S16 = clamp(probe_index_0(abs(tag_0), u32(16)), u32(1), u32(16));
    }
    return _S16;
}

fn probe_coefficient_0( at_key_0 : u32,  coefficients_0 : u32,  mix_0 : f32,  dims_1 : vec2<u32>) -> vec3<f32>
{
    var _S17 : vec4<f32> = probe_load_0(at_key_0, dims_1);
    var value_2 : vec3<f32> = _S17.xyz;
    var value_3 : vec3<f32>;
    if(mix_0 > 0.0f)
    {
        var _S18 : vec4<f32> = probe_load_0(at_key_0 + coefficients_0, dims_1);
        value_3 = mix(value_2, _S18.xyz, vec3<f32>(mix_0));
    }
    else
    {
        value_3 = value_2;
    }
    return value_3;
}

fn sample_probe_volume_0( dims_2 : vec2<u32>,  bounds_0 : vec4<f32>,  f_0 : vec3<f32>,  n_0 : vec3<f32>,  layout_0 : vec4<f32>,  header_0 : u32,  stride_0 : u32,  coefficients_1 : u32,  key_0 : u32,  mix_1 : f32) -> vec3<f32>
{
    var maximum_1 : u32 = dims_2.x * dims_2.y - u32(1);
    var _S19 : bool;
    if((any((f_0 < vec3<f32>(0.0f)))))
    {
        _S19 = true;
    }
    else
    {
        _S19 = (any((f_0 > vec3<f32>(1.0f))));
    }
    var sky_0 : vec3<f32>;
    if(_S19)
    {
        var _S20 : vec4<f32> = probe_load_0(u32(2) + key_0, dims_2);
        var sky_1 : vec3<f32> = _S20.xyz;
        if(mix_1 > 0.0f)
        {
            var _S21 : vec4<f32> = probe_load_0(u32(3) + key_0, dims_2);
            sky_0 = mix(sky_1, _S21.xyz, vec3<f32>(mix_1));
        }
        else
        {
            sky_0 = sky_1;
        }
        return sky_0;
    }
    var node_count_0 : u32 = probe_index_0(layout_0.x, maximum_1 / u32(3));
    var _S22 : u32 = header_0 + node_count_0 * u32(3);
    var _S23 : f32 = bounds_0.w;
    var node_0 : u32 = u32(0);
    sky_0 = f_0;
    var depth_0 : u32 = u32(0);
    var size_0 : f32 = _S23;
    for(;;)
    {
        if(depth_0 <= u32(10))
        {
        }
        else
        {
            break;
        }
        var at_0 : u32 = header_0 + node_0 * u32(3);
        var _S24 : vec4<f32> = probe_load_0(at_0, dims_2);
        var first_0 : u32 = probe_index_0(_S24.x, node_count_0);
        if(first_0 == u32(0))
        {
            var _S25 : vec4<f32> = probe_load_0(at_0 + u32(1), dims_2);
            var _S26 : vec4<f32> = probe_load_0(at_0 + u32(2), dims_2);
            var _S27 : vec3<f32> = vec3<f32>(0.0f);
            var i_0 : u32 = u32(0);
            var sum_0 : vec3<f32> = _S27;
            var weights_0 : f32 = 0.0f;
            for(;;)
            {
                if(i_0 < u32(8))
                {
                }
                else
                {
                    break;
                }
                var corner_0 : vec3<f32> = vec3<f32>(f32((i_0 & (u32(1)))), f32((((i_0 >> (u32(1)))) & (u32(1)))), f32((i_0 >> (u32(2)))));
                var _S28 : vec3<f32> = vec3<f32>(1.0f);
                var trilinear_0 : vec3<f32> = mix(_S28 - sky_0, sky_0, corner_0);
                var weight_0 : f32 = trilinear_0.x * trilinear_0.y * trilinear_0.z;
                var _S29 : f32;
                if(i_0 < u32(4))
                {
                    _S29 = _S25[(i_0 & (u32(3)))];
                }
                else
                {
                    _S29 = _S26[(i_0 & (u32(3)))];
                }
                var _S30 : u32 = maximum_1 / stride_0;
                var p_0 : u32 = _S22 + stride_0 * probe_index_0(_S29, _S30);
                var delta_0 : vec3<f32> = (sky_0 - corner_0) * vec3<f32>(size_0);
                var _S31 : u32 = p_0 + u32(2);
                var _S32 : vec4<f32> = probe_load_0(_S31, dims_2);
                var _S33 : f32 = _S32.w;
                var _S34 : vec4<f32> = probe_load_0(p_0 + u32(3), dims_2);
                var _S35 : f32 = _S34.w;
                var _S36 : vec4<f32> = probe_load_0(p_0 + u32(4), dims_2);
                var delta_1 : vec3<f32> = delta_0 - vec3<f32>(_S33, _S35, _S36.w);
                var minus_0 : vec4<f32> = probe_load_0(p_0, dims_2);
                var weight_1 : f32 = weight_0 * saturate(minus_0.w);
                var neg_0 : vec3<f32> = minus_0.xyz;
                var _S37 : vec4<f32> = probe_load_0(p_0 + u32(1), dims_2);
                var pos_0 : vec3<f32> = _S37.xyz;
                var _S38 : f32;
                if((delta_1.x) >= 0.0f)
                {
                    _S38 = pos_0.x;
                }
                else
                {
                    _S38 = neg_0.x;
                }
                var _S39 : f32;
                if((delta_1.y) >= 0.0f)
                {
                    _S39 = pos_0.y;
                }
                else
                {
                    _S39 = neg_0.y;
                }
                var _S40 : f32;
                if((delta_1.z) >= 0.0f)
                {
                    _S40 = pos_0.z;
                }
                else
                {
                    _S40 = neg_0.z;
                }
                var visibility_0 : vec3<f32> = saturate(_S28 + (vec3<f32>(_S38, _S39, _S40) - abs(delta_1)) / vec3<f32>(max(size_0 * 0.05000000074505806f, 9.99999997475242708e-07f)));
                var weight_2 : f32 = weight_1 * min(visibility_0.x, min(visibility_0.y, visibility_0.z));
                var _S41 : u32 = _S31 + coefficients_1 * key_0;
                var k_0 : u32 = u32(0);
                var sh_0 : vec3<f32> = _S27;
                for(;;)
                {
                    if(k_0 < u32(4))
                    {
                    }
                    else
                    {
                        break;
                    }
                    var _S42 : vec3<f32> = probe_coefficient_0(_S41 + k_0, coefficients_1, mix_1, dims_2);
                    var sh_1 : vec3<f32> = sh_0 + _S42 * vec3<f32>(vec4<f32>(1.0f, n_0)[k_0]);
                    k_0 = k_0 + u32(1);
                    sh_0 = sh_1;
                }
                var sh_2 : vec3<f32>;
                if(coefficients_1 == u32(9))
                {
                    var _S43 : f32 = n_0.x;
                    var _S44 : f32 = n_0.y;
                    var _S45 : f32 = n_0.z;
                    var _S46 : array<f32, i32(5)> = array<f32, i32(5)>( _S43 * _S44, _S44 * _S45, _S43 * _S45, _S43 * _S43 - _S44 * _S44, 3.0f * _S45 * _S45 - 1.0f );
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
                        var _S47 : vec3<f32> = probe_coefficient_0(_S41 + u32(4) + k_1, coefficients_1, mix_1, dims_2);
                        var sh_3 : vec3<f32> = sh_2 + _S47 * vec3<f32>(_S46[k_1]);
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
                i_0 = i_0 + u32(1);
                sum_0 = sum_1;
                weights_0 = weights_1;
            }
            var _S48 : u32 = baked_debug_0();
            if(_S48 == u32(2))
            {
                return vec3<f32>(0.07999999821186066f, 0.25f + 0.07000000029802322f * f32(depth_0), 0.80000001192092896f);
            }
            if(_S48 == u32(3))
            {
                return vec3<f32>(saturate(weights_0));
            }
            return max(sum_0 / vec3<f32>(max(weights_0, 9.99999997475242708e-07f)), _S27);
        }
        var _S49 : vec3<u32> = vec3<u32>(step(vec3<f32>(0.5f), sky_0));
        var _S50 : u32 = min(first_0 + _S49.x + u32(2) * _S49.y + u32(4) * _S49.z, node_count_0);
        var _S51 : vec3<f32> = saturate(vec3<f32>(2.0f) * sky_0 - vec3<f32>(_S49));
        var size_1 : f32 = size_0 * 0.5f;
        var depth_1 : u32 = depth_0 + u32(1);
        node_0 = _S50;
        sky_0 = _S51;
        depth_0 = depth_1;
        size_0 = size_1;
    }
    return vec3<f32>(0.0f);
}

fn probe_irradiance_0( position_1 : vec3<f32>,  n_1 : vec3<f32>,  blend_0 : f32) -> vec3<f32>
{
    var dims_3 : vec2<u32>;
    var _S52 : u32 = dims_3[i32(0)];
    var _S53 : u32 = dims_3[i32(1)];
    {var dim = textureDimensions((irradiance_image_0));((_S52)) = dim.x;((_S53)) = dim.y;};
    dims_3[i32(0)] = _S52;
    dims_3[i32(1)] = _S53;
    var bounds_1 : vec4<f32> = probe_load_0(u32(0), dims_3);
    var f_1 : vec3<f32> = (position_1 - bounds_1.xyz) / vec3<f32>(max(bounds_1.w, 9.99999968265522539e-21f));
    var layout_1 : vec4<f32> = probe_load_0(u32(1), dims_3);
    var _S54 : f32 = layout_1.w;
    var count_0 : u32 = lighting_keys_0(_S54);
    var phase_0 : f32 = saturate(blend_0) * f32(count_0 - u32(1));
    var key_1 : u32 = u32(phase_0);
    var mix_2 : f32 = fract(phase_0);
    var header_1 : u32 = u32(2) + max(count_0, u32(2));
    var coefficients_2 : u32;
    if(_S54 < 0.0f)
    {
        coefficients_2 = u32(9);
    }
    else
    {
        coefficients_2 = u32(4);
    }
    var stride_1 : u32 = u32(2) + coefficients_2 * count_0;
    if(phase_0 == 0.0f)
    {
        var _S55 : vec3<f32> = sample_probe_volume_0(dims_3, bounds_1, f_1, n_1, layout_1, header_1, stride_1, coefficients_2, u32(0), 0.0f);
        return _S55;
    }
    var _S56 : vec3<f32> = sample_probe_volume_0(dims_3, bounds_1, f_1, n_1, layout_1, header_1, stride_1, coefficients_2, key_1, mix_2);
    return _S56;
}

fn evaluate_brdf_0( nl_0 : f32,  nv_0 : f32,  nh_0 : f32,  vh_0 : f32,  f0_1 : vec3<f32>,  diffuse_color_1 : vec3<f32>,  roughness_1 : f32) -> vec3<f32>
{
    var _S57 : f32 = max(roughness_1 * roughness_1, 0.00100000004749745f);
    var a2_0 : f32 = _S57 * _S57;
    var denominator_0 : f32 = nh_0 * nh_0 * (a2_0 - 1.0f) + 1.0f;
    var _S58 : f32 = 1.0f - a2_0;
    var _S59 : vec3<f32> = vec3<f32>(1.0f);
    var fresnel_0 : vec3<f32> = f0_1 + (_S59 - f0_1) * vec3<f32>(pow(clamp(1.0f - vh_0, 0.0f, 1.0f), 5.0f));
    return (_S59 - fresnel_0) * diffuse_color_1 / vec3<f32>(3.14159274101257324f) + fresnel_0 * vec3<f32>((a2_0 / max(3.14159274101257324f * denominator_0 * denominator_0, 9.999999960041972e-13f))) * vec3<f32>((0.5f / max(nv_0 * sqrt(a2_0 + _S58 * nl_0 * nl_0) + nl_0 * sqrt(a2_0 + _S58 * nv_0 * nv_0), 9.99999997475242708e-07f)));
}

fn environment_uv_0( direction_1 : vec3<f32>) -> vec2<f32>
{
    var c_0 : f32 = cos(camera_0.environment_0.y);
    var s_0 : f32 = sin(camera_0.environment_0.y);
    var _S60 : f32 = direction_1.x;
    var _S61 : f32 = direction_1.z;
    var _S62 : f32 = c_0 * _S60 - s_0 * _S61;
    var _S63 : f32 = direction_1.y;
    var _S64 : f32 = s_0 * _S60 + c_0 * _S61;
    var _S65 : vec2<f32> = vec2<f32>(_S62, _S64);
    var u_0 : f32;
    if((dot(_S65, _S65)) > 9.99999968265522539e-21f)
    {
        u_0 = fract(0.5f + atan2(_S64, _S62) / 6.28318548202514648f);
    }
    else
    {
        u_0 = 0.5f;
    }
    return vec2<f32>(u_0, acos(clamp(_S63, -1.0f, 1.0f)) / 3.14159274101257324f);
}

fn isinf_0( x_2 : f32) -> bool
{
    var _S66 : u32 = (bitcast<u32>((x_2)));
    var _S67 : u32 = (_S66 & (u32(8388607)));
    var _S68 : bool;
    if(((((_S66 >> (u32(23)))) & (u32(255)))) == u32(255))
    {
        _S68 = _S67 == u32(0);
    }
    else
    {
        _S68 = false;
    }
    return _S68;
}

fn isfinite_0( x_3 : f32) -> bool
{
    var _S69 : bool;
    if(isinf_0(x_3))
    {
        _S69 = true;
    }
    else
    {
        _S69 = isnan_0(x_3);
    }
    return !_S69;
}

fn isfinite_1( x_4 : vec3<f32>) -> vec3<bool>
{
    var result_1 : vec3<bool>;
    var i_1 : i32 = i32(0);
    for(;;)
    {
        if(i_1 < i32(3))
        {
        }
        else
        {
            break;
        }
        result_1[i_1] = isfinite_0(x_4[i_1]);
        i_1 = i_1 + i32(1);
    }
    return result_1;
}

fn probe_level_0( uv_0 : vec2<f32>,  level_0 : u32,  first_row_0 : u32,  atlas_height_0 : f32) -> vec3<f32>
{
    var rows_0 : u32 = (u32(64) >> (level_0));
    var _S70 : u32;
    if(level_0 == u32(6))
    {
        _S70 = u32(128);
    }
    else
    {
        _S70 = u32(128) - u32(2) * rows_0;
    }
    var _S71 : f32 = f32(rows_0);
    return (textureSampleLevel((reflection_probes_0), (environment_sampler_0), (vec2<f32>(uv_0.x, (f32(first_row_0 + _S70) + clamp(uv_0.y * _S71, 0.5f, _S71 - 0.5f)) / atlas_height_0)), (0.0f))).xyz;
}

fn probe_specular_0( row_0 : u32,  ray_0 : vec3<f32>,  roughness_2 : f32,  blend_1 : f32,  height_1 : f32,  single_0 : bool) -> vec3<f32>
{
    var _S72 : vec2<f32> = ray_0.xz;
    var _S73 : f32;
    if((dot(_S72, _S72)) > 9.99999968265522539e-21f)
    {
        _S73 = 0.5f + atan2(ray_0.z, ray_0.x) / 6.28318548202514648f;
    }
    else
    {
        _S73 = 0.5f;
    }
    var uv_1 : vec2<f32> = vec2<f32>(_S73, acos(clamp(ray_0.y, -1.0f, 1.0f)) / 3.14159274101257324f);
    var lod_0 : f32 = saturate(roughness_2) * 6.0f;
    var a_0 : u32 = u32(lod_0);
    var _S74 : u32 = min(a_0 + u32(1), u32(6));
    var _S75 : bool;
    if(!single_0)
    {
        _S75 = blend_1 == 1.0f;
    }
    else
    {
        _S75 = false;
    }
    var _S76 : u32;
    if(_S75)
    {
        _S76 = row_0 + u32(132);
    }
    else
    {
        _S76 = row_0;
    }
    var _S77 : vec3<f32> = vec3<f32>(fract(lod_0));
    var day_0 : vec3<f32> = mix(probe_level_0(uv_1, a_0, _S76, height_1), probe_level_0(uv_1, _S74, _S76, height_1), _S77);
    if(single_0)
    {
        _S75 = true;
    }
    else
    {
        _S75 = blend_1 == 0.0f;
    }
    if(_S75)
    {
        _S75 = true;
    }
    else
    {
        _S75 = blend_1 == 1.0f;
    }
    if(_S75)
    {
        return day_0;
    }
    var _S78 : u32 = _S76 + u32(132);
    return mix(day_0, mix(probe_level_0(uv_1, a_0, _S78, height_1), probe_level_0(uv_1, _S74, _S78, height_1), _S77), vec3<f32>(blend_1));
}

fn local_specular_0( world_0 : vec3<f32>,  reflected_0 : vec3<f32>,  roughness_3 : f32,  global_0 : vec3<f32>,  parameter_0 : f32,  weights_2 : ptr<function, vec3<f32>>) -> vec3<f32>
{
    var width_1 : u32;
    var height_2 : u32;
    {var dim = textureDimensions((reflection_probes_0));((width_1)) = dim.x;((height_2)) = dim.y;};
    var _S79 : vec3<f32> = vec3<f32>(0.0f);
    (*weights_2) = _S79;
    var _S80 : f32 = saturate(parameter_0 - floor(parameter_0 * 0.5f) * 2.0f);
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
        var _S81 : u32 = height_2 / rows_1;
        count_1 = min(min(_S81, ReflectionMetadata_0.probe_count_0), u32(32));
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
    var _S82 : bool;
    if(count_1 > u32(4))
    {
        _S82 = (ReflectionMetadata_0.grid_scale_0.x) > 0.0f;
    }
    else
    {
        _S82 = false;
    }
    if(_S82)
    {
        var coord_0 : vec3<f32> = (world_0 - ReflectionMetadata_0.grid_origin_0) * ReflectionMetadata_0.grid_scale_0;
        if((all((isfinite_1(coord_0)))))
        {
            var _S83 : vec3<u32> = vec3<u32>(clamp(coord_0, _S79, vec3<f32>(3.0f)));
            var index_1 : u32 = _S83.x + u32(4) * _S83.y + u32(16) * _S83.z;
            candidates_0 = (candidates_0 & ((ReflectionMetadata_0.candidate_masks_0[(index_1 >> (u32(2)))][(index_1 & (u32(3)))])));
        }
    }
    var radiance_0 : vec3<f32> = _S79;
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
        var i_2 : u32 = firstTrailingBit(candidates_0);
        var candidates_1 : u32 = (candidates_0 & ((candidates_0 - u32(1))));
        var row_1 : u32 = i_2 * rows_1;
        var _S84 : u32 = u32(3) * i_2;
        var capture_0 : vec4<f32> = ReflectionMetadata_0.probe_records_0[_S84];
        var _S85 : vec3<f32> = world_0 - ReflectionMetadata_0.probe_records_0[_S84 + u32(1)].xyz;
        var _S86 : vec3<f32> = ReflectionMetadata_0.probe_records_0[_S84 + u32(2)].xyz - world_0;
        var inside_0 : vec3<f32> = min(_S85, _S86);
        var _S87 : f32 = min(inside_0.x, min(inside_0.y, inside_0.z));
        if(_S87 > 0.0f)
        {
            _S82 = (capture_0.w) > 0.0f;
        }
        else
        {
            _S82 = false;
        }
        if(!_S82)
        {
            candidates_0 = candidates_1;
            continue;
        }
        var w_0 : f32 = saturate(_S87 / capture_0.w);
        var t_0 : vec3<f32> = mix(_S85, _S86, step(_S79, reflected_0)) / max(abs(reflected_0), vec3<f32>(9.99999968265522539e-21f));
        var radiance_1 : vec3<f32> = radiance_0 + vec3<f32>(w_0) * probe_specular_0(row_1, unit_or_0(world_0 - capture_0.xyz + vec3<f32>(min(t_0.x, min(t_0.y, t_0.z))) * reflected_0, reflected_0), roughness_3, _S80, f32(height_2), single_1);
        var total_1 : f32 = total_0 + w_0;
        (*weights_2)[i_2 % u32(2)] = (*weights_2)[i_2 % u32(2)] + w_0;
        candidates_0 = candidates_1;
        radiance_0 = radiance_1;
        total_0 = total_1;
    }
    var _S88 : f32 = max(1.0f, total_0);
    var _S89 : vec2<f32> = (*weights_2).xy / vec2<f32>(_S88);
    (*weights_2).x = _S89.x;
    (*weights_2).y = _S89.y;
    (*weights_2)[i32(2)] = saturate(1.0f - total_0);
    return radiance_0 / vec3<f32>(_S88) + vec3<f32>((*weights_2).z) * global_0;
}

fn shadow_depth_0( cascade_0 : u32,  uv_2 : vec2<f32>) -> f32
{
    if(cascade_0 == u32(0))
    {
        return (textureSampleLevel((shadow_image0_0), (shadow_sampler_0), (uv_2), (0.0f))).x;
    }
    if(cascade_0 == u32(1))
    {
        return (textureSampleLevel((shadow_image1_0), (shadow_sampler_0), (uv_2), (0.0f))).x;
    }
    if(cascade_0 == u32(2))
    {
        return (textureSampleLevel((shadow_image2_0), (shadow_sampler_0), (uv_2), (0.0f))).x;
    }
    return (textureSampleLevel((shadow_image3_0), (shadow_sampler_0), (uv_2), (0.0f))).x;
}

fn cascade_visibility_0( cascade_1 : u32,  position_2 : vec3<f32>,  geometric_normal_1 : vec3<f32>,  light_0 : vec3<f32>) -> f32
{
    var _S90 : u32 = min(cascade_1, u32(3));
    var slope_0 : f32 = 1.0f - max(dot(geometric_normal_1, light_0), 0.0f);
    var texel_world_0 : f32 = camera_0.shadow_texel_world_0[_S90];
    var clip_0 : vec4<f32> = (((mat4x4<f32>(camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(0)][i32(0)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(1)][i32(0)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(2)][i32(0)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(3)][i32(0)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(0)][i32(1)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(1)][i32(1)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(2)][i32(1)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(3)][i32(1)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(0)][i32(2)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(1)][i32(2)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(2)][i32(2)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(3)][i32(2)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(0)][i32(3)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(1)][i32(3)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(2)][i32(3)], camera_0.shadow_view_projection_0.data_2[_S90].data_1[i32(3)][i32(3)])) * (vec4<f32>(position_2 + geometric_normal_1 * vec3<f32>((camera_0.shadow_texel_world_0[_S90] * camera_0.shadow_bias_0.z * slope_0)), 1.0f))));
    var _S91 : f32 = clip_0.w;
    if(!(_S91 > 9.99999968265522539e-21f))
    {
        return 1.0f;
    }
    var ndc_0 : vec3<f32> = clip_0.xyz / vec3<f32>(_S91);
    var _S92 : vec2<f32> = vec2<f32>(0.5f);
    var uv_3 : vec2<f32> = ndc_0.xy * _S92 + _S92;
    var _S93 : vec2<f32> = vec2<f32>(0.0f);
    var _S94 : bool;
    if((any((uv_3 < _S93))))
    {
        _S94 = true;
    }
    else
    {
        _S94 = (any((uv_3 > vec2<f32>(1.0f))));
    }
    if(_S94)
    {
        _S94 = true;
    }
    else
    {
        _S94 = (ndc_0.z) < 0.0f;
    }
    if(_S94)
    {
        _S94 = true;
    }
    else
    {
        _S94 = (ndc_0.z) > 1.0f;
    }
    if(_S94)
    {
        return 1.0f;
    }
    var _S95 : f32 = ndc_0.z - texel_world_0 * camera_0.shadow_depth_scale_0[_S90] * (camera_0.shadow_bias_0.x + camera_0.shadow_bias_0.y * slope_0);
    var _S96 : vec2<f32> = vec2<f32>(1.0f);
    var _S97 : vec2<f32> = _S96 / texture_size_0(shadow_image0_0);
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
            var tap_0 : vec2<f32> = uv_3 + vec2<f32>(f32(x_5), f32(y_0)) * _S97;
            if((any((tap_0 < _S93))))
            {
                _S94 = true;
            }
            else
            {
                _S94 = (any((tap_0 > _S96)));
            }
            var visible_1 : f32;
            if(_S94)
            {
                visible_1 = visible_0 + 1.0f;
            }
            else
            {
                if(_S95 <= (shadow_depth_0(_S90, tap_0)))
                {
                    visible_1 = 1.0f;
                }
                else
                {
                    visible_1 = 0.0f;
                }
                visible_1 = visible_0 + visible_1;
            }
            x_5 = x_5 + i32(1);
            visible_0 = visible_1;
        }
        y_0 = y_0 + i32(1);
    }
    return visible_0 / 9.0f;
}

fn sun_visibility_0( position_3 : vec3<f32>,  geometric_normal_2 : vec3<f32>,  light_1 : vec3<f32>) -> f32
{
    if(!((camera_0.environment_0.w) > 0.0f))
    {
        return 1.0f;
    }
    var distance_0 : f32 = dot(position_3 - camera_0.eye_0.xyz, camera_0.camera_forward_blend_0.xyz);
    var _S98 : bool;
    if(distance_0 < (camera_0.eye_0.w))
    {
        _S98 = true;
    }
    else
    {
        _S98 = distance_0 > (camera_0.shadow_splits_0.w);
    }
    if(_S98)
    {
        return 1.0f;
    }
    var cascade_2 : u32;
    if(distance_0 <= (camera_0.shadow_splits_0.x))
    {
        cascade_2 = u32(0);
    }
    else
    {
        if(distance_0 <= (camera_0.shadow_splits_0.y))
        {
            cascade_2 = u32(1);
        }
        else
        {
            if(distance_0 <= (camera_0.shadow_splits_0.z))
            {
                cascade_2 = u32(2);
            }
            else
            {
                cascade_2 = u32(3);
            }
        }
    }
    var visible_2 : f32 = cascade_visibility_0(cascade_2, position_3, geometric_normal_2, light_1);
    var start_0 : f32;
    if(cascade_2 == u32(0))
    {
        start_0 = camera_0.eye_0.w;
    }
    else
    {
        start_0 = camera_0.shadow_splits_0[max(cascade_2, u32(1)) - u32(1)];
    }
    var _S99 : f32 = max((camera_0.shadow_splits_0[cascade_2] - start_0) * clamp(camera_0.camera_forward_blend_0.w, 0.0f, 1.0f), 9.99999997475242708e-07f);
    var weight_3 : f32 = clamp((distance_0 - (camera_0.shadow_splits_0[cascade_2] - _S99)) / _S99, 0.0f, 1.0f);
    var visible_3 : f32;
    if(weight_3 > 0.0f)
    {
        if(cascade_2 < u32(3))
        {
            visible_3 = cascade_visibility_0(cascade_2 + u32(1), position_3, geometric_normal_2, light_1);
        }
        else
        {
            visible_3 = 1.0f;
        }
        visible_3 = mix(visible_2, visible_3, weight_3);
    }
    else
    {
        visible_3 = visible_2;
    }
    return visible_3;
}

fn evaluate_lighting_0( position_4 : vec3<f32>,  surface_1 : Surface_0,  brdf_inputs_0 : BrdfInputs_0) -> vec3<f32>
{
    var _S100 : u32 = baked_debug_0();
    var irradiance_0 : vec3<f32>;
    if(_S100 > u32(0))
    {
        if((surface_1.occlusion_0) == 0.0f)
        {
            return surface_1.emissive_0;
        }
        var value_4 : vec3<f32> = probe_irradiance_0(position_4, surface_1.normal_0, baked_blend_0());
        if(_S100 == u32(1))
        {
            irradiance_0 = value_4 / vec3<f32>(3.14159274101257324f);
        }
        else
        {
            irradiance_0 = value_4;
        }
        return irradiance_0;
    }
    var l_0 : vec3<f32> = unit_or_0(camera_0.light_direction_0.xyz, surface_1.normal_0);
    var h_0 : vec3<f32> = unit_or_0(brdf_inputs_0.view_0 + l_0, surface_1.normal_0);
    var _S101 : f32 = max(dot(surface_1.normal_0, l_0), 0.0f);
    var _S102 : f32 = max(dot(surface_1.normal_0, brdf_inputs_0.view_0), 0.0f);
    var direct_brdf_0 : vec3<f32> = evaluate_brdf_0(_S101, _S102, max(dot(surface_1.normal_0, h_0), 0.0f), max(dot(brdf_inputs_0.view_0, h_0), 0.0f), brdf_inputs_0.f0_0, brdf_inputs_0.diffuse_color_0, surface_1.roughness_0);
    var _S103 : f32 = max(camera_0.environment_0.x, 0.0f);
    var _S104 : vec3<f32> = vec3<f32>(0.0f);
    var _S105 : bool;
    if(_S103 > 0.0f)
    {
        _S105 = true;
    }
    else
    {
        _S105 = (camera_0.shadow_bias_0.w) >= 1.0f;
    }
    if(_S105)
    {
        _S105 = true;
    }
    else
    {
        _S105 = (abs(camera_0.environment_0.w)) >= 2.0f;
    }
    var indirect_0 : vec3<f32>;
    var shadow_visibility_0 : f32;
    if(_S105)
    {
        if((camera_0.shadow_bias_0.w) >= 1.0f)
        {
            if((surface_1.occlusion_0) > 0.0f)
            {
                var _S106 : vec3<f32> = probe_irradiance_0(position_4, surface_1.normal_0, baked_blend_0());
                irradiance_0 = _S106;
            }
            else
            {
                irradiance_0 = _S104;
            }
        }
        else
        {
            irradiance_0 = (textureSampleLevel((irradiance_image_0), (environment_sampler_0), (environment_uv_0(surface_1.normal_0)), (0.0f))).xyz;
        }
        var reflected_1 : vec3<f32> = reflect((vec3<f32>(0) - brdf_inputs_0.view_0), surface_1.normal_0);
        var prefiltered_0 : vec3<f32> = (textureSampleLevel((reflection_image_0), (environment_sampler_0), (environment_uv_0(reflected_1)), (surface_1.roughness_0 * max(camera_0.environment_0.z, 0.0f)))).xyz;
        var lut_size_0 : vec2<f32> = texture_size_0(brdf_image_0);
        var _S107 : vec2<f32> = vec2<f32>(0.5f);
        var brdf_0 : vec2<f32> = (textureSampleLevel((brdf_image_0), (environment_sampler_0), (clamp(vec2<f32>(_S102, surface_1.roughness_0), _S107 / lut_size_0, vec2<f32>(1.0f) - _S107 / lut_size_0)), (0.0f))).xy;
        var indirect_diffuse_0 : vec3<f32> = irradiance_0 * brdf_inputs_0.diffuse_color_0 / vec3<f32>(3.14159274101257324f) * vec3<f32>(surface_1.occlusion_0);
        var split_sum_0 : vec3<f32> = brdf_inputs_0.f0_0 * vec3<f32>(brdf_0.x) + vec3<f32>(brdf_0.y);
        var indirect_specular_0 : vec3<f32> = prefiltered_0 * split_sum_0;
        if((camera_0.shadow_bias_0.w) >= 1.0f)
        {
            irradiance_0 = indirect_diffuse_0 + indirect_specular_0 * vec3<f32>(_S103);
        }
        else
        {
            irradiance_0 = (indirect_diffuse_0 + indirect_specular_0) * vec3<f32>(_S103);
        }
        var probe_parameter_0 : f32 = abs(camera_0.environment_0.w);
        if(probe_parameter_0 >= 2.0f)
        {
            var weights_3 : vec3<f32>;
            var radiance_2 : vec3<f32> = local_specular_0(position_4, reflected_1, surface_1.roughness_0, prefiltered_0 * vec3<f32>(_S103), probe_parameter_0, &(weights_3));
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
                    shadow_visibility_0 = _S103;
                }
                indirect_0 = indirect_diffuse_0 * vec3<f32>(shadow_visibility_0) + radiance_2 * split_sum_0;
            }
            else
            {
                indirect_0 = irradiance_0;
            }
        }
        else
        {
            indirect_0 = irradiance_0;
        }
    }
    else
    {
        indirect_0 = _S104;
    }
    if(_S101 != 0.0f)
    {
        shadow_visibility_0 = sun_visibility_0(position_4, surface_1.geometric_normal_0, l_0);
    }
    else
    {
        shadow_visibility_0 = 1.0f;
    }
    return direct_brdf_0 * camera_0.light_color_0.xyz * vec3<f32>(_S101) * vec3<f32>(shadow_visibility_0) + indirect_0 + surface_1.emissive_0;
}

struct pixelOutput_0
{
    @location(0) output_0 : vec4<f32>,
};

@fragment
fn main(@builtin(position) pixel_0 : vec4<f32>) -> pixelOutput_0
{
    var uv_4 : vec2<f32> = pixel_0.xy / texture_size_0(depth_image_0);
    var depth_2 : f32 = (textureSampleLevel((depth_image_0), (gbuffer_sampler_0), (uv_4), (0.0f))).x;
    if(!(depth_2 < 1.0f))
    {
        discard;
    }
    var ndc_1 : vec2<f32> = uv_4 * vec2<f32>(2.0f) - vec2<f32>(1.0f);
    var x_6 : vec4<f32> = Reconstruction_0.inverse_view_projection_0.data_0[i32(0)] * vec4<f32>(ndc_1.x);
    var y_1 : vec4<f32> = Reconstruction_0.inverse_view_projection_0.data_0[i32(1)] * vec4<f32>(ndc_1.y);
    var z_0 : vec4<f32> = Reconstruction_0.inverse_view_projection_0.data_0[i32(2)] * vec4<f32>(depth_2);
    var xy_0 : vec4<f32> = x_6 + y_1;
    var xyz_0 : vec4<f32> = xy_0 + z_0;
    var world_1 : vec4<f32> = xyz_0 + Reconstruction_0.inverse_view_projection_0.data_0[i32(3)];
    var _S108 : f32 = world_1.w;
    if(!((abs(_S108)) > 9.99999968265522539e-21f))
    {
        discard;
    }
    var position_5 : vec3<f32> = world_1.xyz / vec3<f32>(_S108);
    var base_0 : vec4<f32> = (textureSampleLevel((base_metallic_image_0), (gbuffer_sampler_0), (uv_4), (0.0f)));
    var normal_1 : vec4<f32> = (textureSampleLevel((normal_roughness_image_0), (gbuffer_sampler_0), (uv_4), (0.0f)));
    var emitted_0 : vec4<f32> = (textureSampleLevel((emissive_occlusion_image_0), (gbuffer_sampler_0), (uv_4), (0.0f)));
    var surface_2 : Surface_0;
    surface_2.base_color_0 = base_0.xyz;
    surface_2.normal_0 = oct_decode_0(normal_1.xy);
    surface_2.geometric_normal_0 = unpack_geometric_normal_0(normal_1.w);
    surface_2.emissive_0 = emitted_0.xyz;
    surface_2.metallic_0 = clamp(base_0.w, 0.0f, 1.0f);
    surface_2.roughness_0 = clamp(normal_1.z, 0.0f, 1.0f);
    surface_2.occlusion_0 = clamp(emitted_0.w, 0.0f, 1.0f);
    var color_0 : vec3<f32> = evaluate_lighting_0(position_5, surface_2, make_brdf_inputs_0(position_5, surface_2));
    var _S109 : pixelOutput_0 = pixelOutput_0( vec4<f32>(clamp(color_0, vec3<f32>(0.0f), vec3<f32>(65504.0f)), 1.0f) );
    return _S109;
}
