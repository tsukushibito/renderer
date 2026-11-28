"""Run inside the Namaqualand UE reference project with the Python commandlet."""
import unreal


def prepare_level(source, destination):
    levels = unreal.get_editor_subsystem(unreal.LevelEditorSubsystem)
    if not unreal.EditorAssetLibrary.does_asset_exist(destination):
        if not levels.load_level(source):
            raise RuntimeError(f"Cannot load the reference level {source}")
        editor = unreal.get_editor_subsystem(unreal.UnrealEditorSubsystem)
        if not unreal.EditorLoadingAndSavingUtils.save_map(editor.get_editor_world(), destination):
            raise RuntimeError(f"Cannot create {destination}")
    if not levels.load_level(destination):
        raise RuntimeError(f"Cannot load {destination}")

    actors = unreal.get_editor_subsystem(unreal.EditorActorSubsystem).get_all_level_actors()
    sky, = [actor for actor in actors if isinstance(actor, unreal.SkyLight)]
    dome, = [actor for actor in actors if actor.get_actor_label() == "Rogland Sky"]
    camera, = [actor for actor in actors if actor.get_actor_label() == "Walk Camera"]
    player, = [actor for actor in actors if isinstance(actor, unreal.LevelSequenceActor)]
    # A duplicated map still has the possessable pointing to the source map's camera.
    sequence = player.get_sequence()
    camera_binding, = sequence.get_bindings()
    player.modify()
    player.set_binding(sequence.get_binding_id(camera_binding), [camera], False)

    component = sky.light_component
    if component.get_editor_property("source_type") != unreal.SkyLightSourceType.SLS_CAPTURED_SCENE:
        raise ValueError("Expected the Rogland dome to supply captured sky lighting")
    center, extent = dome.get_actor_bounds(False)
    offset = (sky.get_actor_location() - center).length()
    threshold_cm = 10_000.0
    if threshold_cm >= min(extent.x, extent.y, extent.z) - offset:
        raise ValueError("The sky capture threshold must be inside the Rogland sphere")
    sky.modify()
    component.modify()
    component.set_editor_property("sky_distance_threshold", threshold_cm)
    component.recapture_sky()
    if not levels.save_current_level():
        raise RuntimeError(f"Cannot save the reference level {destination}")
    unreal.log(f"Saved sky reference {destination} with capture threshold {threshold_cm:g} cm")


if __name__ == "__main__":
    prepare_level("/Game/Ya35ShadowAcceptance/Anchors", "/Game/Photometry39z5/AnchorsSky")
    prepare_level("/Game/VSMComparisonCapture/NamaqualandWalk", "/Game/Photometry39z5/WalkSky")
