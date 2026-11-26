"""Bake two crossed, alpha-tested vegetation cards from each source plant at preparation time."""

import numpy as np
from PIL import Image


def bake(shapes, placement, read, texture, size=96):
    # Some authored prototypes are Z-up. Work in an upright local basis.
    up = np.linalg.solve(placement[:3, :3], [0.0, 1.0, 0.0])
    up /= np.linalg.norm(up)
    side = np.cross(up, np.eye(3)[np.argmin(abs(up))])
    side /= np.linalg.norm(side)
    front = np.cross(side, up)
    basis = np.stack([side, up, front])
    parts = []
    for shape in shapes:
        v, faces = read(shape.file)
        pos = (v[:, :3] @ shape.matrix[:3, :3].T + shape.matrix[:3, 3]) @ basis.T
        a, b, c = pos[faces].transpose(1, 0, 2)
        area = np.linalg.norm(np.cross(b - a, c - a), axis=1)
        if area.sum() > 0:
            parts.append((pos, v[:, 6:8], faces, area, shape))
    total = sum(p[3].sum() for p in parts)
    rng = np.random.default_rng(17)
    cloud, colors = [], []
    for pos, uv, faces, area, shape in parts:
        count = max(1, round(36 * size**2 * area.sum() / total))
        corners = faces[rng.choice(len(faces), count, p=area / area.sum())]
        u, v = rng.random((2, count, 1))
        u = np.sqrt(u)
        w = np.stack([1 - u, u * (1 - v), u * v], axis=1)
        point = (pos[corners] * w).sum(axis=1)
        st = (uv[corners] * w).sum(axis=1)
        rgba = np.asarray(texture(shape))
        x = (st[:, 0] % 1 * rgba.shape[1]).astype(int) % rgba.shape[1]
        y = ((1 - st[:, 1]) % 1 * rgba.shape[0]).astype(int) % rgba.shape[0]
        samples = rgba[y, x]
        visible = samples[:, 3] >= 128
        cloud.append(point[visible])
        colors.append(samples[visible])
    xyz, rgba = np.concatenate(cloud), np.concatenate(colors)
    low, high = xyz.min(axis=0), xyz.max(axis=0)
    atlas = np.zeros((size, size * 2, 4), np.uint8)
    vertices, faces = [], []
    for card, horizontal in enumerate((0, 2)):
        depth = 2 - horizontal
        pixel = (xyz - low) / np.maximum(high - low, 1e-10) * (size - 3) + 1
        x, y = pixel[:, horizontal].astype(int), size - 1 - pixel[:, 1].astype(int)
        # Painter's order at each texel; nearest sampled surface wins.
        order = np.argsort(xyz[:, depth], kind="stable")
        atlas[y[order], card * size + x[order]] = rgba[order]
        for u, v in ((0, 0), (1, 0), (1, 1), (0, 1)):
            local = (low + high) / 2
            local[horizontal] = (low, high)[u][horizontal]
            local[1] = (low, high)[v][1]
            vertices.append([*(local @ basis), *up, (card + u) / 2, v])
        base = 4 * card
        faces.extend([[base, base + 1, base + 2], [base, base + 2, base + 3]])
    # Both sides use the same upward canopy normal. Single-sided material
    # plus reversed indices avoids flipping the lighting across crossing cards.
    faces += [list(reversed(face)) for face in faces]
    return np.array(vertices, "<f4"), np.array(faces, "<u4"), Image.fromarray(atlas)
