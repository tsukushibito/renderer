#version 450 core
// Adapted from Godot 4.7.2-stable FXAA 3.11 compact (Simon Rodriguez/NVIDIA).
// Source: servers/rendering/renderer_rd/shaders/effects/tonemap.glsl
// Changes: single-view standalone pass after Neutral; no exposure multiplier;
// positive division/sqrt guards; separate sampler; one final sRGB encode.
// Full Godot/MIT and original FXAA notices are retained in licenses/.
layout(set = 0, binding = 0) uniform texture2D linear_image;
layout(set = 0, binding = 1) uniform sampler source_sampler;
layout(location = 0) out vec4 output_color;
float QUALITY(float q) {
	return (q < 5 ? 1.0 : (q > 5 ? (q < 10 ? 2.0 : (q < 11 ? 4.0 : 8.0)) : 1.5));
}

float rgb2luma(vec3 rgb) {
	return sqrt(max(dot(rgb, vec3(0.299, 0.587, 0.114)), 0.0));
}

vec3 do_fxaa(vec3 color, vec2 uv_interp) {
	vec2 pixel_size = 1.0 / vec2(textureSize(sampler2D(linear_image, source_sampler), 0));
	const float EDGE_THRESHOLD_MIN = 0.0312;
	const float EDGE_THRESHOLD_MAX = 0.125;
	const int ITERATIONS = 12;
	const float SUBPIXEL_QUALITY = 0.75;


	float lumaUp = rgb2luma(textureLodOffset(sampler2D(linear_image, source_sampler), uv_interp, 0.0, ivec2(0, 1)).xyz);
	float lumaDown = rgb2luma(textureLodOffset(sampler2D(linear_image, source_sampler), uv_interp, 0.0, ivec2(0, -1)).xyz);
	float lumaLeft = rgb2luma(textureLodOffset(sampler2D(linear_image, source_sampler), uv_interp, 0.0, ivec2(-1, 0)).xyz);
	float lumaRight = rgb2luma(textureLodOffset(sampler2D(linear_image, source_sampler), uv_interp, 0.0, ivec2(1, 0)).xyz);

	float lumaCenter = rgb2luma(color);

	float lumaMin = min(lumaCenter, min(min(lumaUp, lumaDown), min(lumaLeft, lumaRight)));
	float lumaMax = max(lumaCenter, max(max(lumaUp, lumaDown), max(lumaLeft, lumaRight)));

	float lumaRange = lumaMax - lumaMin;

	if (lumaRange < max(EDGE_THRESHOLD_MIN, lumaMax * EDGE_THRESHOLD_MAX)) {
		return color;
	}

	float lumaDownLeft = rgb2luma(textureLodOffset(sampler2D(linear_image, source_sampler), uv_interp, 0.0, ivec2(-1, -1)).xyz);
	float lumaUpRight = rgb2luma(textureLodOffset(sampler2D(linear_image, source_sampler), uv_interp, 0.0, ivec2(1, 1)).xyz);
	float lumaUpLeft = rgb2luma(textureLodOffset(sampler2D(linear_image, source_sampler), uv_interp, 0.0, ivec2(-1, 1)).xyz);
	float lumaDownRight = rgb2luma(textureLodOffset(sampler2D(linear_image, source_sampler), uv_interp, 0.0, ivec2(1, -1)).xyz);

	float lumaDownUp = lumaDown + lumaUp;
	float lumaLeftRight = lumaLeft + lumaRight;

	float lumaLeftCorners = lumaDownLeft + lumaUpLeft;
	float lumaDownCorners = lumaDownLeft + lumaDownRight;
	float lumaRightCorners = lumaDownRight + lumaUpRight;
	float lumaUpCorners = lumaUpRight + lumaUpLeft;

	float edgeHorizontal = abs(-2.0 * lumaLeft + lumaLeftCorners) + abs(-2.0 * lumaCenter + lumaDownUp) * 2.0 + abs(-2.0 * lumaRight + lumaRightCorners);
	float edgeVertical = abs(-2.0 * lumaUp + lumaUpCorners) + abs(-2.0 * lumaCenter + lumaLeftRight) * 2.0 + abs(-2.0 * lumaDown + lumaDownCorners);

	bool isHorizontal = (edgeHorizontal >= edgeVertical);

	float stepLength = isHorizontal ? pixel_size.y : pixel_size.x;

	float luma1 = isHorizontal ? lumaDown : lumaLeft;
	float luma2 = isHorizontal ? lumaUp : lumaRight;
	float gradient1 = luma1 - lumaCenter;
	float gradient2 = luma2 - lumaCenter;

	bool is1Steepest = abs(gradient1) >= abs(gradient2);

	float gradientScaled = 0.25 * max(abs(gradient1), abs(gradient2));

	float lumaLocalAverage = 0.0;
	if (is1Steepest) {
		stepLength = -stepLength;
		lumaLocalAverage = 0.5 * (luma1 + lumaCenter);
	} else {
		lumaLocalAverage = 0.5 * (luma2 + lumaCenter);
	}

	vec2 currentUv = uv_interp;
	if (isHorizontal) {
		currentUv.y += stepLength * 0.5;
	} else {
		currentUv.x += stepLength * 0.5;
	}

	vec2 offset = isHorizontal ? vec2(pixel_size.x, 0.0) : vec2(0.0, pixel_size.y);
	vec2 uv1 = currentUv - offset * QUALITY(0);
	vec2 uv2 = currentUv + offset * QUALITY(0);

	float lumaEnd1 = rgb2luma(textureLod(sampler2D(linear_image, source_sampler), uv1, 0.0).xyz);
	float lumaEnd2 = rgb2luma(textureLod(sampler2D(linear_image, source_sampler), uv2, 0.0).xyz);
	lumaEnd1 -= lumaLocalAverage;
	lumaEnd2 -= lumaLocalAverage;

	bool reached1 = abs(lumaEnd1) >= gradientScaled;
	bool reached2 = abs(lumaEnd2) >= gradientScaled;
	bool reachedBoth = reached1 && reached2;

	if (!reached1) {
		uv1 -= offset * QUALITY(1);
	}
	if (!reached2) {
		uv2 += offset * QUALITY(1);
	}

	if (!reachedBoth) {
		for (int i = 2; i < ITERATIONS; i++) {
			if (!reached1) {
				lumaEnd1 = rgb2luma(textureLod(sampler2D(linear_image, source_sampler), uv1, 0.0).xyz);
				lumaEnd1 = lumaEnd1 - lumaLocalAverage;
			}
			if (!reached2) {
				lumaEnd2 = rgb2luma(textureLod(sampler2D(linear_image, source_sampler), uv2, 0.0).xyz);
				lumaEnd2 = lumaEnd2 - lumaLocalAverage;
			}
			reached1 = abs(lumaEnd1) >= gradientScaled;
			reached2 = abs(lumaEnd2) >= gradientScaled;
			reachedBoth = reached1 && reached2;
			if (!reached1) {
				uv1 -= offset * QUALITY(i);
			}
			if (!reached2) {
				uv2 += offset * QUALITY(i);
			}
			if (reachedBoth) {
				break;
			}
		}
	}

	float distance1 = isHorizontal ? (uv_interp.x - uv1.x) : (uv_interp.y - uv1.y);
	float distance2 = isHorizontal ? (uv2.x - uv_interp.x) : (uv2.y - uv_interp.y);

	bool isDirection1 = distance1 < distance2;
	float distanceFinal = min(distance1, distance2);

	float edgeThickness = (distance1 + distance2);

	bool isLumaCenterSmaller = lumaCenter < lumaLocalAverage;

	bool correctVariation1 = (lumaEnd1 < 0.0) != isLumaCenterSmaller;
	bool correctVariation2 = (lumaEnd2 < 0.0) != isLumaCenterSmaller;

	bool correctVariation = isDirection1 ? correctVariation1 : correctVariation2;

	float pixelOffset = -distanceFinal / max(edgeThickness, 1e-20) + 0.5;

	float finalOffset = correctVariation ? pixelOffset : 0.0;

	float lumaAverage = (1.0 / 12.0) * (2.0 * (lumaDownUp + lumaLeftRight) + lumaLeftCorners + lumaRightCorners);

	float subPixelOffset1 = clamp(abs(lumaAverage - lumaCenter) / max(lumaRange, 1e-20), 0.0, 1.0);
	float subPixelOffset2 = (-2.0 * subPixelOffset1 + 3.0) * subPixelOffset1 * subPixelOffset1;

	float subPixelOffsetFinal = subPixelOffset2 * subPixelOffset2 * SUBPIXEL_QUALITY;

	finalOffset = max(finalOffset, subPixelOffsetFinal);

	vec2 finalUv = uv_interp;
	if (isHorizontal) {
		finalUv.y += finalOffset * stepLength;
	} else {
		finalUv.x += finalOffset * stepLength;
	}

	vec3 finalColor = textureLod(sampler2D(linear_image, source_sampler), finalUv, 0.0).xyz;
	return finalColor;


}

void main() {
    vec2 uv = gl_FragCoord.xy / vec2(textureSize(sampler2D(linear_image, source_sampler), 0));
    vec3 center = clamp(textureLod(sampler2D(linear_image, source_sampler), uv, 0.0).rgb, 0.0, 1.0);
    vec3 linear_color = clamp(do_fxaa(center, uv), 0.0, 1.0);
#ifdef BISTRO_LINEAR_OUTPUT
    output_color = vec4(linear_color, 1.0);
#else
    vec3 encoded = mix(12.92 * linear_color,
        1.055 * pow(linear_color, vec3(1.0 / 2.4)) - 0.055,
        greaterThan(linear_color, vec3(0.0031308)));
    output_color = vec4(encoded, 1.0);
#endif
}
