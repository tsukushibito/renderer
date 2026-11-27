# The managed fixtures have deliberately narrow, reviewed ABIs. This
# checks reflected interfaces, not shader behavior or trust of arbitrary input.
# Keep the checker usable by CPU tests without loading an SDK or native library.
function Assert-ManagedComputeEntry([object]$Spirv, [string]$Dxil, [string]$Asset, [uint32[]]$LocalSize = @(1, 1, 1)) {
    $entries = @($Spirv.entryPoints)
    if ($entries.Count -ne 1 -or $entries[0].name -cne 'main' -or
        $entries[0].mode -cne 'comp' -or
        (@($entries[0].workgroup_size) -join ',') -cne ($LocalSize -join ',') -or
        @($entries[0].workgroup_size_is_spec_constant_id | Where-Object { $_ }).Count -ne 0) {
        throw "$Asset SPIR-V entry point or local size differs from the managed ABI."
    }
    if ($Dxil -cnotmatch '(?m)^; Compute Shader\s*$' -or
        $Dxil -cnotmatch ('(?m)^; NumThreads=\(' + [regex]::Escape(($LocalSize -join ',')) + '\)\s*$') -or
        $Dxil -cnotmatch '(?m)^; EntryFunctionName: main\s*$') {
        throw "$Asset DXIL entry point or local size differs from the managed ABI."
    }
}

# Caller-owned HDR fixtures exercise typed storage images in the ordinary Graph
# path. This verifies the declared ABI; their sources and unsafe Program import
# separately establish bounds, invocation ownership and full-view writes.
function Assert-HdrComputeAbi([object]$Spirv, [string]$Dxil, [switch]$StorageInput) {
    $asset = if ($StorageInput) { 'compute_hdr_storage.comp' } else { 'compute_hdr_sampled.comp' }
    Assert-ManagedComputeEntry $Spirv $Dxil $asset @(4, 4, 1)
    $categories = @('ubos', 'ssbos', 'separate_images', 'separate_samplers', 'images')
    $expectedCounts = if ($StorageInput) { @(0, 1, 0, 0, 2) } else { @(1, 0, 1, 1, 1) }
    for ($index = 0; $index -lt $categories.Count; $index++) {
        $property = $Spirv.PSObject.Properties[$categories[$index]]
        $resources = @(if ($null -ne $property) { $property.Value })
        if ($resources.Count -ne $expectedCounts[$index]) {
            throw "$asset resource count differs for $($categories[$index])."
        }
    }
    foreach ($category in @('textures', 'sampled_images', 'subpass_inputs', 'acceleration_structures', 'push_constants')) {
        $property = $Spirv.PSObject.Properties[$category]
        if ($null -ne $property -and @($property.Value).Count -ne 0) {
            throw "$asset has unexpected resource category $category."
        }
    }
    foreach ($image in @($Spirv.images)) {
        $expectedQualifier = if ($image.set -eq 0 -and $StorageInput) { 'readonly' } else { 'writeonly' }
        $qualifier = $image.PSObject.Properties[$expectedQualifier]
        if ($image.type -cne 'image2D' -or $image.format -cne 'rgba16f' -or $image.binding -ne 0 -or
            $image.set -notin @(0, 1) -or ($image.set -eq 0 -and -not $StorageInput) -or
            $null -eq $qualifier -or $qualifier.Value -ne $true -or
            $null -ne $image.PSObject.Properties['array']) {
            throw "$asset storage image slot, type, format, count or mutability differs."
        }
    }
    if (@($Spirv.images | Where-Object { $_.set -eq 1 }).Count -ne 1 -or
        ($StorageInput -and @($Spirv.images | Where-Object { $_.set -eq 0 }).Count -ne 1)) {
        throw "$asset requires exactly one storage image at each declared slot."
    }
    $buffer = if ($StorageInput) { $Spirv.ssbos[0] } else { $Spirv.ubos[0] }
    $bufferSlot = if ($StorageInput) { 1 } else { 2 }
    if ($buffer.set -ne 0 -or $buffer.binding -ne $bufferSlot -or $buffer.block_size -ne 16 -or
        $null -ne $buffer.PSObject.Properties['array']) {
        throw "$asset buffer slot, byte size or array count differs."
    }
    if ($StorageInput -and ($null -eq $buffer.PSObject.Properties['readonly'] -or $buffer.readonly -ne $true)) {
        throw "$asset buffer input must be read-only."
    }
    $members = @($Spirv.types.PSObject.Properties[$buffer.type].Value.members)
    if ($members.Count -ne 1 -or $members[0].type -cne 'vec4' -or $members[0].offset -ne 0 -or
        $null -ne $members[0].PSObject.Properties['array']) {
        throw "$asset buffer must contain one vec4 at byte zero."
    }
    if (-not $StorageInput) {
        foreach ($expected in @(@('separate_images', 'texture2D', 0), @('separate_samplers', 'sampler', 1))) {
            $resource = @($Spirv.PSObject.Properties[$expected[0]].Value)[0]
            if ($resource.type -cne $expected[1] -or $resource.set -ne 0 -or $resource.binding -ne $expected[2] -or
                $null -ne $resource.PSObject.Properties['array']) {
                throw "$asset sampled image or sampler ABI differs."
            }
        }
    }
    $sections = @($Dxil.Replace("`r`n", "`n") -split '; Resource Bindings:')
    if ($sections.Count -ne 2) { throw "$asset DXIL resource reflection is missing." }
    $rows = @(Get-DxilResourceRows $sections[1])
    # Default SPIRV-Cross maps even readonly storage images to UAVs. Do not
    # silently change a readonly image to the raw buffer/SRV binding path.
    $expectedRows = if ($StorageInput) {
        @(@('UAV', 'f32', '2d', '^u0(?:,space0)?$'), @('texture', 'byte', 'r/o', '^t1(?:,space0)?$'), @('UAV', 'f32', '2d', '^u0,space1$'))
    } else {
        @(@('texture', 'f32', '2d', '^t0(?:,space0)?$'), @('sampler', 'NA', 'NA', '^s1(?:,space0)?$'),
            @('cbuffer', 'NA', 'NA', '^cb2(?:,space0)?$'), @('UAV', 'f32', '2d', '^u0,space1$'))
    }
    if ($rows.Count -ne $expectedRows.Count -or @($rows | Where-Object { $_.Groups['count'].Value -cne '1' }).Count -ne 0) {
        throw "$asset DXIL resource count differs."
    }
    foreach ($expected in $expectedRows) {
        $matching = @($rows | Where-Object {
            $_.Groups['kind'].Value -ceq $expected[0] -and $_.Groups['format'].Value -ceq $expected[1] -and
            $_.Groups['dimension'].Value -ceq $expected[2] -and $_.Groups['slot'].Value -cmatch $expected[3]
        })
        if ($matching.Count -ne 1) { throw "$asset DXIL resource type, register or space differs." }
    }
    if (-not $StorageInput) {
        $uniform = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'cbuffer' })[0]
        $name = [regex]::Escape($uniform.Groups['name'].Value)
        $body = [regex]::Match($sections[0], ('(?s); cbuffer ' + $name + '\s*\n; \{(?<body>.*?)\n; \}'))
        $members = @($body.Groups['body'].Value -split '\n' | Where-Object { $_ -match 'Offset:' -and $_ -notmatch 'Size:' })
        if (-not $body.Success -or $members.Count -ne 1 -or
            $members[0] -cnotmatch '^;\s+float4\s+\w+;\s*;\s*Offset:\s*0\s*$' -or
            $body.Groups['body'].Value -cnotmatch ('(?m)^;\s+\}\s+' + $name + ';\s*;\s*Offset:\s*0\s+Size:\s*16\s*$')) {
            throw "$asset DXIL Uniform layout differs."
        }
    }
}

function Assert-IndexedIndirectAbi([object]$Spirv, [string]$Dxil) {
    $asset = 'indexed_indirect.comp'
    Assert-ManagedComputeEntry $Spirv $Dxil $asset @(64, 1, 1)
    foreach ($category in @('ubos', 'textures', 'separate_images', 'separate_samplers', 'images',
            'sampled_images', 'subpass_inputs', 'acceleration_structures', 'push_constants')) {
        $property = $Spirv.PSObject.Properties[$category]
        if ($null -ne $property -and @($property.Value).Count -ne 0) {
            throw "$asset has unexpected resource category $category."
        }
    }

    $buffers = @($Spirv.ssbos | Sort-Object binding)
    if ($buffers.Count -ne 3) { throw "$asset requires three storage buffers." }
    $strides = @(16, 4, 20)
    for ($slot = 0; $slot -lt $buffers.Count; $slot++) {
        $buffer = $buffers[$slot]
        $usage = if ($slot -eq 2) { 'writeonly' } else { 'readonly' }
        $qualifier = $buffer.PSObject.Properties[$usage]
        if ($buffer.set -ne 0 -or $buffer.binding -ne $slot -or $buffer.block_size -ne 0 -or
            $null -ne $buffer.PSObject.Properties['array'] -or $null -eq $qualifier -or -not $qualifier.Value) {
            throw "$asset SPIR-V storage slot $slot has the wrong binding, array shape or mutability."
        }
        $members = @($Spirv.types.PSObject.Properties[$buffer.type].Value.members)
        if ($members.Count -ne 1) { throw "$asset storage slot $slot must have one runtime array." }
        $member = $members[0]
        if ($member.offset -ne 0 -or $member.array_stride -ne $strides[$slot] -or
            (@($member.array) -join ',') -cne '0' -or
            (@($member.array_size_is_literal) -join ',') -cne 'True') {
            throw "$asset storage slot $slot has the wrong entry offset, stride or array shape."
        }
        if ($slot -lt 2) {
            $inputType = if ($slot -eq 0) { 'uvec4' } else { 'uint' }
            if ($member.type -cne $inputType) { throw "$asset storage slot $slot has the wrong entry type." }
        } else {
            $fields = @($Spirv.types.PSObject.Properties[$member.type].Value.members)
            if ((@($fields.type) -join ',') -cne 'uint,uint,uint,int,uint' -or
                (@($fields.offset) -join ',') -cne '0,4,8,12,16' -or
                @($fields | Where-Object { $null -ne $_.PSObject.Properties['array'] }).Count -ne 0) {
                throw "$asset indexed arguments do not have the native five-word layout."
            }
        }
    }

    $sections = @($Dxil -csplit '; Resource Bindings:')
    if ($sections.Count -ne 2) { throw "$asset DXIL resource reflection is missing." }
    $rows = @(Get-DxilResourceRows $sections[1])
    if ($rows.Count -ne 3) { throw "$asset DXIL resource count differs." }
    foreach ($expected in @(@('texture', 'r/o', '^t0(?:,space0)?$'),
            @('texture', 'r/o', '^t1(?:,space0)?$'), @('UAV', 'r/w', '^u2(?:,space0)?$'))) {
        $matching = @($rows | Where-Object {
            $_.Groups['kind'].Value -ceq $expected[0] -and $_.Groups['format'].Value -ceq 'byte' -and
            $_.Groups['dimension'].Value -ceq $expected[1] -and $_.Groups['slot'].Value -cmatch $expected[2] -and
            $_.Groups['count'].Value -ceq '1'
        })
        if ($matching.Count -ne 1) { throw "$asset DXIL raw buffer type, register or space differs." }
    }
}

function Get-DxilResourceRows([string]$Section) {
    # Capture every resource row, including unsupported types, so an extra
    # resource cannot pass just because it is not one of the expected kinds.
    return [regex]::Matches($Section,
        '(?m)^;[ \t]+(?<name>\S+)[ \t]+(?<kind>\S+)[ \t]+(?<format>\S+)[ \t]+(?<dimension>\S+)[ \t]+(?<id>\S+)[ \t]+(?<slot>\S+)[ \t]+(?<count>\d+)[ \t]*\r?$')
}

function Assert-RasterDxilSignature([string]$Dxil, [string]$Heading, [object[]]$Expected) {
    $section = [regex]::Match($Dxil.Replace("`r`n", "`n"),
        ('(?m)^; ' + $Heading + ' signature:\n;\n; Name[^\n]*\n; -+[^\n]*\n(?<body>(?:;[^\n]+\n)+)'))
    if (-not $section.Success) { throw "Raster DXIL $Heading signature is missing." }
    $lines = @($section.Groups['body'].Value.TrimEnd("`n") -split "`n")
    if ($lines.Count -ne $Expected.Count) { throw "Raster DXIL $Heading signature count differs." }
    for ($index = 0; $index -lt $Expected.Count; $index++) {
        $row = [regex]::Match($lines[$index],
            '^;[ \t]+(?<name>\S+)[ \t]+(?<index>\d+)[ \t]+(?<mask>\S+)[ \t]+\d+[ \t]+(?<system>\S+)[ \t]+(?<format>\S+)(?:[ \t]+\S+)?[ \t]*$')
        if (-not $row.Success -or $row.Groups['name'].Value -cne $Expected[$index][0] -or
            $row.Groups['index'].Value -cne $Expected[$index][1] -or
            $row.Groups['mask'].Value -cne $Expected[$index][2] -or
            $row.Groups['system'].Value -cne $Expected[$index][3] -or
            $row.Groups['format'].Value -cne 'float') {
            throw "Raster DXIL $Heading signature semantic, type or component count differs."
        }
    }
}

function Assert-BufferCopyAbi([object]$Spirv, [string]$Dxil, [switch]$StorageInput) {
    $asset = if ($StorageInput) { 'storage_read.comp' } else { 'uniform.comp' }
    Assert-ManagedComputeEntry $Spirv $Dxil $asset
    foreach ($category in @('textures', 'separate_images', 'separate_samplers', 'images',
            'sampled_images', 'subpass_inputs', 'acceleration_structures', 'push_constants')) {
        $property = $Spirv.PSObject.Properties[$category]
        if ($null -ne $property -and @($property.Value).Count -ne 0) {
            throw "$asset has unexpected SPIR-V resources: $category"
        }
    }
    $ubos = @()
    $uniformProperty = $Spirv.PSObject.Properties['ubos']
    if ($null -ne $uniformProperty) { $ubos = @($uniformProperty.Value) }
    $ssbos = @($Spirv.ssbos)
    if ($StorageInput) {
        $inputs = @($ssbos | Where-Object { $_.set -eq 0 })
        $outputs = @($ssbos | Where-Object { $_.set -eq 1 })
        if ($ubos.Count -ne 0 -or $ssbos.Count -ne 2 -or
            $inputs.Count -ne 1 -or $outputs.Count -ne 1) {
            throw 'storage_read.comp must reflect one storage input and one storage output.'
        }
        $inputResource = $inputs[0]
        $outputResource = $outputs[0]
        $readonly = $inputResource.PSObject.Properties['readonly']
        if ($null -eq $readonly -or $readonly.Value -ne $true) {
            throw 'storage_read.comp input must be read-only.'
        }
    } else {
        if ($ubos.Count -ne 1 -or $ssbos.Count -ne 1) {
            throw 'uniform.comp must reflect exactly one uniform and one storage buffer.'
        }
        $inputResource = $ubos[0]
        $outputResource = $ssbos[0]
    }
    foreach ($item in @(@($inputResource, 0), @($outputResource, 1))) {
        $resource = $item[0]
        $set = $item[1]
        $array = $resource.PSObject.Properties['array']
        if ($resource.set -ne $set -or $resource.binding -ne 0 -or
            $resource.block_size -ne 16 -or $null -ne $array) {
            throw "$asset SPIR-V set $set buffer binding, count or byte size differs."
        }
        $type = $Spirv.types.PSObject.Properties[$resource.type]
        if ($null -eq $type) { throw "$asset reflected buffer type is missing." }
        $members = @($type.Value.members)
        if ($members.Count -ne 1 -or $members[0].type -cne 'uvec4' -or
            $members[0].offset -ne 0 -or $null -ne $members[0].PSObject.Properties['array']) {
            throw "$asset SPIR-V set $set must contain one uvec4 at byte zero."
        }
    }
    $writeonly = $outputResource.PSObject.Properties['writeonly']
    if ($null -eq $writeonly -or $writeonly.Value -ne $true) {
        throw "$asset output must be write-only."
    }
    $sections = @($Dxil -split '; Resource Bindings:')
    if ($sections.Count -ne 2) { throw "$asset DXIL resource reflection is missing." }
    $rows = @(Get-DxilResourceRows $sections[1])
    if ($rows.Count -ne 2) { throw "$asset DXIL must bind exactly two resources." }
    $inputKind = if ($StorageInput) { 'texture' } else { 'cbuffer' }
    $inputs = @($rows | Where-Object { $_.Groups['kind'].Value -ceq $inputKind })
    $output = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'UAV' })
    if ($inputs.Count -ne 1 -or $output.Count -ne 1 -or
        $inputs[0].Groups['count'].Value -cne '1' -or
        $output[0].Groups['slot'].Value -cne 'u0,space1' -or
        $output[0].Groups['count'].Value -cne '1' -or
        $output[0].Groups['format'].Value -cne 'byte' -or
        $output[0].Groups['dimension'].Value -cne 'r/w') {
        throw "$asset DXIL requires exactly one input and one u0/space1 raw UAV."
    }
    if ($StorageInput) {
        if ($inputs[0].Groups['slot'].Value -cnotmatch '^t0(?:,space0)?$' -or
            $inputs[0].Groups['format'].Value -cne 'byte' -or
            $inputs[0].Groups['dimension'].Value -cne 'r/o') {
            throw 'storage_read.comp DXIL requires one t0/space0 raw read-only SRV.'
        }
        return
    }
    if ($inputs[0].Groups['slot'].Value -cnotmatch '^cb0(?:,space0)?$') {
        throw 'uniform.comp DXIL requires one b0/space0 cbuffer.'
    }
    $name = [regex]::Escape($inputs[0].Groups['name'].Value)
    $body = [regex]::Match($sections[0],
        ('(?s); cbuffer ' + $name + '\s*\n; \{(?<body>.*?)\n; \}'))
    if (-not $body.Success) { throw 'uniform.comp DXIL cbuffer definition is missing.' }
    $members = @($body.Groups['body'].Value -split '\n' |
        Where-Object { $_ -match 'Offset:' -and $_ -notmatch 'Size:' })
    if ($members.Count -ne 1 -or
        $members[0] -cnotmatch '^;\s+uint4\s+\w+;\s*;\s*Offset:\s*0\s*$' -or
        $body.Groups['body'].Value -cnotmatch ('(?m)^;\s+\}\s+' + $name + ';\s*;\s*Offset:\s*0\s+Size:\s*16\s*$')) {
        throw 'uniform.comp DXIL must contain one 16-byte uint4 at byte zero.'
    }
}

function Assert-SampledAbi([object]$Spirv, [string]$Dxil, [switch]$FloatOutput) {
    $asset = if ($FloatOutput) { 'sampled_float.comp' } else { 'sampled.comp' }
    $outputBytes = if ($FloatOutput) { 64 } else { 16 }
    Assert-ManagedComputeEntry $Spirv $Dxil $asset
    foreach ($category in @('textures', 'images', 'ubos', 'sampled_images',
            'subpass_inputs', 'acceleration_structures', 'push_constants')) {
        $property = $Spirv.PSObject.Properties[$category]
        if ($null -ne $property -and @($property.Value).Count -ne 0) {
            throw "$asset has unexpected SPIR-V resources: $category"
        }
    }
    foreach ($expected in @(@('separate_images', 'texture2D', 0), @('separate_samplers', 'sampler', 1))) {
        $resources = @($Spirv.PSObject.Properties[$expected[0]].Value)
        if ($resources.Count -ne 1 -or $resources[0].type -cne $expected[1] -or
            $resources[0].set -ne 0 -or $resources[0].binding -ne $expected[2] -or
            $null -ne $resources[0].PSObject.Properties['array']) {
            throw "$asset requires one separate $($expected[1]) at set 0 binding $($expected[2])."
        }
    }
    $outputs = @($Spirv.ssbos)
    if ($outputs.Count -ne 1) { throw "$asset must have exactly one storage output." }
    $output = $outputs[0]
    if ($output.set -ne 1 -or $output.binding -ne 0 -or $output.block_size -ne $outputBytes -or
        $output.writeonly -ne $true -or $null -ne $output.PSObject.Properties['array']) {
        throw "$asset requires one write-only $outputBytes-byte storage output at set 1 binding 0."
    }
    $members = @($Spirv.types.PSObject.Properties[$output.type].Value.members)
    if ($members.Count -ne 1 -or $members[0].type -cne 'uvec4' -or
        $members[0].offset -ne 0) {
        throw "$asset output must contain a uvec4 member at byte zero."
    }
    if ($FloatOutput) {
        if ((@($members[0].array) -join ',') -cne '4' -or
            (@($members[0].array_size_is_literal) -join ',') -cne 'True' -or
            $members[0].array_stride -ne 16) {
            throw "$asset output must contain four literal uvec4 elements at 16-byte stride."
        }
    } elseif ($null -ne $members[0].PSObject.Properties['array']) {
        throw 'sampled.comp output must contain one non-array uvec4.'
    }
    $sections = @($Dxil -split '; Resource Bindings:')
    if ($sections.Count -ne 2) { throw "$asset DXIL resource reflection is missing." }
    $rows = @(Get-DxilResourceRows $sections[1])
    if ($rows.Count -ne 3) { throw "$asset DXIL must bind exactly three active resources." }
    $images = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'texture' })
    $samplers = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'sampler' })
    $outputs = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'UAV' })
    if ($images.Count -ne 1 -or $samplers.Count -ne 1 -or $outputs.Count -ne 1 -or
        $images[0].Groups['slot'].Value -cnotmatch '^t0(?:,space0)?$' -or
        $images[0].Groups['format'].Value -cne 'f32' -or
        $images[0].Groups['dimension'].Value -cne '2d' -or
        $samplers[0].Groups['slot'].Value -cnotmatch '^s1(?:,space0)?$' -or
        $outputs[0].Groups['slot'].Value -cne 'u0,space1' -or
        $outputs[0].Groups['format'].Value -cne 'byte' -or
        $outputs[0].Groups['dimension'].Value -cne 'r/w' -or
        @($rows | Where-Object { $_.Groups['count'].Value -cne '1' }).Count -ne 0) {
        throw "$asset DXIL requires t0/space0 Texture2D<float4>, active s1/space0 sampler and u0/space1 raw UAV."
    }
}

# Descriptor visibility is established separately for the actual vertex and
# fragment modules. No reflection record can promote arbitrary shader code to
# the reviewed read-only behavior of these checked-in sources.
function Assert-TexturedRasterAbi([object]$Spirv, [string]$Dxil, [switch]$Vertex) {
    $asset = if ($Vertex) { 'textured.vert' } else { 'textured.frag' }
    $mode = if ($Vertex) { 'vert' } else { 'frag' }
    $stage = if ($Vertex) { 'Vertex' } else { 'Pixel' }
    $entries = @($Spirv.entryPoints)
    if ($entries.Count -ne 1 -or $entries[0].name -cne 'main' -or
        $entries[0].mode -cne $mode -or
        $Dxil -cnotmatch ('(?m)^; ' + $stage + ' Shader\s*$') -or
        $Dxil -cnotmatch '(?m)^; EntryFunctionName: main\s*$') {
        throw "$asset shader stage or entry point differs from its reviewed interface."
    }
    $forbidden = @('textures', 'images', 'ssbos', 'sampled_images', 'subpass_inputs', 'acceleration_structures')
    $forbidden += if ($Vertex) { @('separate_images', 'separate_samplers') } else { @('ubos', 'push_constants') }
    foreach ($category in $forbidden) {
        $property = $Spirv.PSObject.Properties[$category]
        if ($null -ne $property -and @($property.Value).Count -ne 0) {
            throw "$asset has unexpected resources: $category"
        }
    }
    $sections = @($Dxil.Replace("`r`n", "`n") -split '; Resource Bindings:')
    if ($sections.Count -ne 2) { throw "$asset DXIL resource reflection is missing." }
    $rows = @(Get-DxilResourceRows $sections[1])
    if ($rows.Count -ne 2 -or @($rows | Where-Object { $_.Groups['count'].Value -cne '1' }).Count -ne 0) {
        throw "$asset must bind exactly two non-array native resources."
    }
    if ($Vertex) {
        Assert-RasterDxilSignature $Dxil 'Input' @(@('TEXCOORD', '0', 'xyz', 'NONE'), @('TEXCOORD', '1', 'xyzw', 'NONE'))
        Assert-RasterDxilSignature $Dxil 'Output' @(@('TEXCOORD', '0', 'xyzw', 'NONE'), @('TEXCOORD', '1', 'xy', 'NONE'), @('SV_Position', '0', 'xyzw', 'POS'))
    } else {
        Assert-RasterDxilSignature $Dxil 'Input' @(@('TEXCOORD', '0', 'xyzw', 'NONE'), @('TEXCOORD', '1', 'xy', 'NONE'))
        Assert-RasterDxilSignature $Dxil 'Output' @(,@('SV_Target', '0', 'xyzw', 'TARGET'))
    }
    $input = @($Spirv.inputs | Sort-Object location)
    $output = @($Spirv.outputs | Sort-Object location)
    $expectedInput = if ($Vertex) { @('vec3', 'vec4') } else { @('vec4', 'vec2') }
    $expectedOutput = if ($Vertex) { @('vec4', 'vec2') } else { @('vec4') }
    foreach ($item in @(@($input, $expectedInput), @($output, $expectedOutput))) {
        $actual = @($item[0])
        $expected = @($item[1])
        if ($actual.Count -ne $expected.Count) { throw "$asset varying or attribute count differs." }
        for ($index = 0; $index -lt $actual.Count; $index++) {
            if ($actual[$index].type -cne $expected[$index] -or $actual[$index].location -ne $index -or
                $null -ne $actual[$index].PSObject.Properties['array']) {
                throw "$asset varying or attribute layout differs."
            }
        }
    }
    if ($Vertex) {
        $ubos = @($Spirv.ubos)
        $push = @($Spirv.push_constants)
        if ($ubos.Count -ne 1 -or $ubos[0].set -ne 0 -or $ubos[0].binding -ne 0 -or
            $ubos[0].block_size -ne 16 -or $null -ne $ubos[0].PSObject.Properties['array'] -or
            $push.Count -ne 1) {
            throw 'textured.vert must read one 16-byte Uniform at group 0/binding 0 and one inline block.'
        }
        $uniformMembers = @($Spirv.types.PSObject.Properties[$ubos[0].type].Value.members)
        $pushMembers = @($Spirv.types.PSObject.Properties[$push[0].type].Value.members)
        if ($uniformMembers.Count -ne 1 -or $uniformMembers[0].type -cne 'vec4' -or
            $uniformMembers[0].offset -ne 0 -or $null -ne $uniformMembers[0].PSObject.Properties['array'] -or
            $pushMembers.Count -ne 2 -or $pushMembers[0].type -cne 'mat4' -or
            $pushMembers[0].offset -ne 0 -or $pushMembers[0].matrix_stride -ne 16 -or
            $pushMembers[1].type -cne 'vec4' -or $pushMembers[1].offset -ne 64 -or
            ($null -ne $pushMembers[0].PSObject.Properties['row_major'] -and $pushMembers[0].row_major -eq $true) -or
            $null -ne $pushMembers[0].PSObject.Properties['array'] -or $null -ne $pushMembers[1].PSObject.Properties['array']) {
            throw 'textured.vert Uniform prefix or inline matrix/tint offsets differ.'
        }
        foreach ($slot in @(@('cb0', 16), @('cb1', 80))) {
            $buffers = @($rows | Where-Object {
                $_.Groups['kind'].Value -ceq 'cbuffer' -and
                $_.Groups['slot'].Value -cmatch ('^' + $slot[0] + '(?:,space0)?$')
            })
            if ($buffers.Count -ne 1) { throw "textured.vert requires $($slot[0])/space0." }
            $name = [regex]::Escape($buffers[0].Groups['name'].Value)
            $body = [regex]::Match($sections[0], ('(?s); cbuffer ' + $name + '\s*\n; \{(?<body>.*?)\n; \}'))
            if (-not $body.Success -or
                $body.Groups['body'].Value -cnotmatch ('(?m)^;\s+\}\s+' + $name + ';\s*;\s*Offset:\s*0\s+Size:\s*' + $slot[1] + '\s*$')) {
                throw 'textured.vert DXIL cbuffer byte size differs.'
            }
            $members = @($body.Groups['body'].Value -split '\n' | Where-Object { $_ -match 'Offset:' -and $_ -notmatch 'Size:' })
            if ($slot[1] -eq 16) {
                if ($members.Count -ne 1 -or $members[0] -cnotmatch '^;\s+float4\s+\w+;\s*;\s*Offset:\s*0\s*$') {
                    throw 'textured.vert DXIL Uniform must contain one float4 at byte zero.'
                }
            } elseif ($members.Count -ne 2 -or
                $members[0] -cnotmatch '^;\s+row_major float4x4\s+\w+;\s*;\s*Offset:\s*0\s*$' -or
                $members[1] -cnotmatch '^;\s+float4\s+\w+;\s*;\s*Offset:\s*64\s*$') {
                throw 'textured.vert DXIL inline matrix/tint layout differs.'
            }
        }
    } else {
        foreach ($expected in @(@('separate_images', 'texture2D', 0), @('separate_samplers', 'sampler', 1))) {
            $resources = @($Spirv.PSObject.Properties[$expected[0]].Value)
            if ($resources.Count -ne 1 -or $resources[0].type -cne $expected[1] -or
                $resources[0].set -ne 1 -or $resources[0].binding -ne $expected[2] -or
                $null -ne $resources[0].PSObject.Properties['array']) {
                throw 'textured.frag must bind one separate float 2D image and sampler in group 1.'
            }
        }
        $images = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'texture' })
        $samplers = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'sampler' })
        if ($images.Count -ne 1 -or $samplers.Count -ne 1 -or
            $images[0].Groups['slot'].Value -cne 't0,space1' -or
            $images[0].Groups['format'].Value -cne 'f32' -or $images[0].Groups['dimension'].Value -cne '2d' -or
            $samplers[0].Groups['slot'].Value -cne 's1,space1') {
            throw 'textured.frag DXIL must retain the active float texture and separate sampler in space 1.'
        }
    }
}

# All standard SDR fragments sample one source mip and write one color. Only
# Neutral takes a float exposure word, mapped to a DXIL root constant.
function Assert-SdrOutputAbi([object]$Spirv, [string]$Dxil, [switch]$Exposure) {
    $entries = @($Spirv.entryPoints)
    if ($entries.Count -ne 1 -or $entries[0].name -cne 'main' -or $entries[0].mode -cne 'frag' -or
        $Dxil -cnotmatch '(?m)^; Pixel Shader\s*$' -or
        $Dxil -cnotmatch '(?m)^; EntryFunctionName: main\s*$') {
        throw 'Standard output shader stage or entry point differs.'
    }
    $forbidden = @('inputs', 'textures', 'images', 'ssbos', 'ubos', 'sampled_images',
        'subpass_inputs', 'acceleration_structures')
    if (-not $Exposure) { $forbidden += 'push_constants' }
    foreach ($category in $forbidden) {
        $property = $Spirv.PSObject.Properties[$category]
        if ($null -ne $property -and @($property.Value).Count -ne 0) {
            throw "Standard output shader has unexpected resources or varyings: $category"
        }
    }
    $outputs = @($Spirv.outputs)
    if ($outputs.Count -ne 1 -or $outputs[0].type -cne 'vec4' -or $outputs[0].location -ne 0 -or
        $null -ne $outputs[0].PSObject.Properties['array']) {
        throw 'Standard output shader requires one float4 color output at location zero.'
    }
    foreach ($expected in @(@('separate_images', 'texture2D', 0), @('separate_samplers', 'sampler', 1))) {
        $slots = @($Spirv.PSObject.Properties[$expected[0]].Value)
        if ($slots.Count -ne 1 -or $slots[0].type -cne $expected[1] -or
            $slots[0].set -ne 0 -or $slots[0].binding -ne $expected[2] -or
            $null -ne $slots[0].PSObject.Properties['array']) {
            throw 'Standard output shader requires separate float 2D image and sampler slots.'
        }
    }
    Assert-RasterDxilSignature $Dxil 'Input' @(,@('SV_Position', '0', 'xyzw', 'POS'))
    Assert-RasterDxilSignature $Dxil 'Output' @(,@('SV_Target', '0', 'xyzw', 'TARGET'))
    $sections = @($Dxil.Replace("`r`n", "`n") -split '; Resource Bindings:')
    if ($sections.Count -ne 2) { throw 'Standard output DXIL resource reflection is missing.' }
    $rows = @(Get-DxilResourceRows $sections[1])
    if ($rows.Count -ne (2 + [int][bool]$Exposure) -or
        @($rows | Where-Object { $_.Groups['count'].Value -cne '1' }).Count -ne 0) {
        throw 'Standard output DXIL resource count differs.'
    }
    $textures = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'texture' })
    $samplers = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'sampler' })
    if ($textures.Count -ne 1 -or $samplers.Count -ne 1 -or
        $textures[0].Groups['slot'].Value -cnotmatch '^t0(?:,space0)?$' -or
        $textures[0].Groups['format'].Value -cne 'f32' -or
        $textures[0].Groups['dimension'].Value -cne '2d' -or
        $samplers[0].Groups['slot'].Value -cnotmatch '^s1(?:,space0)?$') {
        throw 'Standard output DXIL texture or sampler slot differs.'
    }
    if ($Exposure) {
        $push = @($Spirv.push_constants)
        if ($push.Count -ne 1) { throw 'Neutral requires one SPIR-V push constant block.' }
        $members = @($Spirv.types.PSObject.Properties[$push[0].type].Value.members)
        if ($members.Count -ne 1 -or $members[0].type -cne 'float' -or $members[0].offset -ne 0 -or
            $null -ne $members[0].PSObject.Properties['array']) {
            throw 'Neutral requires one float exposure word at byte zero.'
        }
        $constants = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'cbuffer' })
        if ($constants.Count -ne 1 -or $constants[0].Groups['slot'].Value -cnotmatch '^cb0(?:,space0)?$') {
            throw 'Neutral DXIL exposure register differs.'
        }
        $name = [regex]::Escape($constants[0].Groups['name'].Value)
        $body = [regex]::Match($sections[0], ('(?s); cbuffer ' + $name + '\s*\n; \{(?<body>.*?)\n; \}'))
        $layout = $body.Groups['body'].Value
        if (-not $body.Success -or $layout -cnotmatch '(?m)^;\s+float\s+\w+;\s*;\s*Offset:\s*0\s*$' -or
            $layout -cnotmatch ('(?m)^;\s+\}\s+' + $name + ';\s*;\s*Offset:\s*0\s+Size:\s*4\s*$')) {
            throw 'Neutral DXIL exposure word layout differs.'
        }
    }
}
