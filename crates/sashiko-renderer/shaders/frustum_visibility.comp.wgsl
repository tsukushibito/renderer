struct Bounds_std430_0
{
    @align(16) minimum_enabled_0 : vec4<u32>,
    @align(16) maximum_padded_0 : vec4<u32>,
};

@binding(1) @group(0) var<storage, read> world_0 : array<Bounds_std430_0>;

struct _MatrixStorage_float4x4std140_0
{
    @align(16) data_0 : array<vec4<f32>, i32(4)>,
};

struct SLANG_ParameterGroup_Camera_std140_0
{
    @align(16) view_projection_0 : _MatrixStorage_float4x4std140_0,
};

@binding(0) @group(0) var<uniform> Camera_0 : SLANG_ParameterGroup_Camera_std140_0;
@binding(2) @group(0) var<storage, read_write> visibility_0 : array<u32>;

fn finite_0( value_0 : vec3<f32>) -> bool
{
    var _S1 : vec3<u32> = vec3<u32>(u32(2139095040));
    return (all(((((bitcast<vec3<u32>>((value_0))) & (_S1))) < _S1)));
}

fn finite_1( value_1 : vec4<f32>) -> bool
{
    var _S2 : vec4<u32> = vec4<u32>(u32(2139095040));
    return (all(((((bitcast<vec4<u32>>((value_1))) & (_S2))) < _S2)));
}

fn isinf_0( x_0 : f32) -> bool
{
    var _S3 : u32 = (bitcast<u32>((x_0)));
    var _S4 : u32 = (_S3 & (u32(8388607)));
    var _S5 : bool;
    if(((((_S3 >> (u32(23)))) & (u32(255)))) == u32(255))
    {
        _S5 = _S4 == u32(0);
    }
    else
    {
        _S5 = false;
    }
    return _S5;
}

fn outside_0( plane_0 : vec4<f32>,  allowance_0 : f32,  minimum_0 : vec3<f32>,  maximum_0 : vec3<f32>) -> bool
{
    var terms_0 : vec4<f32> = plane_0 * vec4<f32>(select(minimum_0, maximum_0, (plane_0.xyz) >= vec3<f32>(0.0f)), 1.0f);
    var partial_0 : f32 = terms_0.x + terms_0.y;
    var top_0 : f32 = partial_0 + terms_0.z;
    var score_0 : f32 = top_0 + terms_0.w;
    var _S6 : bool;
    if(!isinf_0(score_0))
    {
        _S6 = score_0 < (- allowance_0);
    }
    else
    {
        _S6 = false;
    }
    return _S6;
}

fn overlaps_0( minimum_1 : vec3<f32>,  maximum_1 : vec3<f32>) -> bool
{
    var _S7 : vec4<f32> = Camera_0.view_projection_0.data_0[i32(0)];
    var _S8 : vec4<f32> = Camera_0.view_projection_0.data_0[i32(1)];
    var _S9 : vec4<f32> = Camera_0.view_projection_0.data_0[i32(2)];
    var _S10 : vec4<f32> = Camera_0.view_projection_0.data_0[i32(3)];
    var _S11 : bool;
    if(!finite_0(minimum_1))
    {
        _S11 = true;
    }
    else
    {
        _S11 = !finite_0(maximum_1);
    }
    if(_S11)
    {
        _S11 = true;
    }
    else
    {
        _S11 = (any((minimum_1 > maximum_1)));
    }
    if(_S11)
    {
        _S11 = true;
    }
    else
    {
        _S11 = !finite_1(_S7);
    }
    if(_S11)
    {
        _S11 = true;
    }
    else
    {
        _S11 = !finite_1(_S8);
    }
    if(_S11)
    {
        _S11 = true;
    }
    else
    {
        _S11 = !finite_1(_S9);
    }
    if(_S11)
    {
        _S11 = true;
    }
    else
    {
        _S11 = !finite_1(_S10);
    }
    if(_S11)
    {
        return true;
    }
    var position_limit_0 : vec3<f32> = max(abs(minimum_1), abs(maximum_1));
    var abs0_0 : vec4<f32> = abs(_S7);
    var abs1_0 : vec4<f32> = abs(_S8);
    var abs2_0 : vec4<f32> = abs(_S9);
    var abs3_0 : vec4<f32> = abs(_S10);
    var coefficient_bound_0 : vec4<f32> = abs0_0 + abs1_0 + abs2_0 + abs3_0;
    var clip_bound_0 : vec4<f32> = abs0_0 * vec4<f32>(position_limit_0.x) + abs1_0 * vec4<f32>(position_limit_0.y) + abs2_0 * vec4<f32>(position_limit_0.z) + abs3_0;
    var _S12 : vec4<f32> = vec4<f32>(4.25352933298161075e+37f);
    if(!(all((coefficient_bound_0 < _S12))))
    {
        _S11 = true;
    }
    else
    {
        _S11 = !(all((clip_bound_0 < _S12)));
    }
    if(_S11)
    {
        return true;
    }
    var lost_coordinates_0 : vec3<f32> = vec3<f32>(1.88079096131566001e-37f) * position_limit_0;
    var lost_position_0 : f32 = lost_coordinates_0.x + lost_coordinates_0.y + lost_coordinates_0.z;
    var roundoff_0 : vec4<f32> = vec4<f32>(0.0000152587890625f) * max(clip_bound_0, vec4<f32>(1.0f));
    var underflow_0 : vec4<f32> = vec4<f32>(1.88079096131566001e-37f) * coefficient_bound_0 + vec4<f32>(lost_position_0);
    var error_0 : vec4<f32> = roundoff_0 + underflow_0;
    var row_x_0 : vec4<f32> = vec4<f32>(_S7.x, _S8.x, _S9.x, _S10.x);
    var row_y_0 : vec4<f32> = vec4<f32>(_S7.y, _S8.y, _S9.y, _S10.y);
    var row_z_0 : vec4<f32> = vec4<f32>(_S7.z, _S8.z, _S9.z, _S10.z);
    var row_w_0 : vec4<f32> = vec4<f32>(_S7.w, _S8.w, _S9.w, _S10.w);
    var _S13 : f32 = error_0.w;
    var _S14 : f32 = _S13 + error_0.x;
    if(outside_0(row_w_0 + row_x_0, _S14, minimum_1, maximum_1))
    {
        _S11 = true;
    }
    else
    {
        _S11 = outside_0(row_w_0 - row_x_0, _S14, minimum_1, maximum_1);
    }
    if(_S11)
    {
        _S11 = true;
    }
    else
    {
        _S11 = outside_0(row_w_0 + row_y_0, _S13 + error_0.y, minimum_1, maximum_1);
    }
    if(_S11)
    {
        _S11 = true;
    }
    else
    {
        _S11 = outside_0(row_w_0 - row_y_0, _S13 + error_0.y, minimum_1, maximum_1);
    }
    if(_S11)
    {
        _S11 = true;
    }
    else
    {
        _S11 = outside_0(row_z_0, error_0.z, minimum_1, maximum_1);
    }
    if(_S11)
    {
        _S11 = true;
    }
    else
    {
        _S11 = outside_0(row_w_0 - row_z_0, _S13 + error_0.z, minimum_1, maximum_1);
    }
    return !_S11;
}

@compute
@workgroup_size(64, 1, 1)
fn main(@builtin(global_invocation_id) dispatch_id_0 : vec3<u32>)
{
    var slot_0 : u32 = dispatch_id_0.x;
    var _S15 : Bounds_std430_0 = world_0[slot_0];
    var mask_0 : u32;
    if((_S15.minimum_enabled_0.w) != u32(0))
    {
        mask_0 = u32(overlaps_0((bitcast<vec3<f32>>((_S15.minimum_enabled_0.xyz))), (bitcast<vec3<f32>>((world_0[slot_0].maximum_padded_0.xyz)))));
    }
    else
    {
        mask_0 = u32(0);
    }
    visibility_0[slot_0] = mask_0;
    return;
}
