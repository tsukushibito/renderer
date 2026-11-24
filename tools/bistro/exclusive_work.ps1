param(
    [Parameter(Mandatory = $true)][string]$Issue,
    [Parameter(Mandatory = $true)][scriptblock]$Work,
    [string]$LockPath
)

$ErrorActionPreference = 'Stop'
if (!$LockPath) {
    $commonDir = git -C $PSScriptRoot rev-parse --path-format=absolute --git-common-dir
    if ($LASTEXITCODE -ne 0) { throw 'Cannot locate the shared Git directory for the B3 lock.' }
    $LockPath = Join-Path (Split-Path -Parent $commonDir) '.tools/parallel-b3/exclusive-work.lock'
}
$ownerToken = [guid]::NewGuid().ToString('N')
$resolvedLock = [IO.Path]::GetFullPath($LockPath)
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($resolvedLock)) | Out-Null
$owner = @{ token = $ownerToken; issue = $Issue; pid = $PID; cwd = $PWD.Path; started_utc = [DateTime]::UtcNow.ToString('o') }
try {
    $lockStream = [IO.File]::Open($resolvedLock, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
} catch [IO.IOException] {
    if (Test-Path -LiteralPath $resolvedLock) {
        Write-Host ('Exclusive work is owned by: ' + [IO.File]::ReadAllText($resolvedLock))
    }
    throw 'Exclusive B3 work lock is busy; do not remove another owner''s lock.'
}
try {
    $ownerBytes = [Text.Encoding]::UTF8.GetBytes(($owner | ConvertTo-Json -Compress))
    $lockStream.Write($ownerBytes, 0, $ownerBytes.Length)
    $lockStream.Flush()
    Write-Host ('Acquired B3 lock ' + $ownerToken + ' for ' + $Issue)
    & $Work
    if ($LASTEXITCODE) { throw "Exclusive work failed with exit code $LASTEXITCODE" }
} finally {
    $lockStream.Dispose()
    if (Test-Path -LiteralPath $resolvedLock) {
        $currentOwner = [IO.File]::ReadAllText($resolvedLock) | ConvertFrom-Json
        if ($currentOwner.token -eq $ownerToken) {
            Remove-Item -LiteralPath $resolvedLock
            Write-Host ('Released B3 lock ' + $ownerToken)
        } else {
            Write-Warning 'Lock ownership changed; leaving the lock untouched.'
        }
    }
}
