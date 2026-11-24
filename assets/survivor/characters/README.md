# Research subject appearances

The primary and alternate appearances use two different rigged meshes from **Universal Base Characters, Standard edition** by Quaternius. Both use the authored `Walk_Loop` and `Jog_Fwd_Loop` from **Universal Animation Library, Standard v3.0** by Quaternius with animation contributions by Gonzalo Furnier. Both packs are CC0 1.0 Universal. The game ships the cooked overhead meshes, their skin/eye color atlases and sampled joint transforms. [Model source and license](https://quaternius.com/packs/universalbasecharacters.html) · [Animation source and license](https://quaternius.com/packs/universalanimationlibrary.html) · [CC0 dedication](https://creativecommons.org/publicdomain/zero/1.0/)

The fetch script uses an immutable mirror of the free FBX and texture inputs and checks the SHA-256 of each file. The archived model selection is `Superhero_Male_FullBody` for `primary` and `Superhero_Female_FullBody` for `alternate`. [Mirror and package provenance](https://github.com/zongjunlv/FPS/tree/43d750b81ba3b86033656fa281e8a6d59f37609d/Assets/ThirdParty/Quaternius)

## Rebuild both appearances

From the repository root, with Python 3 and Blender 4.5.16 LTS:

```powershell
python tools/survivor/fetch_characters.py .tools/character-source
blender -b -t 1 --factory-startup --python-exit-code 1 -P tools/survivor/cook_character.py -- .tools/character-source assets/survivor/characters
```

The cook reduces the meshes for the overhead camera, retains four normalized joint weights per vertex, and maps each source bone's walk rotation onto the target skeleton while retaining its bone lengths. Both meshes stand 1.8 m tall in bind pose, with Y up, feet at the origin, and +Z forward. The walk has 80 samples and the jog has 56. Both clips loop at 60 Hz and contain final mesh-space transforms. Character selection changes presentation only. The game owns movement and contact shape.

| Appearance | Triangles | Joints | Color atlas |
|---|---:|---:|---|
| Primary | 2,145 | 62 | 256 × 256 RGBA8 sRGB |
| Alternate | 2,256 | 62 | 256 × 256 RGBA8 sRGB |

## Select and extend a character

The binary resolves `assets/survivor/characters/catalog.json` beside the executable in a distribution. During development it can read the repository catalog. Pass `--character-catalog <file>` to use an edited catalog, and `--character <name>` to select the player's model. The legacy `--alternate-model` option selects the `alternate` entry. The preview and renderer both use the Sashiko cooked character loader.

For compatible FBX rigs, add entries in `tools/survivor/character-recipes.json` and rerun the Blender cook. The cook appends every declared FBX action to the palette stream and writes the catalog with the sample range for each clip. A custom recipe can be supplied with `--recipe <file>` after the two directories.

To add a visual variant from an already cooked skin, add a model entry with a unique stable 32-hex-digit ID, paths to its cooked skin and top-row-first RGBA8 color texture, color extent and named clips. Paths are relative to the catalog directory. Each clip declares its first sample, sample count, sample rate and loop flag. To reuse a mesh for a faster movement, another clip can reference the same sample range and specify a higher playback rate. A one-shot clip holds its last sample.

A `Subject` prefab can select the catalog entry with its `model` field and a named animation with `clip`. A null model follows the player's selected model; the player uses the `walk` clip. Subjects sharing a model and clip share four phase-shifted pose streams at runtime.

## Transition between clips

The version 2 skin stores the target rig's bind transforms and nearest retained parents. Prepare a clip pair once with `model.prepare_crossfade("walk", "jog")?`. For a walk already at tick 36 and a 12-tick transition, call `fade.sample(36, elapsed_ticks, duration, tick_rate)?` as the fixed clock advances. Pass the returned palette to the posed mesh. Both clips advance through the transition. The exact cooked source and destination samples are returned at the endpoints.

Preparing the pair recovers the joint-local poses from the final skin matrices. Sampling interpolates positive scales and translations linearly and takes the shortest quaternion arc for rotations, then propagates the pose from parent to child before applying inverse bind transforms. Interpolation is relative to the nearest retained ancestor where the cook dropped unweighted bones. Joint-local shear and reflections are reported during transition preparation. Direct clip playback uses the original matrix samples for both skin versions and allocates no blend tracks.
