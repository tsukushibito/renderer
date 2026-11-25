struct pixelOutput_0
{
    @location(0) output_0 : vec4<f32>,
};

struct pixelInput_0
{
    @location(0) shaded_0 : vec4<f32>,
};

@fragment
fn main( _S1 : pixelInput_0) -> pixelOutput_0
{
    var _S2 : vec3<f32> = vec3<f32>(3.0f);
    var _S3 : pixelOutput_0 = pixelOutput_0( vec4<f32>(floor(saturate(_S1.shaded_0.xyz) * _S2 + vec3<f32>(0.5f)) / _S2, _S1.shaded_0.w) );
    return _S3;
}
