"""Cook an indexed mesh's independent lightmap parameterization in source corner order."""
import numpy as np
import shapely
import xatlas

TEXELS_PER_METRE = 4.0
PADDING = 2


def authored_layout(positions, faces, uv, areas, density):
    """Reject folded, overlapping and unpadded islands at the reference density."""
    if uv.shape != (len(positions), 2) or not np.isfinite(uv).all() or np.any((uv < 0) | (uv > 1)):
        return None
    active = areas > 0
    if not active.any():
        return None
    polys = shapely.polygons(uv[faces[active]])
    uv_area = shapely.area(polys)
    if (uv_area <= 0).any():
        return None
    size = max(2, int(np.ceil(density * np.sqrt(areas.sum() / uv_area.sum()))))
    # Polygon union also detects containment and coincident triangles.
    island_union = shapely.union_all(polys)
    if uv_area.sum() - island_union.area > 1e-12:
        return None
    islands = shapely.get_parts(island_union)
    if len(islands) > 1:
        pairs = shapely.STRtree(islands).query(islands, predicate="dwithin", distance=2 * PADDING / size)
        if np.any(pairs[0] != pairs[1]):
            return None
    # Coincident UV edges may join different parts of the physical mesh.
    # Each UV endpoint must identify one physical position.
    endpoint = np.unique(np.concatenate([uv, positions], axis=1)[faces[active].ravel()], axis=0)
    if len(np.unique(endpoint[:, :2], axis=0)) != len(endpoint):
        return None
    return uv[faces], [size, size], "authored"


def parameterize(gltf, primitive, world, density=TEXELS_PER_METRE):
    if not np.isfinite(density) or density <= 0:
        raise ValueError("Lightmap texels per metre must be finite and positive")
    corners, extent, source = _parameterize(gltf, primitive, world, density)
    # Validate the stored float32 coordinates, including source slivers that
    # become non-collinear only after transforming and rounding world positions.
    positions = gltf.accessor(primitive["attributes"]["POSITION"])
    indices = gltf.accessor(primitive["indices"]).ravel() if "indices" in primitive else np.arange(len(positions))
    points = positions[indices.reshape(-1, 3)]
    u = corners.astype("<f4").astype(np.float64)
    a, b = u[:, 1] - u[:, 0], u[:, 2] - u[:, 0]
    distinct = np.all([np.any(points[:, i] != points[:, (i+1) % 3], axis=1) for i in range(3)], axis=0)
    collapsed = np.flatnonzero(distinct & (np.abs(a[:, 0]*b[:, 1] - a[:, 1]*b[:, 0]) <= 1e-13))
    if not len(collapsed):
        return corners, extent, source
    # Give each sub-texel face a dedicated sample and a chart filtering gutter.
    pitch = 2 * PADDING + 2
    columns = min(len(collapsed), max(1, extent[0] // pitch))
    rows = (len(collapsed) + columns - 1) // columns
    size = [max(extent[0], columns*pitch), extent[1] + rows*pitch]
    u *= extent
    for slot, face in enumerate(collapsed):
        x, y = (slot % columns)*pitch + PADDING, extent[1] + (slot // columns)*pitch + PADDING
        u[face] = [[x, y], [x+1, y], [x, y+1]]
    return (u / size).astype("<f4"), size, source


def _parameterize(gltf, primitive, world, density):
    attributes = primitive["attributes"]
    positions = gltf.accessor(attributes["POSITION"])
    faces = (gltf.accessor(primitive["indices"]).ravel() if "indices" in primitive
             else np.arange(len(positions))).astype("<u4").reshape(-1, 3)
    # Translation is irrelevant to density. Applying only the linear part also
    # avoids losing small features on placements far from the origin.
    metres = positions @ world[:3, :3].T
    triangles = metres[faces]
    areas = np.linalg.norm(np.cross(triangles[:, 1] - triangles[:, 0],
                                    triangles[:, 2] - triangles[:, 0]), axis=1) / 2
    if "TEXCOORD_1" in attributes:
        result = authored_layout(metres, faces, gltf.accessor(attributes["TEXCOORD_1"]), areas, density)
        if result is not None:
            return result
    active = areas > 0
    corners = np.zeros((len(faces), 3, 2), dtype="<f4")
    if not active.any():
        return corners, [1, 1], "degenerate"
    atlas = xatlas.Atlas()
    atlas.add_mesh(np.ascontiguousarray(metres, dtype="<f4"), faces[active])
    pack = xatlas.PackOptions()
    pack.texels_per_unit = density
    pack.padding = PADDING
    pack.bilinear = True
    atlas.generate(pack_options=pack)
    mapping, indices, uv = atlas[0]
    if atlas.atlas_count != 1 or not np.array_equal(mapping[indices], faces[active]):
        raise ValueError("xatlas did not preserve receiver face order")
    if max(atlas.width, atlas.height) > 2046:
        # A nearly collapsed source face can give an xatlas island an extreme
        # aspect ratio despite its small world extent. Reproject only such
        # islands into metric triangle charts, retaining their requested density.
        charts = []
        faces_active = faces[active]
        packed_uv = uv[indices] * [atlas.width, atlas.height]
        for chart in range(atlas.chart_count):
            ids = atlas.get_mesh_chart(0, chart).faces
            points = packed_uv[ids]
            span = np.ptp(points.reshape(-1, 2), axis=0)
            if max(span) <= 2040:
                charts.append((ids, points))
            else:
                for face in ids:
                    a, b, c = metres[faces_active[face]]
                    edge = b - a
                    length = np.linalg.norm(edge)
                    axis = edge / length
                    u = np.dot(c - a, axis)
                    v = np.linalg.norm(c - a - u * axis)
                    charts.append(([face], np.array([[[0., 0.], [length, 0.], [u, v]]]) * density))
        boxes = []
        for ids, points in charts:
            points = points - points.reshape(-1, 2).min(axis=0)
            width, height = np.ceil(points.reshape(-1, 2).max(axis=0)).astype(int) + 2 * PADDING + 1
            boxes.append((height, width, ids, points))
        x = y = row = used = 0
        for height, width, ids, points in sorted(boxes, key=lambda box: (-box[0], -box[1])):
            if x + width > 2046:
                x = 0
                y += row
                row = 0
            packed_uv[ids] = points + [x + PADDING + .5, y + PADDING + .5]
            x += width
            used = max(used, x)
            row = max(row, height)
        extent = [int(used), int(y + row)]
        corners[active] = packed_uv / extent
        return corners, extent, "xatlas"
    corners[active] = uv[indices]
    return corners, [atlas.width, atlas.height], "xatlas"
