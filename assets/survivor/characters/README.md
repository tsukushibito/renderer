# Research subject appearances

The primary and alternate appearances use two different rigged meshes from **Universal Base Characters, Standard edition** by Quaternius. Both use the authored `Walk_Loop` from **Universal Animation Library, Standard v3.0** by Quaternius with animation contributions by Gonzalo Furnier. Both packs are CC0 1.0 Universal. The game ships the cooked overhead meshes, their skin/eye color atlases and sampled joint transforms. [Model source and license](https://quaternius.com/packs/universalbasecharacters.html) · [Animation source and license](https://quaternius.com/packs/universalanimationlibrary.html) · [CC0 dedication](https://creativecommons.org/publicdomain/zero/1.0/)

The fetch script uses an immutable mirror of the free FBX and texture inputs and checks the SHA-256 of each file. The archived model selection is `Superhero_Male_FullBody` for `primary` and `Superhero_Female_FullBody` for `alternate`. [Mirror and package provenance](https://github.com/zongjunlv/FPS/tree/43d750b81ba3b86033656fa281e8a6d59f37609d/Assets/ThirdParty/Quaternius)

## Rebuild both appearances

From the repository root, with Python 3 and Blender 4.5.16 LTS:

```powershell
python tools/survivor/fetch_characters.py .tools/character-source
blender -b -t 1 --factory-startup --python-exit-code 1 -P tools/survivor/cook_character.py -- .tools/character-source assets/survivor/characters
```

The cook reduces the meshes for the overhead camera, retains four normalized joint weights per vertex, and maps each source bone's walk rotation onto the target skeleton while retaining its bone lengths. Both meshes stand 1.8 m tall in bind pose, with Y up, feet at the origin, and +Z forward. The 80-tick looping palettes contain final mesh-space transforms at 60 Hz. Character selection changes presentation only. The game owns movement and contact shape.

| Appearance | Triangles | Joints | Color atlas |
|---|---:|---:|---|
| Primary | 2,145 | 62 | 256 × 256 RGBA8 sRGB |
| Alternate | 2,256 | 62 | 256 × 256 RGBA8 sRGB |
