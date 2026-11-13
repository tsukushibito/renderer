# Repeat buffer Graph content acceptance, independently of performance testing.
[CmdletBinding()]
param(
    [ValidateRange(1, 100)]
    [int]$Runs = 3,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest

function Invoke-LoggedNative {
    param([string]$Executable, [string[]]$Arguments, [string]$LogPath)
    # Windows PowerShell wraps native stderr in ErrorRecord objects. Preserve
    # their text and determine success from the native exit code.
    $previousAction = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $global:LASTEXITCODE = $null
        $lines = @(& $Executable @Arguments 2>&1 | ForEach-Object { "$_" })
        $nativeExit = $global:LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousAction
    }
    $lines | Set-Content -LiteralPath $LogPath -Encoding utf8
    foreach ($line in $lines) { Write-Host $line }
    if ($null -eq $nativeExit -or $nativeExit -ne 0) {
        throw "Command failed (exit $nativeExit); see $LogPath"
    }
    return $lines
}

function Assert-True {
    param($Value, [string]$Name)
    if ($Value -isnot [bool] -or -not $Value) { throw "Report did not confirm $Name." }
}

function Assert-CountZero {
    param($Value, [string]$Name)
    if (($Value -isnot [int] -and $Value -isnot [long]) -or $Value -ne 0) {
        throw "Report did not confirm zero $Name."
    }
}

function Assert-BufferReport {
    param($Report, [bool]$Strict)
    if ($Report.schema_version -ne 1 -or $Report.scope -cne 'm4-buffer-common-graph') {
        throw 'Unsupported M4 buffer report schema or scope.'
    }
    $configuration = $Report.configuration
    foreach ($field in @('strict_validation', 'development_validation',
            'd3d12_diagnostics_compiled', 'vulkan_diagnostics_compiled')) {
        if ($configuration.$field -isnot [bool] -or $configuration.$field -ne $Strict) {
            throw "Unexpected configuration.$field for strict=$Strict."
        }
    }
    if ($configuration.debug_assertions -isnot [bool] -or $configuration.debug_assertions) {
        throw 'The optimized acceptance must have debug assertions disabled.'
    }
    if ($configuration.graph_diagnostics_compiled -isnot [bool] -or
        $configuration.graph_diagnostics_compiled) {
        throw 'This runner expects Graph diagnostic payloads to be compiled out.'
    }
    Assert-True $Report.adapter.same_adapter 'adapter.same_adapter'
    if (@($Report.adapter.luid).Count -ne 8 -or
        $Report.adapter.vulkan_device_type -ceq 'CPU') {
        throw 'A valid shared hardware adapter identity is required.'
    }
    Assert-True $Report.graph.compiled_once 'graph.compiled_once'
    Assert-True $Report.graph.native_runs_sequential 'graph.native_runs_sequential'
    Assert-True $Report.graph.all_bytes_match 'graph.all_bytes_match'
    if ($Report.graph.compared_bytes_per_backend -le 0 -or
        @($Report.graph.readbacks).Count -ne $Report.graph.readback_passes) {
        throw 'Missing complete byte comparison evidence.'
    }
    $comparedBytes = 0
    foreach ($readback in $Report.graph.readbacks) {
        Assert-True $readback.all_bytes_match "readback $($readback.name)"
        foreach ($field in @('expected_bytes', 'vulkan_bytes', 'd3d12_bytes')) {
            if (@($readback.$field).Count -ne $readback.byte_count) {
                throw "Incomplete raw $field evidence for $($readback.name)."
            }
        }
        $comparedBytes += $readback.byte_count
    }
    if ($comparedBytes -ne $Report.graph.compared_bytes_per_backend) {
        throw 'Readback byte counts do not match the complete comparison total.'
    }
    $d3d12 = $Report.validation.d3d12
    $vulkan = $Report.validation.vulkan
    foreach ($field in @('warnings', 'errors', 'corruption', 'callback_failures', 'dropped_messages')) {
        Assert-CountZero $d3d12.$field "d3d12.$field"
    }
    foreach ($field in @('warnings', 'errors', 'callback_failures', 'dropped_problems', 'dropped_layer_statuses')) {
        Assert-CountZero $vulkan.$field "vulkan.$field"
    }
    foreach ($field in @('debug_layer_enabled', 'gpu_based_validation_enabled')) {
        if ($d3d12.$field -isnot [bool] -or $d3d12.$field -ne $Strict) {
            throw "Unexpected d3d12.$field for strict=$Strict."
        }
    }
    if ($Strict) {
        foreach ($field in @('core_validation', 'synchronization_validation', 'teardown_checked')) {
            Assert-True $vulkan.$field "vulkan.$field"
        }
        Assert-True $d3d12.teardown_checked 'd3d12.teardown_checked'
    } elseif ($vulkan.requested -cne 'Disabled' -or
        $null -ne $vulkan.core_validation -or $null -ne $vulkan.synchronization_validation) {
        throw 'Shipping acceptance must explicitly request no native validation.'
    }
}

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or
    -not [Environment]::Is64BitProcess) {
    throw 'M4 buffer acceptance requires 64-bit Windows PowerShell or pwsh.'
}
$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$cargo = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
$git = (Get-Command git -CommandType Application -ErrorAction Stop).Source
$rustc = (Get-Command rustc -CommandType Application -ErrorAction Stop).Source
$gitArguments = @('-c', "safe.directory=$repository", '-C', $repository)
$commonDirectory = & $git @gitArguments rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify the shared repository directory.' }
$sharedRepository = Split-Path -Parent $commonDirectory
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $runName = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssfffZ') + '-' +
        [Guid]::NewGuid().ToString('N').Substring(0, 8)
    $OutputDirectory = Join-Path $sharedRepository ".tools/m4/buffer/$runName"
} elseif (-not [IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $repository $OutputDirectory
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) {
    throw "Choose a new output directory to preserve existing evidence: $OutputDirectory"
}
$null = New-Item -ItemType Directory -Path $OutputDirectory
$summaryPath = Join-Path $OutputDirectory 'summary.json'
$summary = [ordered]@{
    schema_version = 1
    scope = 'm4-buffer-common-graph'
    started_utc = (Get-Date).ToUniversalTime().ToString('o')
    status = 'running'
    requested_runs_per_configuration = $Runs
    completed_runs = 0
    repository = $repository
    output_directory = $OutputDirectory
    operator = [Environment]::UserName
    machine = [Environment]::MachineName
    os = [Environment]::OSVersion.VersionString
    powershell = $PSVersionTable.PSVersion.ToString()
    profile = 'release'
    commands = @()
    runs = @()
    note = 'Common buffer Graph content and teardown acceptance only; no image, performance, long-duration or external validation injection claim.'
}
$previousReport = [Environment]::GetEnvironmentVariable('RENDERER_M4_BUFFER_REPORT', 'Process')
$previousLocation = Get-Location
try {
    Set-Location -LiteralPath $repository
    $summary.commit = (& $git @gitArguments rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read the current commit.' }
    $summary.git_status = @(& $git @gitArguments status --porcelain=v1 --untracked-files=normal)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read worktree status.' }
    $summary.cpu = @((Get-CimInstance Win32_Processor).Name)
    $summary.rustc = (& $rustc --version).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read rustc version.' }
    $summary.cargo = (& $cargo --version).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read cargo version.' }
    $summary.lock_sha256 = (Get-FileHash -LiteralPath 'Cargo.lock' -Algorithm SHA256).Hash
    $summary.validation_environment = [ordered]@{}
    foreach ($name in @('VK_LAYER_PATH', 'VK_ADD_LAYER_PATH', 'VK_INSTANCE_LAYERS',
            'VK_LOADER_LAYERS_ENABLE', 'VK_LOADER_LAYERS_DISABLE', 'VK_LAYER_SETTINGS_PATH',
            'VK_LAYER_ENABLES', 'VK_LAYER_DISABLES', 'VK_ADD_IMPLICIT_LAYER_PATH',
            'VK_IMPLICIT_LAYER_PATH', 'VK_LOADER_DEBUG', 'VK_DRIVER_FILES',
            'VK_ICD_FILENAMES', 'VK_LOADER_DRIVERS_SELECT', 'VK_LOADER_DRIVERS_DISABLE', 'RUST_LOG')) {
        $summary.validation_environment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
    }
    $summary.build_environment = [ordered]@{}
    foreach ($name in @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET',
            'CARGO_TARGET_DIR', 'CARGO_INCREMENTAL')) {
        $summary.build_environment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
    }
    foreach ($configuration in @(
            @{ name = 'strict'; features = 'vulkan,d3d12,development-validation'; strict = $true },
            @{ name = 'shipping'; features = 'vulkan,d3d12'; strict = $false })) {
        $testName = "m4_buffer_shared_graph_$($configuration.name)"
        $testArgs = @('test', '--locked', '-p', 'renderer', '--release', '--no-default-features',
            '--features', $configuration.features, '--test', 'd3d12_graph', $testName, '--')
        $inventoryArgs = $testArgs + @('--exact', '--list', '--ignored')
        $summary.commands += @{ executable = $cargo; arguments = $inventoryArgs }
        $inventory = @(Invoke-LoggedNative $cargo $inventoryArgs (
            Join-Path $OutputDirectory "$($configuration.name)-inventory.log"))
        $tests = @($inventory | Where-Object { $_ -match '^\S+: test$' })
        if ($tests.Count -ne 1 -or $tests[0] -cne "${testName}: test") {
            throw "Expected exactly the ignored $testName test; found $($tests.Count)."
        }
        $featureArgs = @('tree', '--locked', '-p', 'renderer', '--no-default-features',
            '--features', $configuration.features, '-e', 'features')
        $summary.commands += @{ executable = $cargo; arguments = $featureArgs }
        $null = Invoke-LoggedNative $cargo $featureArgs (
            Join-Path $OutputDirectory "$($configuration.name)-features.log")
        $runArgs = $testArgs + @('--exact', '--ignored', '--nocapture', '--test-threads=1')
        $summary.commands += @{ executable = $cargo; arguments = $runArgs }
        for ($run = 1; $run -le $Runs; $run++) {
            $name = '{0}-{1:D2}' -f $configuration.name, $run
            $reportPath = Join-Path $OutputDirectory "$name.json"
            $logPath = Join-Path $OutputDirectory "$name.log"
            [Environment]::SetEnvironmentVariable('RENDERER_M4_BUFFER_REPORT', $reportPath, 'Process')
            $timer = [Diagnostics.Stopwatch]::StartNew()
            $null = Invoke-LoggedNative $cargo $runArgs $logPath
            $timer.Stop()
            if (-not (Test-Path -LiteralPath $reportPath -PathType Leaf)) {
                throw "The native acceptance test did not produce $reportPath."
            }
            $report = Get-Content -LiteralPath $reportPath -Raw -Encoding utf8 | ConvertFrom-Json
            Assert-BufferReport $report $configuration.strict
            $summary.runs += @{ configuration = $configuration.name; index = $run;
                features = $configuration.features; report = [IO.Path]::GetFileName($reportPath);
                log = [IO.Path]::GetFileName($logPath); elapsed_wall_seconds = $timer.Elapsed.TotalSeconds }
            $summary.completed_runs++
            $summary | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $summaryPath -Encoding utf8
        }
    }
    $summary.status = 'passed'
} catch {
    $summary.status = 'failed'
    $summary.failure = $_.Exception.Message
    throw
} finally {
    try {
        $summary.finished_utc = (Get-Date).ToUniversalTime().ToString('o')
        $summary | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $summaryPath -Encoding utf8
    } finally {
        [Environment]::SetEnvironmentVariable('RENDERER_M4_BUFFER_REPORT', $previousReport, 'Process')
        Set-Location -LiteralPath $previousLocation
        Write-Host "M4 buffer evidence: $summaryPath"
    }
}
