"""Freeze IBL approximation tolerances from small, independent numeric probes.

This never fits a tolerance to Bistro or cross-API output. The independent
reference uses Halton(2,3) and Smith lambda, whereas the cook uses Hammersley
and the factored correlated-visibility expression. Counts, probes, tolerance
ladder, maximum and deliberate fault probes are fixed here before acceptance.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path

import numpy as np

import ibl


def halton(count):
    columns = []
    for base in (2, 3):
        integer = np.arange(1, count + 1, dtype=np.int64)
        value = np.zeros(count, dtype=np.float64)
        denominator = 1.
        while integer.any():
            denominator *= base
            value += (integer % base) / denominator
            integer //= base
        columns.append(value)
    return np.stack(columns, axis=-1)


def reference_half(xi, roughness):
    alpha = max(roughness * roughness, .001)
    cosine2 = (1 - xi[:, 1]) / (1 + (alpha * alpha - 1) * xi[:, 1])
    phi = xi[:, 0] * (2 * math.pi)
    sine = np.sqrt(np.maximum(1 - cosine2, 0))
    return np.stack((sine * np.cos(phi), sine * np.sin(phi), np.sqrt(cosine2)), axis=-1)


def reference_brdf(nov, roughness, xi):
    h = reference_half(xi, roughness)
    v = np.array([math.sqrt(1 - nov * nov), 0., nov])
    vh = np.maximum(np.sum(v * h, axis=-1), 0)
    light = 2 * vh[:, None] * h - v
    nl = light[:, 2]
    accepted = nl > 0
    alpha = max(roughness * roughness, .001)
    lambda_v = .5 * (math.sqrt(1 + alpha * alpha * (1 - nov * nov) / (nov * nov)) - 1)
    selected_nl = nl[accepted]
    lambda_l = .5 * (np.sqrt(1 + alpha * alpha * (1 - selected_nl ** 2) / selected_nl ** 2) - 1)
    masking = 1 / (1 + lambda_v + lambda_l)
    weight = masking * vh[accepted] / (h[accepted, 2] * nov)
    fc = (1 - vh[accepted]) ** 5
    return np.array([np.sum(weight * (1 - fc)), np.sum(weight * fc)]) / len(xi)


def analytic_radiance(direction):
    return .6 + direction * np.array([.3, .2, .1])


def reference_specular(normal, roughness, xi):
    if roughness == 0:
        return analytic_radiance(normal)
    half = reference_half(xi, roughness)
    local = 2 * half[:, 2, None] * half - np.array([0., 0., 1.])
    axis = np.array([0., 0., 1.]) if abs(normal[2]) < .9 else np.array([1., 0., 0.])
    tangent = np.cross(axis, normal)
    tangent /= np.linalg.norm(tangent)
    bitangent = np.cross(normal, tangent)
    light = local[:, 0, None] * tangent + local[:, 1, None] * bitangent + local[:, 2, None] * normal
    weight = np.maximum(local[:, 2], 0)
    return np.sum(analytic_radiance(light) * weight[:, None], axis=0) / np.sum(weight)


def normalized_error(actual, expected):
    return float(np.max(np.abs(np.asarray(actual) - expected) / np.maximum(1., np.abs(expected))))


def half_round(value):
    return np.asarray(value).astype("<f2").astype(np.float64)


def texel_footprint(direction, width, height):
    u = (.5 + math.atan2(direction[2], direction[0]) / (2 * math.pi)) % 1.
    v = math.acos(float(np.clip(direction[1], -1, 1))) / math.pi
    x, y = u * width - .5, min(max(v * height - .5, 0), height - 1)
    column, row = math.floor(x), math.floor(y)
    fx, fy = x - column, y - row
    return [((column + dx) % width, min(row + dy, height - 1), weight)
            for dx, dy, weight in ((0,0,(1-fx)*(1-fy)),(1,0,fx*(1-fy)),(0,1,(1-fx)*fy),(1,1,fx*fy))]


def texel_direction(column, row, width, height):
    phi = ((column + .5) / width - .5) * (2 * math.pi)
    theta = (row + .5) / height * math.pi
    return np.array([math.sin(theta)*math.cos(phi),math.cos(theta),math.sin(theta)*math.sin(phi)])


def runtime_specular(source, direction, roughness, xi, *, max_lod=ibl.MAX_PREFILTER_LOD, cache=None):
    """Actual production spatial dimensions, half storage, bilinear/trilinear."""
    cache = {} if cache is None else cache
    lod = roughness * max_lod
    lower, upper = math.floor(lod), math.ceil(lod)
    results = []
    for mip in (lower, upper):
        width, height = max(1024 >> mip,1), max(512 >> mip,1)
        value = np.zeros(3)
        for column, row, weight in texel_footprint(direction,width,height):
            key = (max_lod,mip,column,row)
            if key not in cache:
                normal = texel_direction(column,row,width,height)
                cache[key] = half_round(ibl.prefilter_directions(source,[normal],mip/max_lod,xi)[0])
            value += cache[key] * weight
        results.append(value)
    return results[0]*(1-(lod-lower)) + results[1]*(lod-lower)


def runtime_brdf(nov, roughness, xi):
    # Shader clamps the LUT footprint to half-texel centers before LOD0.
    x = np.clip(nov*256-.5,0,255)
    y = np.clip(roughness*256-.5,0,255)
    left, top = math.floor(x),math.floor(y)
    fx,fy = x-left,y-top
    result = np.zeros(2)
    for dx,dy,weight in ((0,0,(1-fx)*(1-fy)),(1,0,fx*(1-fy)),(0,1,(1-fx)*fy),(1,1,fx*fy)):
        column,row = min(left+dx,255),min(top+dy,255)
        result += half_round(ibl.brdf_values([(column+.5)/256],(row+.5)/256,xi)[0])*weight
    return result


def calibrate(samples=1024):
    coarse, fine = halton(65536), halton(262144)
    xi = ibl.hammersley(samples)
    brdf_rows = []
    maximum = 0.
    convergence = 0.
    # Exact centers of the production 256x256 LUT; no texture interpolation
    # uncertainty is conflated with the integration test.
    for roughness in ((index + .5) / 256 for index in (0, 15, 63, 127, 191, 255)):
        for nov in ((index + .5) / 256 for index in (0, 15, 63, 127, 191, 255)):
            expected = reference_brdf(nov, roughness, fine)
            low = reference_brdf(nov, roughness, coarse)
            actual = half_round(ibl.brdf_values([nov], roughness, xi)[0])
            error = normalized_error(actual, expected)
            converged = normalized_error(low, expected)
            maximum, convergence = max(maximum, error), max(convergence, converged)
            brdf_rows.append(dict(nov=nov, roughness=roughness, actual=actual.tolist(), reference=expected.tolist(),
                                  normalized_error=error, reference_convergence=converged))
    source = analytic_radiance(ibl.directions(256, 128))
    diffuse = half_round(ibl.irradiance(source, 8, 4, samples=samples))
    normals = ibl.directions(8, 4)
    expected_diffuse = math.pi * .6 + (2 * math.pi / 3) * normals * np.array([.3, .2, .1])
    diffuse_error = normalized_error(diffuse, expected_diffuse)
    maximum = max(maximum, diffuse_error)
    specular_rows = []
    levels = ibl.prefilter(source, 32, 16, samples=samples)
    prefilter_max_lod=min(ibl.MAX_PREFILTER_LOD,len(levels)-1)
    for mip, level in enumerate(levels):
        height, width = level.shape[:2]
        for row, column in ((0, 0), (height // 2, width // 2), (height - 1, width - 1)):
            normal = ibl.directions(width, height)[row, column]
            roughness = min(mip,prefilter_max_lod) / prefilter_max_lod
            expected = reference_specular(normal, roughness, fine)
            low = reference_specular(normal, roughness, coarse)
            actual = half_round(level[row, column])
            error, converged = normalized_error(actual, expected), normalized_error(low, expected)
            maximum, convergence = max(maximum, error), max(convergence, converged)
            specular_rows.append(dict(mip=mip, roughness=roughness, direction=normal.tolist(), actual=actual.tolist(),
                                      reference=expected.tolist(), normalized_error=error, reference_convergence=converged))
    probes = np.array([[1,0,0],[-1,0,0],[0,1,0],[0,-1,0],[0,0,1],[0,0,-1],
                       [1,1,1],[-1,-1,1],[-1,1,-1],[-1,0,1e-6],[-1,0,-1e-6]],dtype=np.float64)
    probes /= np.linalg.norm(probes,axis=1)[:,None]
    runtime_rows = []
    cache = {}
    legacy_error = 0.
    for roughness in (0.,.05,.1,.125,.2,.35,.375,.5,.625,.65,.8,.875,1.):
        for normal in probes:
            expected = reference_specular(normal,roughness,fine)
            low = reference_specular(normal,roughness,coarse)
            actual = runtime_specular(source,normal,roughness,xi,cache=cache)
            error,converged = normalized_error(actual,expected),normalized_error(low,expected)
            maximum,convergence = max(maximum,error),max(convergence,converged)
            runtime_rows.append(dict(direction=normal.tolist(),roughness=roughness,actual=actual.tolist(),
                                     reference=expected.tolist(),normalized_error=error,reference_convergence=converged))
            if roughness==1.:
                legacy_error=max(legacy_error,normalized_error(runtime_specular(source,normal,roughness,xi,max_lod=10),expected))
    runtime_brdf_rows=[]
    for roughness in (0.,.05,.12,.37,.63,.9,1.):
        for nov in (.04,.1,.33,.55,.89,.99):
            expected=reference_brdf(nov,roughness,fine)
            low=reference_brdf(nov,roughness,coarse)
            actual=runtime_brdf(nov,roughness,xi)
            error,converged=normalized_error(actual,expected),normalized_error(low,expected)
            maximum,convergence=max(maximum,error),max(convergence,converged)
            runtime_brdf_rows.append(dict(nov=nov,roughness=roughness,actual=actual.tolist(),reference=expected.tolist(),
                                          normalized_error=error,reference_convergence=converged))
    production_diffuse=half_round(ibl.irradiance(source,128,64,samples=samples))
    sampled_diffuse=ibl.sample_environment(production_diffuse,probes)
    reference_diffuse=math.pi*.6+(2*math.pi/3)*probes*np.array([.3,.2,.1])
    runtime_diffuse_error=normalized_error(sampled_diffuse,reference_diffuse)
    maximum=max(maximum,runtime_diffuse_error)
    threshold = max(1 / 1024, 2. ** math.ceil(math.log2(max(2 * maximum, 2. ** -1024))))
    # Every deliberate fault must be detected by the frozen tolerance, not by a
    # separate loose image mask. These probes are algorithm/boundary sensitivity
    # evidence; the real shader path is separately checked by both GPU backends.
    mutations = {
        "missing_ibl": normalized_error(np.zeros_like(diffuse), expected_diffuse),
        "strength_applied_twice": normalized_error(diffuse * 3, expected_diffuse),
        "yaw_180_degrees": normalized_error(np.roll(diffuse, diffuse.shape[1]//2, axis=1), expected_diffuse),
        "vertical_flip": normalized_error(np.flip(diffuse, axis=0), expected_diffuse),
        "brdf_channels_swapped": max(normalized_error(np.array(row["actual"])[::-1], np.array(row["reference"]))
                                      for row in brdf_rows),
        "roughness_reversed": max(normalized_error(half_round(ibl.brdf_values([row["nov"]], 1 - row["roughness"], xi)[0]),
                                                   np.array(row["reference"])) for row in brdf_rows),
        "legacy_roughness1_at_1x1_lod10":legacy_error,
    }
    passed = threshold <= 1 / 32 and convergence <= threshold / 4 and all(error > threshold for error in mutations.values())
    return dict(schema_version=1, status="passed" if passed else "failed", samples=samples,
                profile=dict(normalization="abs(actual-reference)/max(1,abs(reference))", maximum_exceeding_fraction=0.,
                             threshold=threshold, floor=1/1024, cap=1/32,
                             rule="ceil_pow2(2*max_small_pilot_error), clamped only to lower floor; reject above cap",
                             reference_convergence_maximum=convergence, required_reference_convergence=threshold/4),
                maximum_error=maximum, diffuse_error=diffuse_error, brdf=brdf_rows, specular=specular_rows,
                runtime_specular=runtime_rows,runtime_brdf=runtime_brdf_rows,runtime_diffuse_error=runtime_diffuse_error,
                max_prefilter_lod=ibl.MAX_PREFILTER_LOD,
                maximum_errors_by_category=dict(brdf=max(row["normalized_error"] for row in brdf_rows),
                    specular_texels=max(row["normalized_error"] for row in specular_rows),
                    runtime_specular=max(row["normalized_error"] for row in runtime_rows),
                    runtime_brdf=max(row["normalized_error"] for row in runtime_brdf_rows),
                    diffuse=diffuse_error,runtime_diffuse=runtime_diffuse_error),
                mutations={name:dict(error=value, detected=value > threshold) for name,value in mutations.items()},
                reference=dict(sequence="Halton bases 2,3; starts at index1", counts=[65536,262144],
                               visibility="independent Smith lambda formulation", precision="float64"),
                cook_tool_sha256=ibl.tool_hash(), pilot_tool_sha256=hashlib.sha256(Path(__file__).read_text(encoding="utf-8").encode()).hexdigest())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--samples", type=int, default=1024, choices=[1024,4096,16384])
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = calibrate(args.samples)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, allow_nan=False, sort_keys=True) + "\n",encoding="utf-8")
    print(json.dumps({key:result[key] for key in ("status","samples","maximum_error","profile")},sort_keys=True))
    if result["status"] != "passed":
        raise SystemExit("IBL numeric pilot failed; investigate or advance the fixed sample ladder before formal acceptance")


if __name__ == "__main__":
    main()
