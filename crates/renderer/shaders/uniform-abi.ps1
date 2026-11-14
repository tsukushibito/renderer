# The managed uniform fixture has one deliberately narrow, reviewed ABI. This
# checks reflected interfaces, not shader behavior or trust of arbitrary input.
# Keep the checker usable by CPU tests without loading an SDK or native library.
function Assert-UniformAbi([object]$Spirv, [string]$Dxil) {
    $entries = @($Spirv.entryPoints)
    if ($entries.Count -ne 1 -or $entries[0].name -cne 'main' -or
        $entries[0].mode -cne 'comp' -or
        (@($entries[0].workgroup_size) -join ',') -cne '1,1,1' -or
        @($entries[0].workgroup_size_is_spec_constant_id | Where-Object { $_ }).Count -ne 0) {
        throw 'uniform.comp SPIR-V entry point or local size differs from the managed ABI.'
    }
    foreach ($category in @('textures', 'separate_images', 'separate_samplers', 'images',
            'sampled_images', 'subpass_inputs', 'acceleration_structures', 'push_constants')) {
        $property = $Spirv.PSObject.Properties[$category]
        if ($null -ne $property -and @($property.Value).Count -ne 0) {
            throw "uniform.comp has unexpected SPIR-V resources: $category"
        }
    }
    $ubos = @($Spirv.ubos)
    $ssbos = @($Spirv.ssbos)
    if ($ubos.Count -ne 1 -or $ssbos.Count -ne 1) {
        throw 'uniform.comp must reflect exactly one uniform and one storage buffer.'
    }
    foreach ($item in @(@($ubos[0], 0), @($ssbos[0], 1))) {
        $resource = $item[0]
        $set = $item[1]
        $array = $resource.PSObject.Properties['array']
        if ($resource.set -ne $set -or $resource.binding -ne 0 -or
            $resource.block_size -ne 16 -or $null -ne $array) {
            throw "uniform.comp SPIR-V set $set buffer binding, count or byte size differs."
        }
        $type = $Spirv.types.PSObject.Properties[$resource.type]
        if ($null -eq $type) { throw 'uniform.comp reflected buffer type is missing.' }
        $members = @($type.Value.members)
        if ($members.Count -ne 1 -or $members[0].type -cne 'uvec4' -or
            $members[0].offset -ne 0 -or $null -ne $members[0].PSObject.Properties['array']) {
            throw "uniform.comp SPIR-V set $set must contain one uvec4 at byte zero."
        }
    }
    $writeonly = $ssbos[0].PSObject.Properties['writeonly']
    if ($null -eq $writeonly -or $writeonly.Value -ne $true) {
        throw 'uniform.comp private output must be write-only.'
    }
    if ($Dxil -cnotmatch '(?m)^; Compute Shader\s*$' -or
        $Dxil -cnotmatch '(?m)^; NumThreads=\(1,1,1\)\s*$' -or
        $Dxil -cnotmatch '(?m)^; EntryFunctionName: main\s*$') {
        throw 'uniform.comp DXIL entry point or local size differs from the managed ABI.'
    }
    $sections = @($Dxil -split '; Resource Bindings:')
    if ($sections.Count -ne 2) { throw 'uniform.comp DXIL resource reflection is missing.' }
    # Capture every reflected resource row, including unsupported types, so an
    # extra resource cannot pass just because it is not a cbuffer or raw UAV.
    $rows = [regex]::Matches($sections[1],
        '(?m)^;\s+(?<name>\S+)\s+(?<kind>\S+)\s+(?<format>\S+)\s+(?<dimension>\S+)\s+(?<id>\S+)\s+(?<slot>\S+)\s+(?<count>\d+)\s*$')
    if ($rows.Count -ne 2) { throw 'uniform.comp DXIL must bind exactly two resources.' }
    $uniform = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'cbuffer' })
    $output = @($rows | Where-Object { $_.Groups['kind'].Value -ceq 'UAV' })
    if ($uniform.Count -ne 1 -or $output.Count -ne 1 -or
        $uniform[0].Groups['slot'].Value -cnotmatch '^cb0(?:,space0)?$' -or
        $uniform[0].Groups['count'].Value -cne '1' -or
        $output[0].Groups['slot'].Value -cne 'u0,space1' -or
        $output[0].Groups['count'].Value -cne '1' -or
        $output[0].Groups['format'].Value -cne 'byte' -or
        $output[0].Groups['dimension'].Value -cne 'r/w') {
        throw 'uniform.comp DXIL requires one b0/space0 cbuffer and one u0/space1 raw UAV.'
    }
    $name = [regex]::Escape($uniform[0].Groups['name'].Value)
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
