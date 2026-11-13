[CmdletBinding()]
param([switch]$Verify)

$ErrorActionPreference = 'Stop'
$repository = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repository 'tools/d3d12-bindings/Cargo.toml'
$destination = Join-Path $repository 'crates/renderer/src/backends/d3d12/bindings.rs'
$temporary = Join-Path ([IO.Path]::GetTempPath()) ('renderer-d3d12-bindings-' + [Guid]::NewGuid().ToString('N') + '.rs')
try {
    cargo run --locked --manifest-path $manifest --target-dir (Join-Path $repository '.tools/d3d12-bindings-target') -- $temporary
    if ($LASTEXITCODE -ne 0) { throw 'D3D12 binding generation failed.' }
    # Formatting is part of the pinned Rust 1.99.0 generation conditions.
    rustfmt --edition 2024 $temporary
    if ($LASTEXITCODE -ne 0) { throw 'D3D12 binding formatting failed.' }
    $hash = (Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash
    if ($Verify) {
        if (-not (Test-Path -LiteralPath $destination -PathType Leaf) -or
            $hash -cne (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash) {
            throw 'Checked-in D3D12 bindings differ from fixed-input regeneration.'
        }
    } else {
        Copy-Item -LiteralPath $temporary -Destination $destination -Force
    }
    Write-Output "D3D12 bindings SHA256 $hash"
} finally {
    if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
}
