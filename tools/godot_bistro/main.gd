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
var inventory := {"mesh_instances": 0, "surfaces": 0, "triangles": 0}
var import_inventory: Dictionary
var finished := false

func fail(message: String) -> void:
	push_error(message)
	get_tree().quit(1)

func vec(values: Array) -> Vector3:
	return Vector3(values[0], values[1], values[2])

func _ready() -> void:
	var args := OS.get_cmdline_user_args()
	if args.size() != 1:
		fail("Expected one absolute configuration JSON path")
		return
	config = JSON.parse_string(FileAccess.get_file_as_string(args[0]))
	conditions = JSON.parse_string(FileAccess.get_file_as_string(config.conditions))
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
	inspect_scene(scene)
	if inventory != {"mesh_instances": 1296, "surfaces": 1591, "triangles": 2832120}:
		fail("Bistro inventory differs: %s" % inventory)
		return
	var world := WorldEnvironment.new()
	var environment := Environment.new()
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
	world.environment = environment
	add_child(world)
	var sun := DirectionalLight3D.new()
	sun.shadow_enabled = false
	sun.light_energy = 1.0
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
		instance.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
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

func after_draw() -> void:
	if finished:
		return
	var now := Time.get_ticks_usec()
	var frame := tick - int(config.warmup)
	if frame == -1:
		roi_start_usec = now
	if frame >= 0 and frame < int(config.frames):
		var viewport := get_viewport().get_viewport_rid()
		var sample := {"frame": frame, "frame_interval_ms": float(now - previous_post_usec) / 1000.0}
		if config.timing:
			sample.cpu_setup_ms = RenderingServer.get_frame_setup_time_cpu()
			sample.cpu_viewport_ms = RenderingServer.viewport_get_measured_render_time_cpu(viewport)
			sample.cpu_render_ms = sample.cpu_setup_ms + sample.cpu_viewport_ms
			sample.gpu_ms = RenderingServer.viewport_get_measured_render_time_gpu(viewport)
			sample.draw_calls = RenderingServer.get_rendering_info(RenderingServer.RENDERING_INFO_TOTAL_DRAW_CALLS_IN_FRAME)
			sample.primitives = RenderingServer.get_rendering_info(RenderingServer.RENDERING_INFO_TOTAL_PRIMITIVES_IN_FRAME)
		samples[frame] = sample
	previous_post_usec = now
	tick += 1
	if frame == int(config.frames) - 1:
		finished = true
		var report := {"schema_version": 1, "config": config, "inventory": inventory,
			"godot": Engine.get_version_info(), "debug": OS.has_feature("debug"),
			"import_inventory": import_inventory,
			"adapter": RenderingServer.get_video_adapter_name(),
			"vendor": RenderingServer.get_video_adapter_vendor(),
			"api_version": RenderingServer.get_video_adapter_api_version(),
			"driver": RenderingServer.get_current_rendering_driver_name(),
			"renderer": RenderingServer.get_current_rendering_method(),
			"viewport_size": [get_viewport().size.x, get_viewport().size.y],
			"vsync": DisplayServer.window_get_vsync_mode(), "max_fps": Engine.max_fps,
			"roi_wall_ms": float(now - roi_start_usec) / 1000.0, "samples": samples,
			"static_memory_bytes": OS.get_static_memory_usage(),
			"video_memory_bytes": Performance.get_monitor(Performance.RENDER_VIDEO_MEM_USED),
			"texture_memory_bytes": Performance.get_monitor(Performance.RENDER_TEXTURE_MEM_USED),
			"buffer_memory_bytes": Performance.get_monitor(Performance.RENDER_BUFFER_MEM_USED)}
		var output := FileAccess.open(config.output, FileAccess.WRITE)
		if output == null:
			fail("Cannot write report")
			return
		output.store_string(JSON.stringify(report))
		output.close()
		capture.call_deferred()

func capture() -> void:
	# Readback and PNG work occur strictly after ROI, at seven pinned cameras.
	set_camera(0)
	await RenderingServer.frame_post_draw
	var image := get_viewport().get_texture().get_image()
	if image == null or image.is_empty() or image.save_png(config.capture) != OK:
		fail("Capture failed")
		return
	for view in conditions.views:
		camera.position = vec(view.position)
		camera.look_at(vec(view.target), Vector3.UP)
		camera.fov = view.vertical_fov_degrees
		await RenderingServer.frame_post_draw
		image = get_viewport().get_texture().get_image()
		if image.save_png(config.capture.get_base_dir().path_join(view.name + ".png")) != OK:
			fail("Named capture failed")
			return
	print("BISTRO_DONE ", config.output)
	get_tree().quit()
