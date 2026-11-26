@binding(5) @group(0) var shadow_table_0 : texture_2d<f32>;

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

fn shadow_texel_0( at_0 : u32,  width_0 : u32) -> vec4<f32>
{
    var _S1 : u32 = at_0 % width_0;
    var _S2 : i32 = i32(_S1);
    var _S3 : u32 = at_0 / width_0;
    var _S4 : vec3<i32> = vec3<i32>(_S2, i32(_S3), i32(0));
    return (textureLoad((shadow_table_0), ((_S4)).xy, ((_S4)).z));
}

fn isnan_0( x_0 : f32) -> bool
{
    var _S5 : u32 = (bitcast<u32>((x_0)));
    var _S6 : u32 = (_S5 & (u32(8388607)));
    var _S7 : bool;
    if(((((_S5 >> (u32(23)))) & (u32(255)))) == u32(255))
    {
        _S7 = _S6 != u32(0);
    }
    else
    {
        _S7 = false;
    }
    return _S7;
}

fn rsqrt_0( x_1 : f32) -> f32
{
    return 1.0f / sqrt(x_1);
}

fn unit_or_0( value_0 : vec3<f32>,  fallback_0 : vec3<f32>) -> vec3<f32>
{
    var squared_0 : f32 = dot(value_0, value_0);
    var _S8 : vec3<f32>;
    if(squared_0 > 9.99999968265522539e-21f)
    {
        _S8 = value_0 * vec3<f32>(rsqrt_0(squared_0));
    }
    else
    {
        _S8 = fallback_0;
    }
    return _S8;
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

fn sample_material_0( world_normal_0 : vec3<f32>,  world_tangent_0 : vec4<f32>,  texcoord_0 : vec2<f32>,  front_facing_0 : bool,  pixel_scale_0 : vec2<f32>) -> MaterialSample_0
{
    var dx_0 : vec2<f32> = dpdx(texcoord_0) * vec2<f32>(pixel_scale_0.x);
    var dy_0 : vec2<f32> = dpdy(texcoord_0) * vec2<f32>(pixel_scale_0.y);
    var _S9 : bool;
    if(!front_facing_0)
    {
        _S9 = (material_0.flags_0.y) == u32(0);
    }
    else
    {
        _S9 = false;
    }
    if(_S9)
    {
        discard;
    }
    var alpha_0 : f32 = clamp((material_0.base_factor_0 * (textureSampleGrad((base_image_0), (material_sampler_0), (texcoord_0), (dx_0), (dy_0)))).w, 0.0f, 1.0f);
    var alpha_mode_0 : u32 = material_0.flags_0.x;
    if(alpha_mode_0 == u32(1))
    {
        _S9 = alpha_0 < (material_0.metallic_roughness_occlusion_cutoff_0.w);
    }
    else
    {
        _S9 = false;
    }
    if(_S9)
    {
        _S9 = true;
    }
    else
    {
        if(alpha_mode_0 == u32(2))
        {
            _S9 = alpha_0 <= 0.0f;
        }
        else
        {
            _S9 = false;
        }
    }
    if(_S9)
    {
        discard;
    }
    var _S10 : vec3<f32> = vec3<f32>(0.0f);
    var _S11 : Surface_0 = Surface_0( _S10, _S10, _S10, _S10, 0.0f, 0.0f, 0.0f );
    var result_0 : MaterialSample_0;
    result_0.surface_0 = _S11;
    result_0.output_alpha_0 = 0.0f;
    var geometric_0 : vec3<f32> = unit_or_0(world_normal_0, vec3<f32>(0.0f, 0.0f, 1.0f));
    var _S12 : vec3<f32>;
    if(front_facing_0)
    {
        _S12 = geometric_0;
    }
    else
    {
        _S12 = (vec3<f32>(0) - geometric_0);
    }
    result_0.surface_0.geometric_normal_0 = _S12;
    return result_0;
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
    var _S13 : vec4<f32> = shadow_texel_0(u32(0), width_1);
    var _S14 : vec2<u32> = vec2<u32>(clamp(pixel_0 * _S13.xy, vec2<f32>(0.0f), vec2<f32>(15.0f)));
    var _S15 : vec4<f32> = shadow_texel_0(u32(1) + _S14.y * u32(16) + _S14.x, width_1);
    var range_0 : vec2<f32> = _S15.xy;
    var _S16 : u32 = texels_0 * u32(4);
    var _S17 : u32 = min(u32(range_0.x), _S16);
    var _S18 : u32 = min(u32(range_0.y), _S16 - _S17);
    var occlusion_1 : f32 = 0.0f;
    var j_0 : u32 = u32(0);
    for(;;)
    {
        if(j_0 < _S18)
        {
        }
        else
        {
            break;
        }
        var word_0 : u32 = _S17 + j_0;
        var _S19 : vec4<f32> = shadow_texel_0(word_0 / u32(4), width_1);
        var _S20 : u32 = min(u32(_S19[word_0 % u32(4)]), texels_0 - u32(3));
        var a_0 : vec4<f32> = shadow_texel_0(_S20, width_1);
        var _S21 : vec4<f32> = shadow_texel_0(_S20 + u32(1), width_1);
        var _S22 : vec3<f32> = a_0.xyz;
        var segment_0 : vec3<f32> = _S21.xyz - _S22;
        var lighting_0 : vec4<f32> = shadow_texel_0(_S20 + u32(2), width_1);
        var delta_0 : vec3<f32> = _S22 + segment_0 * vec3<f32>(saturate(dot(position_0 - _S22, segment_0) / max(dot(segment_0, segment_0), 9.99999968265522539e-21f))) - position_0;
        var distance_0 : f32 = length(delta_0);
        var _S23 : f32 = a_0.w;
        var _S24 : bool;
        if(distance_0 >= (6.0f * _S23))
        {
            _S24 = true;
        }
        else
        {
            _S24 = (dot(delta_0, normal_1)) <= (- _S23);
        }
        if(_S24)
        {
            j_0 = j_0 + u32(1);
            continue;
        }
        var _S25 : f32 = max(distance_0, 1.00000001335143196e-10f);
        var axis_0 : vec3<f32> = delta_0 / vec3<f32>(_S25);
        var ratio_0 : f32 = saturate(_S23 / _S25);
        var angle_0 : f32 = asin(ratio_0);
        occlusion_1 = max(occlusion_1, (1.0f - smoothstep(4.0f, 6.0f, distance_0 / _S23)) * mix(ratio_0 * ratio_0 * saturate(dot(axis_0, normal_1)), smoothstep(0.0f, 1.0f, (angle_0 + 0.64999997615814209f - acos(clamp(dot(axis_0, lighting_0.xyz), -1.0f, 1.0f))) / (2.0f * min(angle_0, 0.64999997615814209f))) * saturate(angle_0 * angle_0 / 0.42249995470046997f), lighting_0.w));
        j_0 = j_0 + u32(1);
    }
    return 1.0f - occlusion_1;
}

fn oct_wrap_0( components_0 : vec2<f32>) -> vec2<f32>
{
    var _S26 : f32;
    if((components_0.x) >= 0.0f)
    {
        _S26 = 1.0f;
    }
    else
    {
        _S26 = -1.0f;
    }
    var _S27 : f32;
    if((components_0.y) >= 0.0f)
    {
        _S27 = 1.0f;
    }
    else
    {
        _S27 = -1.0f;
    }
    return (vec2<f32>(1.0f) - abs(components_0.yx)) * vec2<f32>(_S26, _S27);
}

fn oct_encode_0( direction_0 : vec3<f32>) -> vec2<f32>
{
    var _S28 : f32 = direction_0.z;
    var oct_0 : vec2<f32> = direction_0.xy / vec2<f32>(max(abs(direction_0.x) + abs(direction_0.y) + abs(_S28), 9.99999968265522539e-21f));
    var _S29 : vec2<f32>;
    if(_S28 >= 0.0f)
    {
        _S29 = oct_0;
    }
    else
    {
        _S29 = oct_wrap_0(oct_0);
    }
    return _S29;
}

fn isnan_1( x_2 : vec2<f32>) -> vec2<bool>
{
    var result_1 : vec2<bool>;
    var i_0 : i32 = i32(0);
    for(;;)
    {
        if(i_0 < i32(2))
        {
        }
        else
        {
            break;
        }
        result_1[i_0] = isnan_0(x_2[i_0]);
        i_0 = i_0 + i32(1);
    }
    return result_1;
}

fn pack_geometric_normal_0( direction_1 : vec3<f32>) -> f32
{
    var _S30 : vec2<f32> = vec2<f32>(0.5f);
    var encoded_0 : vec2<f32> = oct_encode_0(direction_1) * _S30 + _S30;
    var encoded_1 : vec2<f32>;
    if((any((isnan_1(encoded_0)))))
    {
        encoded_1 = vec2<f32>(0.5f);
    }
    else
    {
        encoded_1 = encoded_0;
    }
    var _S31 : vec2<u32> = vec2<u32>(round(clamp(encoded_1, vec2<f32>(0.0f), vec2<f32>(1.0f)) * vec2<f32>(127.0f)));
    return (unpack2x16float((u32(1024) + _S31.x + (((_S31.y) << (u32(7)))))).x);
}

fn half_receiver_0( position_1 : vec3<f32>,  normal_2 : vec3<f32>,  pixel_1 : vec3<f32>) -> vec4<f32>
{
    var _S32 : vec4<f32> = shadow_texel_0(u32(0), u32(1));
    var full_extent_0 : vec2<f32> = _S32.zw;
    var scaled_depth_0 : f32 = pixel_1.z * 1024.0f;
    var _S33 : f32 = indirect_visibility_0(position_1, normal_2, pixel_1.xy * full_extent_0 / ceil(full_extent_0 * vec2<f32>(0.5f)));
    return vec4<f32>(_S33, floor(scaled_depth_0), fract(scaled_depth_0), pack_geometric_normal_0(normal_2));
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
fn main( _S34 : pixelInput_0, @builtin(position) pixel_2 : vec4<f32>, @builtin(front_facing) front_facing_1 : bool) -> pixelOutput_0
{
    var _S35 : vec4<f32> = shadow_texel_0(u32(0), u32(1));
    var _S36 : vec2<f32> = ceil(_S35.zw * vec2<f32>(0.5f));
    var _S37 : vec4<f32> = shadow_texel_0(u32(0), u32(1));
    var sampled_0 : MaterialSample_0 = sample_material_0(_S34.world_normal_1, _S34.world_tangent_1, _S34.texcoord_1, front_facing_1, _S36 / _S37.zw);
    if((_S34.lightmap_slot_0) == u32(0))
    {
        discard;
    }
    var _S38 : vec4<f32> = half_receiver_0(_S34.world_position_0, sampled_0.surface_0.geometric_normal_0, pixel_2.xyz);
    var _S39 : pixelOutput_0 = pixelOutput_0( _S38 );
    return _S39;
}
