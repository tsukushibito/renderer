# The managed fixtures have deliberately narrow, reviewed ABIs. This
# checks reflected interfaces, not shader behavior or trust of arbitrary input.
# Keep the checker usable by CPU tests without loading an SDK or native library.
function Assert-ManagedComputeEntry([object]$Spirv, [string]$Dxil, [string]$Asset) {
    $entries = @($Spirv.entryPoints)
    if ($entries.Count -ne 1 -or $entries[0].name -cne 'main' -or
        $entries[0].mode -cne 'comp' -or
        (@($entries[0].workgroup_size) -join ',') -cne '1,1,1' -or
        @($entries[0].workgroup_size_is_spec_constant_id | Where-Object { $_ }).Count -ne 0) {
        throw "$Asset SPIR-V entry point or local size differs from the managed ABI."
    }
    if ($Dxil -cnotmatch '(?m)^; Compute Shader\s*$' -or
        $Dxil -cnotmatch '(?m)^; NumThreads=\(1,1,1\)\s*$' -or
        $Dxil -cnotmatch '(?m)^; EntryFunctionName: main\s*$') {
        throw "$Asset DXIL entry point or local size differs from the managed ABI."
    }
}

function Get-DxilResourceRows([string]$Section) {
    # Capture every resource row, including unsupported types, so an extra
    # resource cannot pass just because it is not one of the expected kinds.
    return [regex]::Matches($Section,
        '(?m)^;\s+(?<name>\S+)\s+(?<kind>\S+)\s+(?<format>\S+)\s+(?<dimension>\S+)\s+(?<id>\S+)\s+(?<slot>\S+)\s+(?<count>\d+)\s*$')
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
