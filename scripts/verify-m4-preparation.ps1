# Repeat the explicit preparation probe. This is not a rendering benchmark.
[CmdletBinding()]
param(
    [ValidateRange(1, 100)]
    [int]$Runs = 3,
    [ValidateNotNullOrEmpty()]
    [string]$SdkRoot = 'C:/VulkanSDK/1.4.341.1',
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
Set-StrictMode -Version Latest

function Invoke-LoggedNative {
    param([string]$Executable, [string[]]$Arguments, [string]$LogPath)
    # Windows PowerShell wraps native stderr in ErrorRecord objects. Preserve
    # their text, then decide success only from the native exit code.
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

function Assert-PreparationReport {
    param($Report)
    if ($Report.schema_version -ne 1) { throw 'Unsupported preparation report schema.' }
    foreach ($field in @('debug_layer_requested', 'gpu_based_validation_requested',
            'debug_device_available', 'info_queue_available', 'teardown_checked')) {
        if ($Report.validation.$field -isnot [bool] -or -not $Report.validation.$field) {
            throw "Preparation report did not confirm validation.$field."
        }
    }
    foreach ($field in @('warnings', 'errors', 'corruption')) {
        $count = $Report.validation.$field
        if (($count -isnot [int] -and $count -isnot [long]) -or $count -ne 0) {
            throw "Preparation report did not confirm zero validation.$field."
        }
    }
    if ($Report.vulkan.same_adapter -isnot [bool] -or -not $Report.vulkan.same_adapter -or
        $Report.vulkan.luid_valid -isnot [bool] -or -not $Report.vulkan.luid_valid) {
        throw 'D3D12 and Vulkan were not confirmed to use the same hardware adapter.'
    }
    if ($Report.allocator.baseline -cne 'committed_resource' -or
        $Report.allocator.probe_created -isnot [bool] -or -not $Report.allocator.probe_created -or
        $Report.allocator.submitted -isnot [bool] -or $Report.allocator.submitted) {
        throw 'Unexpected preparation allocation probe result.'
    }
}

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT -or
    -not [Environment]::Is64BitProcess) {
    throw 'M4 preparation acceptance requires 64-bit Windows PowerShell or pwsh.'
}
$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$shaderVerification = Join-Path $repository 'crates/renderer/shaders/regenerate-dxil.ps1'
if (-not (Test-Path -LiteralPath $shaderVerification -PathType Leaf)) {
    throw "Missing DXIL verification script: $shaderVerification"
}
if (-not (Test-Path -LiteralPath (Join-Path $SdkRoot 'Bin/dxc.exe') -PathType Leaf)) {
    throw "The pinned Vulkan SDK DXC is missing from $SdkRoot."
}
$SdkRoot = (Resolve-Path -LiteralPath $SdkRoot).ProviderPath
$cargo = (Get-Command cargo -CommandType Application -ErrorAction Stop).Source
$git = (Get-Command git -CommandType Application -ErrorAction Stop).Source
$rustc = (Get-Command rustc -CommandType Application -ErrorAction Stop).Source
$hostName = if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh.exe' } else { 'powershell.exe' }
$hostExecutable = Join-Path $PSHOME $hostName
$gitArguments = @('-c', "safe.directory=$repository", '-C', $repository)
$commonDirectory = & $git @gitArguments rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify the shared repository directory.' }
$sharedRepository = Split-Path -Parent $commonDirectory
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $runName = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssfffZ') + '-' +
        [Guid]::NewGuid().ToString('N').Substring(0, 8)
    $OutputDirectory = Join-Path $sharedRepository ".tools/m4/preparation/$runName"
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
    scope = 'm4-preparation-only'
    started_utc = (Get-Date).ToUniversalTime().ToString('o')
    status = 'running'
    requested_runs = $Runs
    completed_runs = 0
    repository = $repository
    output_directory = $OutputDirectory
    operator = [Environment]::UserName
    machine = [Environment]::MachineName
    os = [Environment]::OSVersion.VersionString
    powershell = $PSVersionTable.PSVersion.ToString()
    sdk_root = [IO.Path]::GetFullPath($SdkRoot)
    commands = @()
    runs = @()
    note = 'Initialization/allocation readiness only; no Graph execution, image equivalence or performance claim.'
}
$previousReport = [Environment]::GetEnvironmentVariable('RENDERER_M4_PREPARATION_REPORT', 'Process')
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
    $shaderArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File',
        $shaderVerification, '-SdkRoot', $SdkRoot, '-Verify')
    $summary.commands += @{ executable = $hostExecutable; arguments = $shaderArgs }
    $null = Invoke-LoggedNative $hostExecutable $shaderArgs (Join-Path $OutputDirectory 'shader-verification.log')
    $summary.shader_artifacts = @(
        Get-ChildItem -LiteralPath 'crates/renderer/shaders' -File -Recurse |
            Where-Object Extension -in @('.spv', '.hlsl', '.dxil', '.json', '.vert', '.frag', '.comp') |
            Sort-Object FullName | ForEach-Object {
                @{ path = $_.FullName.Substring($repository.Length + 1).Replace('\', '/');
                   sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
            }
    )
    $probeArgs = @('test', '--locked', '-p', 'renderer', '--lib', '--no-default-features',
        '--features', 'vulkan,d3d12', '::d3d12_preparation_', '--')
    $inventoryArgs = $probeArgs + @('--list', '--ignored')
    $summary.commands += @{ executable = $cargo; arguments = $inventoryArgs }
    $inventory = @(Invoke-LoggedNative $cargo $inventoryArgs (Join-Path $OutputDirectory 'probe-inventory.log'))
    $tests = @($inventory | Where-Object { $_ -match '^\S+: test$' })
    if ($tests.Count -ne 1) {
        throw "Expected exactly one ignored preparation test, found $($tests.Count)."
    }
    $summary.test = $tests[0] -replace ': test$', ''
    $runArgs = $probeArgs + @('--ignored', '--nocapture', '--test-threads=1')
    $summary.commands += @{ executable = $cargo; arguments = $runArgs }
    for ($run = 1; $run -le $Runs; $run++) {
        $reportPath = Join-Path $OutputDirectory ("probe-{0:D2}.json" -f $run)
        $logPath = Join-Path $OutputDirectory ("probe-{0:D2}.log" -f $run)
        [Environment]::SetEnvironmentVariable('RENDERER_M4_PREPARATION_REPORT', $reportPath, 'Process')
        $timer = [Diagnostics.Stopwatch]::StartNew()
        $null = Invoke-LoggedNative $cargo $runArgs $logPath
        $timer.Stop()
        if (-not (Test-Path -LiteralPath $reportPath -PathType Leaf)) {
            throw "The native preparation test did not produce $reportPath."
        }
        $report = Get-Content -LiteralPath $reportPath -Raw -Encoding utf8 | ConvertFrom-Json
        Assert-PreparationReport $report
        $summary.runs += @{ index = $run; report = [IO.Path]::GetFileName($reportPath);
            log = [IO.Path]::GetFileName($logPath); elapsed_wall_seconds = $timer.Elapsed.TotalSeconds }
        $summary.completed_runs = $run
        $summary | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $summaryPath -Encoding utf8
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
        [Environment]::SetEnvironmentVariable('RENDERER_M4_PREPARATION_REPORT', $previousReport, 'Process')
        Set-Location -LiteralPath $previousLocation
        Write-Host "M4 preparation evidence: $summaryPath"
    }
}
