"""Create the Namaqualand UE rock/terrain reference maps after preparing the sky maps."""
from pathlib import Path
import sys

import unreal

sys.path.insert(0, str(Path(__file__).resolve().parent))
from prepare_contact_reference import CONTACT_MATERIALS
from prepare_ue_sky_reference import prepare_level


def contact_components(actors, names):
    found = {}
    for actor in actors.get_all_level_actors():
        if isinstance(actor, unreal.InstancedFoliageActor):
            for comp in actor.get_components_by_class(unreal.InstancedStaticMeshComponent):
                mesh = comp.static_mesh
                if mesh and mesh.get_name() in names and comp.get_instance_count():
                    name = mesh.get_name()
                    if name in found:
                        raise ValueError(f"Expected a single foliage group for {name}")
                    found[name] = (actor, comp)
    if found.keys() != names:
        raise ValueError(f"Missing reference meshes: {sorted(names - found.keys())}")
    return found


def prepare_contact_level(source, destination):
    prepare_level(source, destination)
    library = unreal.EditorAssetLibrary
    names = {f"WalkMesh{index:03}" for index in CONTACT_MATERIALS}
    world = unreal.get_editor_subsystem(unreal.UnrealEditorSubsystem).get_editor_world()
    actors = unreal.get_editor_subsystem(unreal.EditorActorSubsystem)
    groups = contact_components(actors, names)
    expected_counts = {name: comp.get_instance_count() for name, (_, comp) in groups.items()}
    for name, (actor, component) in groups.items():
        mesh = component.static_mesh
        path = f"/Game/ContactGeometry/SourceMeshes/{name}"
        if library.does_asset_exist(path):
            reference = library.load_asset(path)
        else:
            reference = library.duplicate_asset(mesh.get_path_name(), path)
        if not reference:
            raise RuntimeError(f"Cannot load or create {path}")
        settings = reference.get_editor_property("nanite_settings")
        if settings.enabled:
            settings.enabled = False
            reference.set_editor_property("nanite_settings", settings)
            if not library.save_loaded_asset(reference):
                raise RuntimeError(f"Cannot save source geometry for {name}")
        # A foliage component is reconstructed from its type when the editor reloads.
        # Duplicate the type as well so the other maps retain their imported mesh.
        source_type = library.load_asset(f"/Game/WalkReference/Types/FT_{name}")
        if not source_type:
            raise RuntimeError(f"Missing imported foliage type for {name}")
        type_path = f"/Game/ContactGeometry/Types/FT_{name}"
        if library.does_asset_exist(type_path):
            ref_type = library.load_asset(type_path)
        else:
            ref_type = library.duplicate_asset(source_type.get_path_name(), type_path)
        if not ref_type:
            raise RuntimeError(f"Cannot prepare foliage type {type_path}")
        if ref_type.get_editor_property("mesh") != reference:
            ref_type.set_editor_property("mesh", reference)
            if not library.save_loaded_asset(ref_type):
                raise RuntimeError(f"Cannot save {type_path}")
        if mesh != reference:
            transforms = [component.get_instance_transform(i, world_space=True)
                          for i in range(component.get_instance_count())]
            actor.modify()
            unreal.InstancedFoliageActor.remove_all_instances(world, source_type)
            unreal.InstancedFoliageActor.add_instances(world, ref_type, transforms)
    for name, (_, comp) in contact_components(actors, names).items():
        if (comp.get_instance_count() != expected_counts[name] or
                comp.static_mesh.get_path_name() != f"/Game/ContactGeometry/SourceMeshes/{name}.{name}"):
            raise RuntimeError(f"Incomplete foliage replacement for {name}")
    if not unreal.get_editor_subsystem(unreal.LevelEditorSubsystem).save_current_level():
        raise RuntimeError(f"Cannot save {destination}")


if __name__ == "__main__":
    for prefix in ("Anchors", "Walk"):
        prepare_contact_level(f"/Game/Photometry39z5/{prefix}Sky", f"/Game/ContactGeometry/{prefix}Exact")
