[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateNotNullOrEmpty()]
    [string[]] $Path,

    [ValidateNotNullOrEmpty()]
    [string] $Validator
)

$ErrorActionPreference = 'Stop'
# Handle native exit codes explicitly in both Windows PowerShell and pwsh.
$PSNativeCommandUseErrorActionPreference = $false

try {
    $inputs = @(
        foreach ($inputPath in $Path) {
            if ([string]::IsNullOrWhiteSpace($inputPath) -or
                -not (Test-Path -LiteralPath $inputPath -PathType Leaf)) {
                throw "SPIR-V input file does not exist: $inputPath"
            }
            (Resolve-Path -LiteralPath $inputPath).ProviderPath
        }
    )

    if ([string]::IsNullOrWhiteSpace($Validator)) {
        $onWindows = [System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT
        if (-not [string]::IsNullOrWhiteSpace($env:VULKAN_SDK)) {
            $relativeValidator = if ($onWindows) { 'Bin/spirv-val.exe' } else { 'bin/spirv-val' }
            $Validator = Join-Path $env:VULKAN_SDK $relativeValidator
        }
        elseif ($onWindows -and
            (Test-Path -LiteralPath 'C:/VulkanSDK/1.4.341.1/Bin/spirv-val.exe' -PathType Leaf)) {
            $Validator = 'C:/VulkanSDK/1.4.341.1/Bin/spirv-val.exe'
        }
        else {
            $Validator = 'spirv-val'
        }
    }

    $validatorCommand = Get-Command -Name $Validator -CommandType Application -ErrorAction Stop |
        Select-Object -First 1
    $validatorPath = $validatorCommand.Source

    foreach ($inputPath in $inputs) {
        & $validatorPath --target-env vulkan1.3 $inputPath
        $validatorExit = $LASTEXITCODE
        if ($validatorExit -ne 0) {
            [Console]::Error.WriteLine("SPIR-V validation failed for $inputPath (exit $validatorExit).")
            exit $validatorExit
        }
    }

    Write-Output ("Validated {0} SPIR-V file(s) for Vulkan 1.3 with {1}" -f $inputs.Count, $validatorPath)
}
catch {
    [Console]::Error.WriteLine("SPIR-V validation failed: {0}", $_.Exception.Message)
    exit 1
}
