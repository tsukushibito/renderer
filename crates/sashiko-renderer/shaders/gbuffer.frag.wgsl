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

fn oct_wrap_0( components_0 : vec2<f32>) -> vec2<f32>
{
    var _S37 : f32;
    if((components_0.x) >= 0.0f)
    {
        _S37 = 1.0f;
    }
    else
    {
        _S37 = -1.0f;
    }
    var _S38 : f32;
    if((components_0.y) >= 0.0f)
    {
        _S38 = 1.0f;
    }
    else
    {
        _S38 = -1.0f;
    }
    return (vec2<f32>(1.0f) - abs(components_0.yx)) * vec2<f32>(_S37, _S38);
}

fn oct_encode_0( direction_0 : vec3<f32>) -> vec2<f32>
{
    var _S39 : f32 = direction_0.z;
    var oct_0 : vec2<f32> = direction_0.xy / vec2<f32>(max(abs(direction_0.x) + abs(direction_0.y) + abs(_S39), 9.99999968265522539e-21f));
    var _S40 : vec2<f32>;
    if(_S39 >= 0.0f)
    {
        _S40 = oct_0;
    }
    else
    {
        _S40 = oct_wrap_0(oct_0);
    }
    return _S40;
}

fn isnan_1( x_2 : vec2<f32>) -> vec2<bool>
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
        result_1[i_1] = isnan_0(x_2[i_1]);
        i_1 = i_1 + i32(1);
    }
    return result_1;
}

fn pack_geometric_normal_0( direction_1 : vec3<f32>) -> f32
{
    var _S41 : vec2<f32> = vec2<f32>(0.5f);
    var encoded_0 : vec2<f32> = oct_encode_0(direction_1) * _S41 + _S41;
    var encoded_1 : vec2<f32>;
    if((any((isnan_1(encoded_0)))))
    {
        encoded_1 = vec2<f32>(0.5f);
    }
    else
    {
        encoded_1 = encoded_0;
    }
    var _S42 : vec2<u32> = vec2<u32>(round(clamp(encoded_1, vec2<f32>(0.0f), vec2<f32>(1.0f)) * vec2<f32>(127.0f)));
    return (unpack2x16float((u32(1024) + _S42.x + (((_S42.y) << (u32(7)))))).x);
}

struct GBuffer_0
{
    @location(0) base_metallic_0 : vec4<f32>,
    @location(1) normal_roughness_0 : vec4<f32>,
    @location(2) emissive_occlusion_0 : vec4<f32>,
};

struct pixelInput_0
{
    @location(1) world_normal_1 : vec3<f32>,
    @location(2) world_tangent_1 : vec4<f32>,
    @location(3) texcoord_1 : vec2<f32>,
    @location(5) lightmap_uv_0 : vec2<f32>,
    @interpolate(flat) @location(6) lightmap_slot_0 : u32,
};

@fragment
fn main( _S43 : pixelInput_0, @builtin(front_facing) front_facing_1 : bool) -> GBuffer_0
{
    var _S44 : MaterialSample_0 = sample_material_0(_S43.world_normal_1, _S43.world_tangent_1, _S43.texcoord_1, front_facing_1);
    var surface_1 : Surface_0 = _S44.surface_0;
    apply_lightmap_0(&(surface_1), _S43.lightmap_slot_0, front_facing_1, _S43.lightmap_uv_0);
    var result_2 : GBuffer_0;
    result_2.base_metallic_0 = vec4<f32>(surface_1.base_color_0, surface_1.metallic_0);
    result_2.normal_roughness_0 = vec4<f32>(oct_encode_0(surface_1.normal_0), surface_1.roughness_0, pack_geometric_normal_0(surface_1.geometric_normal_0));
    result_2.emissive_occlusion_0 = vec4<f32>(min(surface_1.emissive_0, vec3<f32>(65504.0f)), surface_1.occlusion_0);
    return result_2;
}
