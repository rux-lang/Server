[CmdletBinding()]
param(
    [Parameter()]
    [string] $Path
)

$repositoryRoot = $PSScriptRoot

if ([string]::IsNullOrWhiteSpace($Path)) {
    $localPath = Join-Path $repositoryRoot '.env'
    $examplePath = Join-Path $repositoryRoot '.env.example'
    $Path = if (Test-Path -LiteralPath $localPath -PathType Leaf) {
        $localPath
    }
    else {
        $examplePath
    }
}
elseif (-not [System.IO.Path]::IsPathRooted($Path)) {
    $Path = Join-Path $repositoryRoot $Path
}

if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    throw "Environment file not found: $Path"
}

$importedNames = [System.Collections.Generic.List[string]]::new()

foreach ($line in Get-Content -LiteralPath $Path) {
    $trimmedLine = $line.Trim()

    if (
        [string]::IsNullOrWhiteSpace($trimmedLine) -or
        $trimmedLine.StartsWith('#')
    ) {
        continue
    }

    $separator = $trimmedLine.IndexOf('=')
    if ($separator -le 0) {
        throw "Invalid environment entry in '$Path': $line"
    }

    $name = $trimmedLine.Substring(0, $separator).Trim()
    $value = $trimmedLine.Substring($separator + 1).Trim()

    if ($name -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') {
        throw "Invalid environment variable name in '$Path': $name"
    }

    if (
        $value.Length -ge 2 -and
        (
            ($value.StartsWith('"') -and $value.EndsWith('"')) -or
            ($value.StartsWith("'") -and $value.EndsWith("'"))
        )
    ) {
        $value = $value.Substring(1, $value.Length - 2)
    }

    [Environment]::SetEnvironmentVariable($name, $value, 'Process')
    $importedNames.Add($name)
}

$requiredNames = @(
    'RUX_DATABASE_URL',
    'RUX_STORAGE_ENDPOINT',
    'RUX_STORAGE_BUCKET',
    'RUX_STORAGE_ACCESS_KEY',
    'RUX_STORAGE_SECRET_KEY',
    'RUX_STORAGE_REGION',
    'RUX_STORAGE_FORCE_PATH_STYLE',
    'RUX_ALLOWED_WEB_ORIGIN',
    'RUX_WEB_CALLBACK_URL',
    'RUX_GITHUB_CLIENT_ID',
    'RUX_GITHUB_CLIENT_SECRET',
    'RUX_GITHUB_CALLBACK_URL'
)

$missingNames = @(
    foreach ($requiredName in $requiredNames) {
        if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($requiredName, 'Process'))) {
            $requiredName
        }
    }
)

if ($missingNames.Count -gt 0) {
    throw "Missing required environment variables: $($missingNames -join ', ')"
}

Write-Host "Imported $($importedNames.Count) environment variables from $Path"
Write-Host ($importedNames | Sort-Object | ForEach-Object { "  $_" } | Out-String)
