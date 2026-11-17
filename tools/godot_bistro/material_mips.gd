extends RefCounted
## B3 material preparation, entirely before warmup/ROI. Native readback verifies
## the uploaded chain; ordinary drawing never scans image bytes or references.

const ROLES := {
	"albedo": BaseMaterial3D.TEXTURE_ALBEDO,
	"normal": BaseMaterial3D.TEXTURE_NORMAL,
	"metallic": BaseMaterial3D.TEXTURE_METALLIC,
	"roughness": BaseMaterial3D.TEXTURE_ROUGHNESS,
	"emission": BaseMaterial3D.TEXTURE_EMISSION,
	"ao": BaseMaterial3D.TEXTURE_AMBIENT_OCCLUSION,
}

static func complete_mip_count(width: int, height: int) -> int:
	var size := maxi(width, height)
	var count := 0
	while size > 1:
		size = maxi(size >> 1, 1)
		count += 1
	return count

static func sha256(data: PackedByteArray) -> String:
	var context := HashingContext.new()
	context.start(HashingContext.HASH_SHA256)
	context.update(data)
	return context.finish().hex_encode()

static func prepare(images: Array[Texture2D], materials: Array[Material]) -> Dictionary:
	var texture_indices: Dictionary = {}
	var uses: Dictionary = {}
	var references: Array = []
	for index in images.size():
		var texture := images[index]
		if not texture is ImageTexture or texture_indices.has(texture.get_instance_id()):
			return {"error": "B3 expects distinct runtime ImageTexture source images"}
		texture_indices[texture.get_instance_id()] = index
		uses[index] = {}
	for index in materials.size():
		if not materials[index] is StandardMaterial3D:
			return {"error": "B3 mip preparation requires StandardMaterial3D imports"}
		var material := materials[index] as StandardMaterial3D
		if material.texture_filter != BaseMaterial3D.TEXTURE_FILTER_LINEAR_WITH_MIPMAPS:
			return {"error": "B3 material must use linear filtering with mipmaps"}
		for role in ROLES:
			var texture := material.get_texture(ROLES[role])
			if texture == null:
				continue
			if not texture_indices.has(texture.get_instance_id()):
				return {"error": "B3 material texture is absent from the glTF image inventory"}
			var image_index: int = texture_indices[texture.get_instance_id()]
			uses[image_index][role] = true
			references.append({"material": index, "role": role, "image": image_index,
				"resource_id": texture.get_instance_id(), "rid": texture.get_rid().get_id()})
	var records: Array = []
	var levels := 0
	var normal_images := 0
	for index in images.size():
		if uses[index].is_empty() or (uses[index].has("normal") and uses[index].size() != 1):
			return {"error": "B3 source image is unused or shared between normal and other semantics"}
		var texture := images[index] as ImageTexture
		var source := texture.get_image()
		if source == null or source.is_empty() or source.is_compressed():
			return {"error": "B3 requires readable uncompressed runtime source images"}
		if source.get_mipmap_count() != 0:
			return {"error": "Pinned runtime PNG import unexpectedly supplied material mips"}
		var base := source.get_data()
		var width := source.get_width()
		var height := source.get_height()
		var format := source.get_format()
		if format != Image.FORMAT_RGB8 and format != Image.FORMAT_RGBA8:
			return {"error": "B3 source PNG readback must be RGB8 or RGBA8"}
		var count := complete_mip_count(width, height)
		var normal: bool = uses[index].has("normal")
		var source_hash := sha256(base)
		var resource_id := texture.get_instance_id()
		var rid := texture.get_rid()
		var error := source.generate_mipmaps(normal)
		if error != OK or source.get_mipmap_count() != count or source.get_data().slice(0, base.size()) != base:
			return {"error": "B3 mip generation failed or changed base image bytes"}
		# set_image replaces native storage while retaining the existing Resource
		# and RID, so every shared material reference observes the same chain.
		# update deliberately rejects changes to the mipmap configuration.
		texture.set_image(source)
		var native := texture.get_image()
		if (native == null or native.is_empty() or native.is_compressed()
				or native.get_width() != width or native.get_height() != height or native.get_format() != format
				or native.get_mipmap_count() != count or native.has_mipmaps() != (count > 0)
				or native.get_data().slice(0, base.size()) != base
				or texture.get_instance_id() != resource_id or texture.get_rid() != rid):
			return {"error": "B3 native material mip chain, base bytes or resource identity differs after upload"}
		var native_hash := sha256(native.get_data().slice(0, base.size()))
		if source_hash != native_hash:
			return {"error": "B3 native base image hash differs"}
		records.append({"index": index, "class": texture.get_class(), "width": width, "height": height,
			"format": format, "compressed": native.is_compressed(), "mipmaps_before": 0,
			"mipmap_count": native.get_mipmap_count(), "has_mipmaps": native.has_mipmaps(),
			"normal_renormalized": normal, "base_bytes": base.size(), "native_data_bytes": native.get_data().size(),
			"base_sha256": source_hash, "native_base_sha256": native_hash, "resource_identity_preserved": true})
		levels += count + 1
		normal_images += int(normal)
	var result := {"policy": "b3-explicit-png-full-mips-v1", "outside_roi": true,
		"verified": true, "image_count": images.size(), "material_count": materials.size(),
		"material_reference_count": references.size(), "normal_images": normal_images,
		"total_levels_including_base": levels, "images": records, "references": references}
	if not references_match(materials, result):
		return {"error": "B3 material references changed during mip preparation"}
	return result

static func references_match(materials: Array[Material], inventory: Dictionary) -> bool:
	for reference in inventory.references:
		var texture := (materials[reference.material] as StandardMaterial3D).get_texture(ROLES[reference.role])
		if (texture == null or texture.get_instance_id() != reference.resource_id
				or texture.get_rid().get_id() != reference.rid):
			return false
	return true
