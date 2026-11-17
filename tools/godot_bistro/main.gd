extends Node3D
## External assets are loaded once, outside the measured window, in release templates.
## Timings describe Godot's viewport/setup scopes, not Renderer phase equivalents.

var config: Dictionary
var conditions: Dictionary
var camera: Camera3D
var samples: Array = []
var tick := 0
var previous_post_usec := 0
var roi_start_usec := 0
var pre_draw_usec := 0
var cpu_clock_enabled := false
var cpu_start: PackedInt64Array
var cpu_start_after_usec := 0
var inventory := {"mesh_instances": 0, "surfaces": 0, "triangles": 0}
var import_inventory: Dictionary
var finished := false
var environment: Environment
var sun: DirectionalLight3D
var panorama: PanoramaSkyMaterial
var sky: Sky
var ibl_identity: Dictionary = {}
var mesh_shadow_modes: Dictionary = {}
var b3_materials: Array[Material] = []
var material_mip_inventory: Dictionary = {}
var ao_enabled_before := 0
var completed_report: Dictionary
var capture_frames: Dictionary = {}
const CAPTURE_SETTLING_FRAMES := 8
const CAPTURE_MAX_FRAMES := 16
const MaterialMips = preload("res://material_mips.gd")

func fail(message: String) -> void:
	push_error(message)
	get_tree().quit(1)

func vec(values: Array) -> Vector3:
	return Vector3(values[0], values[1], values[2])

func triple(value: Vector3) -> Array:
	return [value.x, value.y, value.z]

func quality_settings() -> Dictionary:
	# Read back the live objects and loaded project settings, not config echoes.
	var background := environment.background_color.srgb_to_linear()
	var result := {
		"profile": config.profile, "background_mode": environment.background_mode,
		"background_linear_rgb": [background.r, background.g, background.b],
		"background_energy": environment.background_energy_multiplier,
		"ambient_source": environment.ambient_light_source,
		"ambient_energy": environment.ambient_light_energy,
		"ambient_sky_contribution": environment.ambient_light_sky_contribution,
		"reflection_source": environment.reflected_light_source,
		"sky_rotation": triple(environment.sky_rotation),
		"sun_energy": sun.light_energy,
		"sun_color": [sun.light_color.r, sun.light_color.g, sun.light_color.b],
		"sun_direction": triple(sun.global_basis.z),
		"shadow_enabled": sun.shadow_enabled,
		"tonemap_mode": environment.tonemap_mode, "exposure": environment.tonemap_exposure,
		"screen_space_aa": get_viewport().screen_space_aa,
		"msaa_3d": get_viewport().msaa_3d, "taa": get_viewport().use_taa,
		"scaling_3d_scale": get_viewport().scaling_3d_scale,
		"mesh_lod_threshold": get_viewport().mesh_lod_threshold,
		"occlusion_culling": get_viewport().use_occlusion_culling,
		"ssao": environment.ssao_enabled, "ssil": environment.ssil_enabled,
		"ssr": environment.ssr_enabled, "sdfgi": environment.sdfgi_enabled,
		"glow": environment.glow_enabled, "fog": environment.fog_enabled,
		"volumetric_fog": environment.volumetric_fog_enabled,
		"adjustments": environment.adjustment_enabled, "debanding": get_viewport().use_debanding,
		"physical_light_units": ProjectSettings.get_setting("rendering/lights_and_shadows/use_physical_light_units"),
		"mesh_shadow_casting": mesh_shadow_modes.keys()[0] if mesh_shadow_modes.size() == 1 else -1,
	}
	if config.profile == "b3":
		var ao_enabled := 0
		var texture_filters: Dictionary = {}
		for material in b3_materials:
			if (material as StandardMaterial3D).ao_enabled:
				ao_enabled += 1
			texture_filters[(material as StandardMaterial3D).texture_filter] = true
		result.merge({
			"ambient_occlusion_enabled_materials": ao_enabled,
			"material_full_mipmaps": material_mip_inventory.get("verified", false),
			"material_texture_filter": texture_filters.keys()[0] if texture_filters.size() == 1 else -1,
			"nearest_mipmap_filter": ProjectSettings.get_setting("rendering/textures/default_filters/use_nearest_mipmap_filter"),
			"ibl_energy": panorama.energy_multiplier,
			"sky_process_mode": sky.process_mode, "sky_radiance_size": sky.radiance_size,
			"shadow_mode": sun.directional_shadow_mode,
			"shadow_distance": sun.directional_shadow_max_distance,
			"shadow_splits": [sun.directional_shadow_split_1, sun.directional_shadow_split_2, sun.directional_shadow_split_3],
			"shadow_blend_splits": sun.directional_shadow_blend_splits,
			"shadow_fade_start": sun.directional_shadow_fade_start,
			"shadow_bias": sun.shadow_bias, "shadow_normal_bias": sun.shadow_normal_bias,
			"shadow_blur": sun.shadow_blur, "shadow_pancake_size": sun.directional_shadow_pancake_size,
			"sun_angular_distance": sun.light_angular_distance,
			"shadow_atlas_size": ProjectSettings.get_setting("rendering/lights_and_shadows/directional_shadow/size"),
			"shadow_16_bits": ProjectSettings.get_setting("rendering/lights_and_shadows/directional_shadow/16_bits"),
			"shadow_filter_quality": ProjectSettings.get_setting("rendering/lights_and_shadows/directional_shadow/soft_shadow_filter_quality"),
			"agx_white": environment.tonemap_agx_white, "agx_contrast": environment.tonemap_agx_contrast,
		})
	return result

func _ready() -> void:
	var args := OS.get_cmdline_user_args()
	if args.size() != 1:
		fail("Expected one absolute configuration JSON path")
		return
	config = JSON.parse_string(FileAccess.get_file_as_string(args[0]))
	conditions = JSON.parse_string(FileAccess.get_file_as_string(config.conditions))
	if int(ProjectSettings.get_setting("rendering/driver/threads/thread_model")) != 1:
		fail("CPU elapsed scopes require pinned Safe/main-thread rendering")
		return
	if config.has("cpu_clock_library"):
		if GDExtensionManager.load_extension(config.cpu_clock_library) != GDExtensionManager.LOAD_STATUS_OK:
			fail("Could not load the optional benchmark CPU clock")
			return
		cpu_clock_enabled = ClassDB.class_exists("B3CpuClock")
		if not cpu_clock_enabled:
			fail("Optional CPU clock class is unavailable")
			return
	if OS.has_feature("editor") or OS.has_feature("debug"):
		fail("Benchmark requires a release export template")
		return
	if RenderingServer.get_current_rendering_driver_name() != "vulkan":
		fail("Vulkan was not selected")
		return
	if RenderingServer.get_current_rendering_method() != "forward_plus":
		fail("Forward+ was not selected")
		return
	Engine.max_fps = 0
	DisplayServer.window_set_vsync_mode(DisplayServer.VSYNC_DISABLED)
	get_viewport().mesh_lod_threshold = 0.0
	get_viewport().use_occlusion_culling = false
	get_viewport().msaa_3d = Viewport.MSAA_DISABLED
	get_viewport().use_taa = false
	get_viewport().use_debanding = false
	get_viewport().scaling_3d_scale = 1.0
	get_viewport().screen_space_aa = Viewport.SCREEN_SPACE_AA_FXAA if config.profile == "b3" else Viewport.SCREEN_SPACE_AA_DISABLED
	var doc := GLTFDocument.new()
	var state := GLTFState.new()
	var error := doc.append_from_file(config.gltf, state, GLTFDocument.IMPORT_FLAG_GENERATE_TANGENT_ARRAYS)
	if error != OK:
		fail("glTF load failed: %s" % error)
		return
	var scene := doc.generate_scene(state)
	if scene == null:
		fail("glTF scene generation failed")
		return
	add_child(scene)
	import_inventory = {"materials": state.get_materials().size(), "images": state.get_images().size()}
	if import_inventory != {"materials": 132, "images": 405}:
		fail("Bistro material/image inventory differs: %s" % import_inventory)
		return
	if config.profile == "b3":
		# Fixed Bistro source policy: the packed ORM R channel is disabled as in
		# the original material configuration; metallic/roughness/source bytes stay.
		b3_materials = state.get_materials()
		for material in b3_materials:
			if not material is StandardMaterial3D:
				fail("B3 source-AO override requires StandardMaterial3D imports")
				return
			var standard := material as StandardMaterial3D
			if standard.ao_enabled:
				ao_enabled_before += 1
			standard.ao_enabled = false
		material_mip_inventory = MaterialMips.prepare(state.get_images(), b3_materials)
		if material_mip_inventory.has("error"):
			fail(material_mip_inventory.error)
			return
	inspect_scene(scene)
	if inventory != {"mesh_instances": 1296, "surfaces": 1591, "triangles": 2832120}:
		fail("Bistro inventory differs: %s" % inventory)
		return
	var world := WorldEnvironment.new()
	environment = Environment.new()
	environment.background_mode = Environment.BG_COLOR
	# Godot Color properties are sRGB; the source condition is scene-linear.
	var background := vec(conditions.output.background_linear_rgb)
	environment.background_color = Color(background.x, background.y, background.z).linear_to_srgb()
	environment.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
	environment.ambient_light_color = Color.WHITE
	environment.ambient_light_energy = 0.03
	environment.ambient_light_sky_contribution = 0.0
	# No sky or reflection probes are created; background is a constant color.
	environment.tonemap_mode = Environment.TONE_MAPPER_LINEAR
	environment.tonemap_exposure = 1.0
	environment.ssao_enabled = false
	environment.ssil_enabled = false
	environment.ssr_enabled = false
	environment.sdfgi_enabled = false
	environment.glow_enabled = false
	environment.fog_enabled = false
	environment.volumetric_fog_enabled = false
	environment.adjustment_enabled = false
	if config.profile == "b3":
		var image := Image.load_from_file(config.ibl_path)
		if image == null or image.is_empty():
			fail("Cannot load the cooked B3 IBL panorama")
			return
		ibl_identity = {"sha256": FileAccess.get_sha256(config.ibl_path), "width": image.get_width(), "height": image.get_height()}
		# JSON numbers are float Variants; Dictionary equality is type-strict even
		# though scalar int/float equality preserves the required numeric value.
		if (ibl_identity.sha256 != config.ibl.sha256
				or ibl_identity.width != config.ibl.width or ibl_identity.height != config.ibl.height):
			fail("B3 IBL panorama identity differs from its cook: actual %s expected %s" % [ibl_identity, config.ibl])
			return
		panorama = PanoramaSkyMaterial.new()
		panorama.panorama = ImageTexture.create_from_image(image)
		panorama.energy_multiplier = 3.0
		sky = Sky.new()
		sky.sky_material = panorama
		sky.process_mode = Sky.PROCESS_MODE_QUALITY
		sky.radiance_size = Sky.RADIANCE_SIZE_512
		environment.sky = sky
		environment.sky_rotation = Vector3.ZERO
		environment.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
		environment.ambient_light_sky_contribution = 1.0
		environment.ambient_light_energy = 1.0
		environment.reflected_light_source = Environment.REFLECTION_SOURCE_SKY
		environment.tonemap_mode = Environment.TONE_MAPPER_AGX
		environment.tonemap_exposure = 2.0
		environment.tonemap_agx_white = 16.29
		environment.tonemap_agx_contrast = 1.25
	world.environment = environment
	add_child(world)
	sun = DirectionalLight3D.new()
	sun.shadow_enabled = false
	sun.light_energy = 1.0
	if config.profile == "b3":
		sun.shadow_enabled = true
		sun.directional_shadow_mode = DirectionalLight3D.SHADOW_PARALLEL_4_SPLITS
		sun.directional_shadow_max_distance = 120.0
		sun.directional_shadow_split_1 = config.quality.shadow_splits[0]
		sun.directional_shadow_split_2 = config.quality.shadow_splits[1]
		sun.directional_shadow_split_3 = config.quality.shadow_splits[2]
		sun.directional_shadow_blend_splits = true
		sun.directional_shadow_fade_start = 0.9
		sun.shadow_bias = 0.1
		sun.shadow_normal_bias = 2.0
		sun.shadow_blur = 1.0
		sun.directional_shadow_pancake_size = 20.0
		# Keep fixed PCF. Nonzero angular distance would add Godot PCSS.
		sun.light_angular_distance = 0.0
	add_child(sun)
	sun.look_at(-vec(conditions.lighting.sun.direction_to_light), Vector3.UP)
	camera = Camera3D.new()
	camera.near = conditions.camera.near_m
	camera.far = conditions.camera.far_m
	camera.keep_aspect = Camera3D.KEEP_HEIGHT
	add_child(camera)
	camera.make_current()
	set_camera(0)
	samples.resize(int(config.frames))
	RenderingServer.viewport_set_measure_render_time(get_viewport().get_viewport_rid(), config.timing)
	RenderingServer.frame_pre_draw.connect(before_draw)
	RenderingServer.frame_post_draw.connect(after_draw)
	print("BISTRO_READY ", JSON.stringify(inventory))

func inspect_scene(node: Node) -> void:
	if node is Light3D:
		# The pinned benchmark supplies its own light; ignore glTF authoring lights.
		(node as Light3D).light_energy = 0.0
	if node is MeshInstance3D:
		var instance := node as MeshInstance3D
		inventory.mesh_instances += 1
		RenderingServer.instance_set_ignore_culling(instance.get_instance(), config.culling == "all")
		instance.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_ON if config.profile == "b3" else GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
		mesh_shadow_modes[instance.cast_shadow] = true
		for surface in instance.mesh.get_surface_count():
			inventory.surfaces += 1
			var arrays := instance.mesh.surface_get_arrays(surface)
			var indices: PackedInt32Array = arrays[Mesh.ARRAY_INDEX]
			var vertices: PackedVector3Array = arrays[Mesh.ARRAY_VERTEX]
			inventory.triangles += (indices.size() if not indices.is_empty() else vertices.size()) / 3
	for child in node.get_children():
		inspect_scene(child)

func view_named(name: String) -> Dictionary:
	for view in conditions.views:
		if view.name == name:
			return view
	return {}

func set_camera(frame: int) -> void:
	var route: Dictionary = conditions.route
	var period := int(route.duration_seconds * route.fps)
	var route_frame := frame % period
	var keys: Array = route.keyframes
	for index in range(keys.size() - 1):
		var start := int(keys[index].seconds * route.fps)
		var end := int(keys[index + 1].seconds * route.fps)
		if route_frame >= start and route_frame < end:
			var first := view_named(keys[index].view)
			var second := view_named(keys[index + 1].view)
			var weight := float(route_frame - start) / float(end - start)
			camera.position = vec(first.position).lerp(vec(second.position), weight)
			camera.look_at(vec(first.target).lerp(vec(second.target), weight), Vector3.UP)
			camera.fov = lerpf(first.vertical_fov_degrees, second.vertical_fov_degrees, weight)
			return

func _process(_delta: float) -> void:
	if camera != null and not finished:
		# The logical 60Hz route is indexed by rendered frames, never wall time.
		set_camera(tick if tick < int(config.warmup) else tick - int(config.warmup))

func before_draw() -> void:
	pre_draw_usec = Time.get_ticks_usec()

func cpu_snapshot() -> PackedInt64Array:
	var value: PackedInt64Array = ClassDB.class_call_static("B3CpuClock", "snapshot")
	if value.size() != 7 or value[0] < 0 or value[1] <= 0:
		fail("Native CPU boundary snapshot failed")
		return PackedInt64Array()
	return value

func after_draw() -> void:
	if finished:
		return
	var now := Time.get_ticks_usec()
	var frame := tick - int(config.warmup)
	if frame == -1:
		roi_start_usec = now
		if cpu_clock_enabled:
			cpu_start = cpu_snapshot()
			cpu_start_after_usec = Time.get_ticks_usec()
	var cpu_end := PackedInt64Array()
	var cpu_end_after_usec := 0
	if frame == int(config.frames) - 1 and cpu_clock_enabled:
		cpu_end = cpu_snapshot()
		cpu_end_after_usec = Time.get_ticks_usec()
	if frame >= 0 and frame < int(config.frames):
		var viewport := get_viewport().get_viewport_rid()
		var sample := {"frame": frame, "frame_interval_ms": float(now - previous_post_usec) / 1000.0,
			"render_pre_post_ms": float(now - pre_draw_usec) / 1000.0,
			"observation_frame_drawn": Engine.get_frames_drawn()}
		if config.timing:
			sample.cpu_setup_ms = RenderingServer.get_frame_setup_time_cpu()
			sample.cpu_viewport_ms = RenderingServer.viewport_get_measured_render_time_cpu(viewport)
			sample.gpu_ms = RenderingServer.viewport_get_measured_render_time_gpu(viewport)
			sample.draw_calls = RenderingServer.get_rendering_info(RenderingServer.RENDERING_INFO_TOTAL_DRAW_CALLS_IN_FRAME)
			sample.primitives = RenderingServer.get_rendering_info(RenderingServer.RENDERING_INFO_TOTAL_PRIMITIVES_IN_FRAME)
		samples[frame] = sample
	previous_post_usec = now
	tick += 1
	if frame == int(config.frames) - 1:
		finished = true
		var report := {"schema_version": 2, "config": config, "inventory": inventory,
			"quality": quality_settings(), "ibl": ibl_identity,
			"godot": Engine.get_version_info(), "debug": OS.has_feature("debug"),
			"import_inventory": import_inventory,
			"adapter": RenderingServer.get_video_adapter_name(),
			"vendor": RenderingServer.get_video_adapter_vendor(),
			"api_version": RenderingServer.get_video_adapter_api_version(),
			"driver": RenderingServer.get_current_rendering_driver_name(),
			"renderer": RenderingServer.get_current_rendering_method(),
			"render_thread_model": int(ProjectSettings.get_setting("rendering/driver/threads/thread_model")),
			"frame_queue": {"configured_frames": ProjectSettings.get_setting("rendering/rendering_device/vsync/frame_queue_size"),
				"actual_rd_frame_delay": RenderingServer.get_rendering_device().get_frame_delay(),
				"requested_swapchain_images": ProjectSettings.get_setting("rendering/rendering_device/vsync/swapchain_image_count"),
				"actual_swapchain_images": null,
				"scope": "RD ring slots, not host-owned pending count or actual GPU occupancy; requested swapchain images may be driver-clamped and actual count is not exposed here"},
			"worker_pool": {"configured_max_threads": ProjectSettings.get_setting("threading/worker_pool/max_threads"),
				"low_priority_thread_ratio": ProjectSettings.get_setting("threading/worker_pool/low_priority_thread_ratio"),
				"actual_threads": null,
				"actual_threads_source": "not exposed to script; separate verbose metadata probe and pinned worker initialization source, not a per-run count"},
			"wait_measurement": {"elapsed_ms": null,
				"scope": "native blocking acquire and frame-slot fence waits are inside pre/post and whole-process elapsed, not separately timed by stock script APIs; never derive wait as wall minus all-thread CPU"},
			"cpu_window": {"start": Array(cpu_start), "end": Array(cpu_end),
				"call_brackets_usec": [roi_start_usec, cpu_start_after_usec, now, cpu_end_after_usec],
				"boundary_order": "QPC_then_process_times_then_thread_times",
				"scope": "N post-draw callback intervals; all process threads, script/physics, rendering, native graph lowering/record/submit/present, ring-slot fence wait/retire and timer bookkeeping; steady-state pipeline remains occupied, no forced GPU drain/readback; presentation/engine work differs from Renderer"},
			"timing_attribution": "frame is the callback/route observation index; setup is current draw, viewport CPU/GPU are delayed timestamp observations with no exposed producer frame ID; never add their percentiles or interpret setup+viewport as one-frame CPU",
			"viewport_size": [get_viewport().size.x, get_viewport().size.y],
			"vsync": DisplayServer.window_get_vsync_mode(), "max_fps": Engine.max_fps,
			"roi_wall_ms": float(now - roi_start_usec) / 1000.0, "samples": samples,
			"static_memory_bytes": OS.get_static_memory_usage(),
			"video_memory_bytes": Performance.get_monitor(Performance.RENDER_VIDEO_MEM_USED),
			"texture_memory_bytes": Performance.get_monitor(Performance.RENDER_TEXTURE_MEM_USED),
			"buffer_memory_bytes": Performance.get_monitor(Performance.RENDER_BUFFER_MEM_USED)}
		if config.profile == "b3":
			if not MaterialMips.references_match(b3_materials, material_mip_inventory):
				fail("B3 material references changed after preparation")
				return
			report.material_mips = material_mip_inventory
			report.material_overrides = {"policy": "bistro-source-ao-disabled", "inspected_materials": b3_materials.size(),
				"ao_enabled_before": ao_enabled_before, "ao_enabled_after": report.quality.ambient_occlusion_enabled_materials}
		completed_report = report
		capture.call_deferred()

func capture_image(name: String) -> Image:
	if config.profile != "b3":
		await RenderingServer.frame_post_draw
		return get_viewport().get_texture().get_image()
	# Hold each pinned camera for a fixed settling interval, then require two
	# consecutive exact readbacks. This is outside ROI and never substitutes for
	# cross-process image equality. Fail explicitly if the image does not settle.
	var previous := PackedByteArray()
	for index in range(CAPTURE_MAX_FRAMES):
		await RenderingServer.frame_post_draw
		if index + 1 < CAPTURE_SETTLING_FRAMES:
			continue
		var image := get_viewport().get_texture().get_image()
		if image == null or image.is_empty():
			fail("Empty fixed-view capture: " + name)
			return null
		var pixels := image.get_data()
		if not previous.is_empty() and pixels == previous:
			capture_frames[name] = index + 1
			return image
		previous = pixels
	fail("Fixed-view capture did not become byte-stable: " + name)
	return null

func capture() -> void:
	# Readback and PNG work occur strictly after ROI, at seven pinned cameras.
	set_camera(0)
	var image: Image = await capture_image("route0")
	if image == null or image.is_empty() or image.save_png(config.capture) != OK:
		fail("Capture failed")
		return
	for view in conditions.views:
		camera.position = vec(view.position)
		camera.look_at(vec(view.target), Vector3.UP)
		camera.fov = view.vertical_fov_degrees
		image = await capture_image(view.name)
		if image == null or image.is_empty() or image.save_png(config.capture.get_base_dir().path_join(view.name + ".png")) != OK:
			fail("Named capture failed")
			return
	if config.profile == "b3":
		completed_report.capture_validation = {"settling_frames": CAPTURE_SETTLING_FRAMES,
			"maximum_frames": CAPTURE_MAX_FRAMES, "consecutive_equal_readbacks": 2,
			"outside_roi": true, "frames_by_view": capture_frames}
	var output := FileAccess.open(config.output, FileAccess.WRITE)
	if output == null:
		fail("Cannot write report")
		return
	# Preserve the original parsed double-valued configuration exactly, and only
	# publish a complete report after every requested capture has succeeded.
	output.store_string(JSON.stringify(completed_report, "", true, true))
	output.close()
	print("BISTRO_DONE ", config.output)
	get_tree().quit()
