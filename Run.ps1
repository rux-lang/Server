<#
.SYNOPSIS
    Every command needed to develop, test, and verify the Rux server.

.DESCRIPTION
    One entry point for the commands otherwise spread across README.md,
    docs/migrations.md, docs/playground.md, and .github/workflows/ci.yml. Run
    without arguments for full help.

    Local development only. The production sequences in docs/deployment.md and
    docs/recovery.md are deliberately unautomated and are not reproduced here.
#>

# Deliberately no param() block. Arguments arrive in $args exactly as typed,
# which is the only way both "./Run.ps1 -Build" and a cargo short flag survive:
# a param() block would make this an advanced function, and PowerShell would
# then reject "-p rux-domain" as an ambiguous common parameter.
$Arguments = $args

$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Pinned values
# ---------------------------------------------------------------------------

# The sandbox image's compiler pin. Duplicated by design - PLAYGROUND_RUX_VERSION
# and PLAYGROUND_RUX_SHA256 in .github/workflows/ci.yml, the same pair in
# docs/playground.md, and the production host. Move all of them together or the
# sandbox drifts.
$PlaygroundVersion = '0.3.0'
$PlaygroundSha256 = '82e654f9ced042dc029220836d1322b208790099627f32efd9d8d600834be5cc'

# From rust-version in Cargo.toml. There is no rust-toolchain.toml, so whatever
# toolchain rustup has active is the one used.
$MsrvMinimum = [version] '1.85'

$RepoRoot = Split-Path -Parent $PSCommandPath
$DefaultConfigPath = Join-Path $RepoRoot 'config\config.toml'
$UserConfigPath = Join-Path $env:APPDATA 'rux\config.toml'
$PlaygroundTestRoot = Join-Path $RepoRoot '.cache\playground-test'
$StorageAlias = 'rux-local'

$script:ConfigOverride = $null
$script:FailureCode = 1

# ---------------------------------------------------------------------------
# Help, as data
# ---------------------------------------------------------------------------

$Commands = @(
    @{ Group = 'Getting started'; Name = 'help'; Aliases = '-h, --help, /?'; Usage = 'help [command]'
        Summary = 'This help. With a command, only that command.' }
    @{ Group = 'Getting started'; Name = 'doctor'; Aliases = 'env, check-env'; Usage = 'doctor'
        Summary = 'Probe the toolchain, PostgreSQL, MinIO, and the resolved configuration.' }
    @{ Group = 'Getting started'; Name = 'config'; Aliases = 'cfg'; Usage = 'config [path|show|init|edit]'
        Summary = 'Show the configuration in use, or create your own outside the repository.' }

    @{ Group = 'Quality gates'; Name = 'fmt'; Aliases = 'format'; Usage = 'fmt [check]'
        Summary = 'cargo fmt --all. With "check", the non-writing form CI runs.' }
    @{ Group = 'Quality gates'; Name = 'lint'; Aliases = 'clippy'; Usage = 'lint'
        Summary = 'cargo clippy --workspace --all-targets --all-features -- -D warnings' }
    @{ Group = 'Quality gates'; Name = 'test'; Aliases = 't'; Usage = 'test [cargo arguments]'
        Summary = 'cargo test --workspace --all-features, or exactly the arguments you give.' }
    @{ Group = 'Quality gates'; Name = 'build'; Aliases = 'b'; Usage = 'build [release]'
        Summary = 'cargo build --workspace, optionally --release.' }
    @{ Group = 'Quality gates'; Name = 'check'; Aliases = 'gates'; Usage = 'check'
        Summary = 'All four CI gates in order: fmt --check, lint, test, release build.' }
    @{ Group = 'Quality gates'; Name = 'ci'; Aliases = 'all'; Usage = 'ci'
        Summary = 'check, then the playground image build and its containment suite.' }

    @{ Group = 'Running'; Name = 'dev'; Aliases = 'run, api, serve'; Usage = 'dev [cargo arguments]'
        Summary = 'The API on the resolved configuration. A plain cargo run - nothing watches or reloads.' }
    @{ Group = 'Running'; Name = 'broker'; Aliases = 'playgroundd'; Usage = 'broker'
        Summary = 'Run the playground broker. Needs a Unix host with Docker - use WSL.' }

    @{ Group = 'Database'; Name = 'migrate'; Aliases = 'db'; Usage = 'migrate [run|info|add|revert|install]'
        Summary = 'Drive the SQLx CLI. Bare "migrate" runs pending migrations.' }

    @{ Group = 'Object storage'; Name = 'storage'; Aliases = 'minio, s3'; Usage = 'storage [check|init|test]'
        Summary = 'Check the bucket, create it with versioning, or run the live-bucket test.' }

    @{ Group = 'Playground'; Name = 'playground'; Aliases = 'sandbox'; Usage = 'playground [build|test|docker-test]'
        Summary = 'Build the pinned sandbox image and assert its containment.' }

    @{ Group = 'Verification'; Name = 'smoke'; Aliases = 'verify'; Usage = 'smoke'
        Summary = "CI's contract checks against an API you already started." }

    @{ Group = 'Housekeeping'; Name = 'clean'; Aliases = ''; Usage = 'clean'
        Summary = 'cargo clean, and remove the playground test scratch directory.' }
)

$Aliases = @{
    'h' = 'help'; '?' = 'help'; 'usage' = 'help'
    'env' = 'doctor'; 'check-env' = 'doctor'; 'checkenv' = 'doctor'
    'cfg' = 'config'
    'format' = 'fmt'
    'clippy' = 'lint'
    't' = 'test'
    'b' = 'build'
    'gates' = 'check'
    'all' = 'ci'
    'run' = 'dev'; 'api' = 'dev'; 'serve' = 'dev'
    'playgroundd' = 'broker'
    'db' = 'migrate'; 'sqlx' = 'migrate'
    'minio' = 'storage'; 's3' = 'storage'
    'sandbox' = 'playground'
    'verify' = 'smoke'
}

# ---------------------------------------------------------------------------
# Output helpers
# ---------------------------------------------------------------------------

function Write-Heading {
    param([Parameter(Mandatory)][string] $Text)

    Write-Host ''
    Write-Host $Text -ForegroundColor White
}

function Write-Ok {
    param([Parameter(Mandatory)][string] $Text)

    Write-Host '  ok   ' -ForegroundColor Green -NoNewline
    Write-Host $Text
}

function Write-Warn {
    param([Parameter(Mandatory)][string] $Text, [string] $Remedy)

    Write-Host '  warn ' -ForegroundColor Yellow -NoNewline
    Write-Host $Text
    if ($Remedy) { Write-Host "       $Remedy" -ForegroundColor DarkGray }
}

function Write-Fail {
    param([Parameter(Mandatory)][string] $Text, [string] $Remedy)

    Write-Host '  FAIL ' -ForegroundColor Red -NoNewline
    Write-Host $Text
    if ($Remedy) { Write-Host "       $Remedy" -ForegroundColor DarkGray }
}

function Write-Note {
    param([Parameter(Mandatory)][string] $Text)

    Write-Host $Text -ForegroundColor DarkGray
}

# Echoes the exact command line before running it: this script is meant to teach
# the underlying commands, not to hide them.
function Invoke-Step {
    param(
        [Parameter(Mandatory)][string] $Command,
        [string[]] $CommandArguments = @(),
        [switch] $AllowFailure
    )

    $rendered = ($CommandArguments | ForEach-Object {
        if ($_ -match '[\s"]') { '"' + $_ + '"' } else { $_ }
    }) -join ' '
    Write-Host ''
    Write-Host "> $Command $rendered" -ForegroundColor Cyan

    & $Command @CommandArguments
    $code = $LASTEXITCODE

    if ($code -ne 0 -and -not $AllowFailure) {
        $script:FailureCode = $code
        throw "$Command exited with $code"
    }

    return $code
}

# Always returns an array. A bare slice assigned out of an `if` is unrolled by
# PowerShell when it holds one element, and a lone string then indexes by
# character - which would silently turn "migrate add foo" into "migrate add f".
function Get-Tail {
    param([string[]] $Tokens, [int] $Skip = 1)

    if (-not $Tokens -or $Tokens.Count -le $Skip) { return , @() }
    return , @($Tokens[$Skip..($Tokens.Count - 1)])
}

function Test-Tool {
    param([Parameter(Mandatory)][string] $Name)

    return [bool] (Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue)
}

# Git Bash, not the WSL stub in System32. The playground scripts bind-mount
# fixtures into containers run by Docker Desktop, so they need a shell that
# speaks Windows paths - which is also why test-image.sh has the
# RUX_PLAYGROUND_TEST_ROOT knob. Set RUX_BASH to override.
function Get-BashPath {
    if ($env:RUX_BASH) { return $env:RUX_BASH }

    $candidates = @()

    $git = Get-Command git -CommandType Application -ErrorAction SilentlyContinue
    if ($git) {
        $candidates += Join-Path (Split-Path -Parent (Split-Path -Parent $git.Source)) 'bin\bash.exe'
    }

    $candidates += @(
        (Join-Path $env:ProgramFiles 'Git\bin\bash.exe')
        (Join-Path ${env:ProgramFiles(x86)} 'Git\bin\bash.exe')
        (Join-Path $env:LOCALAPPDATA 'Programs\Git\bin\bash.exe')
    )

    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate)) { return $candidate }
    }

    $fallback = Get-Command bash -CommandType Application -ErrorAction SilentlyContinue
    if ($fallback) { return $fallback.Source }

    return $null
}

function Assert-Tool {
    param([Parameter(Mandatory)][string] $Name, [Parameter(Mandatory)][string] $Remedy)

    if (-not (Test-Tool $Name)) {
        throw "$Name is not on PATH. $Remedy"
    }
}

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

# The server is the authority on this file and rejects anything malformed at
# startup, so this reads only the handful of scalars the script itself needs
# rather than pretending to be a TOML parser.
function Read-TomlScalars {
    param([Parameter(Mandatory)][string] $Path)

    $values = @{}
    $section = ''

    foreach ($line in [System.IO.File]::ReadAllLines($Path)) {
        $text = $line.Trim()
        if ($text.Length -eq 0 -or $text.StartsWith('#')) { continue }

        if ($text -match '^\[([^\]]+)\]$') {
            $section = $Matches[1].Trim()
            continue
        }

        if ($text -match '^([A-Za-z0-9_]+)\s*=\s*(.+)$') {
            $key = $Matches[1]
            $raw = $Matches[2].Trim()
            if ($raw -match '^"([^"]*)"') {
                $value = $Matches[1]
            }
            else {
                $value = ($raw -split '#', 2)[0].Trim()
            }
            $name = if ($section) { "$section.$key" } else { $key }
            $values[$name] = $value
        }
    }

    return $values
}

function Resolve-ConfigPath {
    if ($script:ConfigOverride) {
        if (-not (Test-Path -LiteralPath $script:ConfigOverride)) {
            throw "No configuration file at $($script:ConfigOverride)"
        }
        return (Resolve-Path -LiteralPath $script:ConfigOverride).Path
    }

    if (Test-Path -LiteralPath $UserConfigPath) {
        return $UserConfigPath
    }

    return $DefaultConfigPath
}

function Get-Config {
    $path = Resolve-ConfigPath
    $values = Read-TomlScalars -Path $path
    $values['_path'] = $path
    return $values
}

function Get-ConfigValue {
    param(
        [Parameter(Mandatory)] $Config,
        [Parameter(Mandatory)][string] $Key,
        [string] $Default = ''
    )

    if ($Config.ContainsKey($Key) -and $Config[$Key]) { return $Config[$Key] }
    return $Default
}

function Get-ApiBaseUrl {
    param([Parameter(Mandatory)] $Config)

    $bind = Get-ConfigValue $Config 'server.bind_address' '127.0.0.1:8080'
    $port = ($bind -split ':')[-1]
    return "http://127.0.0.1:$port"
}

# The SQLx CLI's credential is deliberately not the server's - the one that can
# change the schema is not the one the service runs with. DATABASE_URL wins when
# it is already set; the fallback is announced rather than silent.
function Set-DatabaseUrl {
    param([Parameter(Mandatory)] $Config)

    if ($env:DATABASE_URL) {
        Write-Note "DATABASE_URL from the environment: $(Hide-Password $env:DATABASE_URL)"
        return
    }

    $url = Get-ConfigValue $Config 'database.url'
    if (-not $url) {
        throw "No DATABASE_URL set and no database.url in $($Config['_path'])"
    }

    $env:DATABASE_URL = $url
    Write-Note "DATABASE_URL from $($Config['_path']): $(Hide-Password $url)"
}

function Hide-Password {
    param([string] $Url)

    return ($Url -replace '(?<=://[^:@/]+:)[^@/]*(?=@)', '****')
}

function Split-DatabaseUrl {
    param([Parameter(Mandatory)][string] $Url)

    if ($Url -notmatch '^postgres(?:ql)?://(?:([^:@/]+)(?::([^@/]*))?@)?([^:/?]+)(?::(\d+))?(?:/([^?]*))?') {
        return $null
    }

    $port = if ($Matches[4]) { [int] $Matches[4] } else { 5432 }

    return [pscustomobject] @{
        User     = $Matches[1]
        Password = $Matches[2]
        Server   = $Matches[3]
        Port     = $port
        Database = $Matches[5]
    }
}

function Get-LockedVersion {
    param([Parameter(Mandatory)][string] $Package)

    $lock = Join-Path $RepoRoot 'Cargo.lock'
    $lines = [System.IO.File]::ReadAllLines($lock)

    for ($index = 0; $index -lt $lines.Length; $index++) {
        if ($lines[$index].Trim() -eq "name = `"$Package`"") {
            if ($lines[$index + 1].Trim() -match '^version = "([^"]+)"$') {
                return $Matches[1]
            }
        }
    }

    return $null
}

# ---------------------------------------------------------------------------
# Probes
# ---------------------------------------------------------------------------

function Test-TcpPort {
    param(
        [Parameter(Mandatory)][string] $Server,
        [Parameter(Mandatory)][int] $Port,
        [int] $TimeoutMilliseconds = 2000
    )

    $client = [System.Net.Sockets.TcpClient]::new()
    try {
        $pending = $client.BeginConnect($Server, $Port, $null, $null)
        if (-not $pending.AsyncWaitHandle.WaitOne($TimeoutMilliseconds)) { return $false }
        $client.EndConnect($pending)
        return $true
    }
    catch {
        return $false
    }
    finally {
        $client.Dispose()
    }
}

function Invoke-HttpRequest {
    param(
        [Parameter(Mandatory)][string] $Uri,
        [string] $Method = 'Get',
        [hashtable] $Headers = @{},
        [string] $Body,
        [int] $TimeoutSeconds = 15
    )

    $parameters = @{
        Uri             = $Uri
        Method          = $Method
        Headers         = $Headers
        TimeoutSec      = $TimeoutSeconds
        UseBasicParsing = $true
        ErrorAction     = 'Stop'
    }
    if ($PSBoundParameters.ContainsKey('Body')) { $parameters['Body'] = $Body }

    # PowerShell 7 can hand back the response for a 4xx or 5xx instead of
    # throwing, which is what the 403 and 503 checks below want to inspect.
    if ($PSVersionTable.PSVersion.Major -ge 6) { $parameters['SkipHttpErrorCheck'] = $true }

    try {
        $response = Invoke-WebRequest @parameters
        $status = [int] $response.StatusCode
        # An error response under -UseBasicParsing arrives as raw bytes, and a
        # problem document is worth reading.
        $content = $response.Content
        if ($content -is [byte[]]) { $content = [System.Text.Encoding]::UTF8.GetString($content) }
        return [pscustomobject] @{ Status = $status; Content = $content; Failed = ($status -ge 400) }
    }
    catch {
        $status = 0
        $content = ''
        $inner = $_.Exception.Response
        if ($inner) {
            try { $status = [int] $inner.StatusCode } catch { $status = 0 }
            if ($_.ErrorDetails -and $_.ErrorDetails.Message) { $content = $_.ErrorDetails.Message }
        }
        return [pscustomobject] @{ Status = $status; Content = $content; Failed = $true; Error = $_.Exception.Message }
    }
}

# ---------------------------------------------------------------------------
# help
# ---------------------------------------------------------------------------

function Show-Help {
    param([string] $Topic)

    if ($Topic) {
        $name = Resolve-CommandName $Topic
        $entry = $Commands | Where-Object { $_.Name -eq $name }
        if (-not $entry) {
            Write-Fail "Unknown command '$Topic'." 'Run ./Run.ps1 help for the full list.'
            $script:FailureCode = 64
            throw "unknown command"
        }
        Write-Heading $entry.Group
        Show-CommandEntry $entry
        Show-Subcommands $entry.Name
        return
    }

    Write-Host ''
    Write-Host 'Rux Server - development commands' -ForegroundColor White
    Write-Host ''
    Write-Host '  ./Run.ps1 <command> [subcommand] [arguments]'
    Write-Host '  ./Run.ps1 -Build                       switch spelling works too'
    Write-Host '  ./Run.ps1 <command> -Config <path>     use a specific configuration file'
    Write-Host ''
    Write-Host '  Trailing arguments pass through to the underlying tool, so'
    Write-Host '  ./Run.ps1 test -p rux-infrastructure --test repositories works as written.'

    foreach ($group in ($Commands | ForEach-Object { $_.Group } | Select-Object -Unique)) {
        Write-Heading $group
        foreach ($entry in ($Commands | Where-Object { $_.Group -eq $group })) {
            Show-CommandEntry $entry
        }
    }

    Write-Heading 'Subcommands'
    foreach ($name in @('config', 'migrate', 'storage', 'playground')) {
        Show-Subcommands $name
    }

    Write-Heading 'Configuration'
    Write-Host "  Both binaries take --config <path> and default to config\config.toml."
    Write-Host '  This script resolves, in order:'
    Write-Host ''
    Write-Host '    1. -Config <path> given on the command line'
    Write-Host "    2. $UserConfigPath"
    Write-Host "    3. $DefaultConfigPath"
    Write-Host ''
    Write-Host '  Keep your own credentials in' -NoNewline
    Write-Host " $UserConfigPath" -ForegroundColor White
    Write-Host '  - outside the repository, so a real GitHub OAuth secret can never be'
    Write-Host '  committed by accident. Create it with:'
    Write-Host ''
    Write-Host '    ./Run.ps1 config init' -ForegroundColor Cyan
    Write-Host ''
    Write-Host '  It is a whole configuration, not an overlay: the server does no layering'
    Write-Host '  and refuses unknown keys. In use right now:'
    Write-Host "    $(Resolve-ConfigPath)"

    Write-Heading 'Not covered here'
    Write-Host '  Production deployment and recovery are deliberately manual - see'
    Write-Host '  docs\deployment.md and docs\recovery.md.'
    Write-Host ''
}

function Show-CommandEntry {
    param([Parameter(Mandatory)] $Entry)

    $width = 41
    $usage = $Entry.Usage.PadRight($width)
    Write-Host "  $usage " -ForegroundColor Cyan -NoNewline
    Write-Host $Entry.Summary
    if ($Entry.Aliases) {
        Write-Host "  $(''.PadRight($width)) aliases: $($Entry.Aliases)" -ForegroundColor DarkGray
    }
}

function Show-Subcommands {
    param([Parameter(Mandatory)][string] $Name)

    $lines = switch ($Name) {
        'config' {
            @(
                'config path                              Print the configuration file in use.'
                'config show                              Print that file.'
                'config init                              Copy config\config.toml to your APPDATA path.'
                'config edit                              Open it in $env:EDITOR, or notepad.'
            )
        }
        'migrate' {
            @(
                'migrate run                              Apply pending migrations.'
                'migrate info                             Show applied and pending migrations.'
                'migrate add <name>                       New reversible migration pair.'
                'migrate revert                           Undo the last one - disposable databases only.'
                'migrate install                          cargo install the CLI at the lockfile version.'
            )
        }
        'storage' {
            @(
                'storage check                            Endpoint, bucket, and versioning.'
                'storage init                             Create the bucket and enable versioning.'
                'storage test                             The ignored object-storage test, live bucket.'
            )
        }
        'playground' {
            @(
                "playground build [version] [sha256]      Build rux-playground:$PlaygroundVersion."
                'playground test [image]                  Run the containment suite against it.'
                'playground docker-test                   The ignored rux-sandbox Docker test.'
            )
        }
        default { @() }
    }

    foreach ($line in $lines) {
        Write-Host "  $line"
    }
}

# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

function Invoke-Fmt {
    param([string[]] $Tail)

    if ($Tail.Count -gt 0 -and $Tail[0] -in @('check', 'ci')) {
        Invoke-Step 'cargo' @('fmt', '--all', '--', '--check') | Out-Null
        return
    }

    Invoke-Step 'cargo' (@('fmt', '--all') + $Tail) | Out-Null
}

function Invoke-Lint {
    param([string[]] $Tail)

    Invoke-Step 'cargo' (@('clippy', '--workspace', '--all-targets', '--all-features') + $Tail + @('--', '-D', 'warnings')) | Out-Null
}

function Invoke-Test {
    param([string[]] $Tail)

    # Integration tests under crates/infrastructure use #[sqlx::test], which
    # creates a database per test and therefore needs DATABASE_URL.
    Set-DatabaseUrl (Get-Config)

    if ($Tail.Count -gt 0) {
        Invoke-Step 'cargo' (@('test') + $Tail) | Out-Null
        return
    }

    Invoke-Step 'cargo' @('test', '--workspace', '--all-features') | Out-Null
}

function Invoke-BuildCommand {
    param([string[]] $Tail)

    $rest = @()
    foreach ($token in $Tail) {
        if ($token -in @('release', '--release')) { $rest += '--release' }
        else { $rest += $token }
    }

    Invoke-Step 'cargo' (@('build', '--workspace') + $rest) | Out-Null
}

function Invoke-Gates {
    Write-Note 'The four gates CI runs, in CI order. First failure stops the run.'
    Set-DatabaseUrl (Get-Config)

    Invoke-Step 'cargo' @('fmt', '--all', '--', '--check') | Out-Null
    Invoke-Step 'cargo' @('clippy', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings') | Out-Null
    Invoke-Step 'cargo' @('test', '--workspace', '--all-features') | Out-Null
    Invoke-Step 'cargo' @('build', '--workspace', '--release') | Out-Null

    Write-Host ''
    Write-Ok 'All four gates passed.'
}

function Invoke-Ci {
    Invoke-Gates
    Invoke-Playground @('build')
    Invoke-Playground @('test')

    Write-Host ''
    Write-Ok 'Everything CI runs that is reproducible here passed.'
}

function Invoke-Dev {
    param([string[]] $Tail)

    $config = Get-Config
    Write-Note "Configuration: $($config['_path'])"
    $base = Get-ApiBaseUrl $config
    Write-Note "Listening on $base once started - $base/health/live, $base/health/ready, $base/openapi/v1.json"

    Invoke-Step 'cargo' (@('run', '-p', 'rux-server') + $Tail + @('--', '--config', $config['_path'])) | Out-Null
}

function Invoke-Broker {
    param([string[]] $Tail)

    $config = Get-Config

    if ($env:OS -eq 'Windows_NT') {
        Write-Warn 'The broker needs a Unix host with a container runtime.' 'It will refuse to start here; run it from WSL or a Linux host.'
    }

    Write-Note "Configuration: $($config['_path'])"
    Invoke-Step 'cargo' (@('run', '-p', 'rux-server', '--bin', 'rux-playgroundd') + $Tail + @('--', '--config', $config['_path'])) | Out-Null
}

function Invoke-Migrate {
    param([string[]] $Tail)

    $action = if ($Tail.Count -gt 0) { $Tail[0].ToLowerInvariant() } else { 'run' }
    $rest = Get-Tail $Tail 1

    if ($action -eq 'install') {
        $version = Get-LockedVersion 'sqlx'
        if (-not $version) { throw 'Could not read the sqlx version from Cargo.lock' }
        Write-Note "Pinning sqlx-cli to the lockfile's sqlx version ($version)."
        Invoke-Step 'cargo' @(
            'install', 'sqlx-cli', '--version', $version, '--locked',
            '--no-default-features', '--features', 'rustls,postgres'
        ) | Out-Null
        return
    }

    Assert-Tool 'sqlx' 'Install it with ./Run.ps1 migrate install'
    Set-DatabaseUrl (Get-Config)

    switch ($action) {
        'run' { Invoke-Step 'sqlx' (@('migrate', 'run') + $rest) | Out-Null }
        'info' { Invoke-Step 'sqlx' (@('migrate', 'info') + $rest) | Out-Null }
        'add' {
            if ($rest.Count -lt 1) { throw 'migrate add needs a name, e.g. ./Run.ps1 migrate add add_package_search_columns' }
            Invoke-Step 'sqlx' (@('migrate', 'add', '-r') + $rest) | Out-Null
        }
        'revert' {
            Write-Warn 'Migrations that have reached a shared database are immutable.' 'Revert disposable databases only; correct a shared one with a new forward migration.'
            Invoke-Step 'sqlx' (@('migrate', 'revert') + $rest) | Out-Null
        }
        default { throw "Unknown migrate subcommand '$action'. Try run, info, add, revert, or install." }
    }
}

function Invoke-Storage {
    param([string[]] $Tail)

    $action = if ($Tail.Count -gt 0) { $Tail[0].ToLowerInvariant() } else { 'check' }
    $config = Get-Config

    $endpoint = Get-ConfigValue $config 'storage.endpoint' 'http://localhost:9000'
    $bucket = Get-ConfigValue $config 'storage.bucket' 'packages'
    $accessKey = Get-ConfigValue $config 'storage.access_key'
    $secretKey = Get-ConfigValue $config 'storage.secret_key'
    $region = Get-ConfigValue $config 'storage.region' 'us-east-1'
    $pathStyle = Get-ConfigValue $config 'storage.force_path_style' 'true'

    switch ($action) {
        'check' {
            $healthy = Test-Storage -Endpoint $endpoint -Bucket $bucket -AccessKey $accessKey -SecretKey $secretKey
            if (-not $healthy) {
                $script:FailureCode = 1
                throw 'object storage is not ready'
            }
        }
        'init' {
            Assert-Tool 'mc' 'Install the MinIO client and put mc.exe on PATH.'
            Write-Note "Bucket versioning is required: publication stores exact object versions and the orphan sweep deletes them."
            Invoke-Step 'mc' @('alias', 'set', $StorageAlias, $endpoint, $accessKey, $secretKey) | Out-Null
            Invoke-Step 'mc' @('mb', '--ignore-existing', "$StorageAlias/$bucket") | Out-Null
            Invoke-Step 'mc' @('version', 'enable', "$StorageAlias/$bucket") | Out-Null
            Write-Host ''
            Write-Ok "$bucket is ready with versioning enabled."
        }
        'test' {
            $env:RUX_DATABASE_URL = Get-ConfigValue $config 'database.url'
            $env:RUX_STORAGE_ENDPOINT = $endpoint
            $env:RUX_STORAGE_BUCKET = $bucket
            $env:RUX_STORAGE_ACCESS_KEY = $accessKey
            $env:RUX_STORAGE_SECRET_KEY = $secretKey
            $env:RUX_STORAGE_REGION = $region
            $env:RUX_STORAGE_FORCE_PATH_STYLE = $pathStyle
            Write-Note "Live bucket test against $endpoint/$bucket"
            Invoke-Step 'cargo' @('test', '-p', 'rux-infrastructure', '--test', 'object_storage', '--', '--ignored') | Out-Null
        }
        default { throw "Unknown storage subcommand '$action'. Try check, init, or test." }
    }
}

function Invoke-Playground {
    param([string[]] $Tail)

    $action = if ($Tail.Count -gt 0) { $Tail[0].ToLowerInvariant() } else { 'build' }
    $rest = Get-Tail $Tail 1

    switch ($action) {
        'build' {
            $bash = Get-BashPath
            if (-not $bash) { throw 'No bash found. Install Git for Windows, or set RUX_BASH to a bash executable.' }
            Assert-Tool 'docker' 'Start Docker Desktop.'

            $version = if ($rest.Count -ge 1) { $rest[0] } else { $PlaygroundVersion }
            $checksum = if ($rest.Count -ge 2) { $rest[1] } else { $PlaygroundSha256 }
            $packages = if ($rest.Count -ge 3) { $rest[2..($rest.Count - 1)] -join ' ' } else { $null }

            $scriptArguments = @('playground/build-image.sh', $version, $checksum)
            if ($packages) { $scriptArguments += $packages }

            Invoke-Step $bash $scriptArguments | Out-Null
        }
        'test' {
            $bash = Get-BashPath
            if (-not $bash) { throw 'No bash found. Install Git for Windows, or set RUX_BASH to a bash executable.' }
            Assert-Tool 'docker' 'Start Docker Desktop.'

            $image = if ($rest.Count -ge 1) { $rest[0] } else { "rux-playground:$PlaygroundVersion" }

            # Fixtures are bind-mounted, so they must sit somewhere the Docker
            # daemon can resolve - which a Git Bash /tmp is not.
            New-Item -ItemType Directory -Path $PlaygroundTestRoot -Force | Out-Null
            $env:RUX_PLAYGROUND_TEST_ROOT = $PlaygroundTestRoot -replace '\\', '/'
            Write-Note "RUX_PLAYGROUND_TEST_ROOT=$($env:RUX_PLAYGROUND_TEST_ROOT)"

            Invoke-Step $bash @('playground/test-image.sh', $image) | Out-Null
        }
        'docker-test' {
            Assert-Tool 'docker' 'Start Docker Desktop.'
            $env:RUX_PLAYGROUND_IMAGE = "rux-playground:$PlaygroundVersion"
            Write-Note "RUX_PLAYGROUND_IMAGE=$($env:RUX_PLAYGROUND_IMAGE)"
            Invoke-Step 'cargo' @('test', '-p', 'rux-sandbox', '--test', 'docker', '--', '--ignored') | Out-Null
        }
        default { throw "Unknown playground subcommand '$action'. Try build, test, or docker-test." }
    }
}

function Invoke-Config {
    param([string[]] $Tail)

    $action = if ($Tail.Count -gt 0) { $Tail[0].ToLowerInvariant() } else { 'path' }

    switch ($action) {
        'path' {
            $path = Resolve-ConfigPath
            Write-Heading 'Configuration'
            $missing = if (Test-Path -LiteralPath $UserConfigPath) { '' } else { '   (does not exist - ./Run.ps1 config init)' }
            Write-Host "  In use:     $path"
            Write-Host "  Yours:      $UserConfigPath$missing"
            Write-Host "  Committed:  $DefaultConfigPath"
            Write-Host ''
            Write-Note '  Passed to both binaries as --config <path>.'
        }
        'show' {
            $path = Resolve-ConfigPath
            Write-Heading $path
            Get-Content -LiteralPath $path | ForEach-Object { Write-Host "  $_" }
        }
        'init' {
            if (Test-Path -LiteralPath $UserConfigPath) {
                Write-Warn "$UserConfigPath already exists." 'Refusing to overwrite it. Edit it with ./Run.ps1 config edit.'
                return
            }

            $parent = Split-Path -Parent $UserConfigPath
            New-Item -ItemType Directory -Path $parent -Force | Out-Null
            Copy-Item -LiteralPath $DefaultConfigPath -Destination $UserConfigPath

            Write-Ok "Created $UserConfigPath"
            Write-Note '  A whole configuration, not an overlay - the server does no layering.'
            Write-Note '  Put your real GitHub OAuth client id and secret in it; every command'
            Write-Note '  in this script will pick it up from now on.'
        }
        'edit' {
            $path = Resolve-ConfigPath
            $editor = if ($env:EDITOR) { $env:EDITOR } else { 'notepad' }
            Invoke-Step $editor @($path) | Out-Null
        }
        default { throw "Unknown config subcommand '$action'. Try path, show, init, or edit." }
    }
}

function Invoke-Clean {
    Invoke-Step 'cargo' @('clean') | Out-Null

    if (Test-Path -LiteralPath $PlaygroundTestRoot) {
        Remove-Item -LiteralPath $PlaygroundTestRoot -Recurse -Force
        Write-Ok "Removed $PlaygroundTestRoot"
    }
}

# ---------------------------------------------------------------------------
# doctor
# ---------------------------------------------------------------------------

function Test-Storage {
    param(
        [Parameter(Mandatory)][string] $Endpoint,
        [Parameter(Mandatory)][string] $Bucket,
        [string] $AccessKey,
        [string] $SecretKey
    )

    $health = Invoke-HttpRequest -Uri "$Endpoint/minio/health/live" -TimeoutSeconds 5
    if ($health.Failed) {
        Write-Fail "Object storage is not answering at $Endpoint" 'Start MinIO, then ./Run.ps1 storage init'
        return $false
    }
    Write-Ok "Object storage answering at $Endpoint"

    if (-not (Test-Tool 'mc')) {
        Write-Warn 'mc is not on PATH, so the bucket and its versioning cannot be checked.' 'Install the MinIO client to use ./Run.ps1 storage init.'
        return $true
    }

    & mc alias set $StorageAlias $Endpoint $AccessKey $SecretKey *> $null
    if ($LASTEXITCODE -ne 0) {
        Write-Fail 'mc could not authenticate against the endpoint.' 'Check storage.access_key and storage.secret_key in the configuration.'
        return $false
    }

    & mc ls "$StorageAlias/$Bucket" *> $null
    if ($LASTEXITCODE -ne 0) {
        Write-Fail "Bucket '$Bucket' does not exist." './Run.ps1 storage init'
        return $false
    }
    Write-Ok "Bucket '$Bucket' exists"

    $versioning = (& mc version info "$StorageAlias/$Bucket" 2>&1) -join ' '
    if ($versioning -match '(?i)enabled') {
        Write-Ok "Versioning is enabled on '$Bucket'"
        return $true
    }

    Write-Fail "Versioning is not enabled on '$Bucket'." 'Publication stores exact object versions; run ./Run.ps1 storage init'
    return $false
}

function Invoke-Doctor {
    $config = Get-Config

    Write-Heading 'Toolchain'

    if (Test-Tool 'cargo') {
        $rustc = (& rustc --version) -join ''
        if ($rustc -match '(\d+\.\d+\.\d+)') {
            $version = [version] $Matches[1]
            if ($version -ge $MsrvMinimum) { Write-Ok "$rustc (MSRV $MsrvMinimum)" }
            else { Write-Fail "$rustc is below the MSRV $MsrvMinimum" 'rustup update stable' }
        }
        else {
            Write-Warn "Could not read a version out of '$rustc'."
        }
    }
    else {
        Write-Fail 'cargo is not on PATH.' 'Install Rust from https://rustup.rs'
    }

    $locked = Get-LockedVersion 'sqlx'
    if (Test-Tool 'sqlx') {
        $installed = (& sqlx --version) -join ''
        if (-not $locked) { Write-Warn "sqlx-cli $installed is installed, but Cargo.lock has no sqlx entry to check it against." }
        elseif ($installed -match [regex]::Escape($locked)) { Write-Ok "sqlx-cli $installed matches the lockfile ($locked)" }
        else { Write-Warn "sqlx-cli is $installed but the lockfile pins $locked." './Run.ps1 migrate install' }
    }
    else {
        Write-Fail 'sqlx-cli is not on PATH.' './Run.ps1 migrate install'
    }

    foreach ($optional in @(
        @{ Name = 'docker'; Need = 'the playground image build and its containment suite' },
        @{ Name = 'mc'; Need = 'creating the bucket and checking its versioning' }
    )) {
        if (Test-Tool $optional.Name) { Write-Ok "$($optional.Name) present" }
        else { Write-Warn "$($optional.Name) is not on PATH." "Only needed for $($optional.Need)." }
    }

    $bash = Get-BashPath
    if (-not $bash) {
        Write-Warn 'No bash found.' 'Only needed for the playground scripts; install Git for Windows or set RUX_BASH.'
    }
    elseif ($bash -like '*\System32\bash.exe') {
        Write-Warn "bash resolves to the WSL stub ($bash)." 'The playground scripts want Git Bash; set RUX_BASH to its bash.exe.'
    }
    else {
        Write-Ok "bash: $bash"
    }

    Write-Heading 'Configuration'
    Write-Host "  Using $($config['_path'])"

    if ($config['_path'] -eq $DefaultConfigPath) {
        Write-Warn 'This is the committed development file.' "Keep your own credentials in $UserConfigPath - ./Run.ps1 config init"
    }
    else {
        Write-Ok 'Using your own configuration file.'
        $committed = Get-Item -LiteralPath $DefaultConfigPath
        $mine = Get-Item -LiteralPath $config['_path']
        if ($committed.LastWriteTimeUtc -gt $mine.LastWriteTimeUtc) {
            Write-Warn 'config\config.toml is newer than your copy.' 'A key may have been added upstream; unknown or missing keys are a startup error. Diff the two.'
        }
    }

    $clientId = Get-ConfigValue $config 'github.client_id'
    if ($clientId -like 'replace-with-*') {
        Write-Warn 'github.client_id is still the placeholder.' 'Browser OAuth will not work until it is a real client id.'
    }

    $playgroundEnabled = Get-ConfigValue $config 'playground.api.enabled' 'false'
    if ($playgroundEnabled -eq 'true') { Write-Ok 'playground.api.enabled is true' }
    else { Write-Note '  playground.api.enabled is false, so /v1/playground/* answers 404 from the fallback.' }

    Write-Heading 'PostgreSQL'

    $databaseUrl = Get-ConfigValue $config 'database.url'
    $parsed = if ($databaseUrl) { Split-DatabaseUrl $databaseUrl } else { $null }

    if (-not $parsed) {
        Write-Fail 'Could not read database.url from the configuration.' 'It is required; see config\config.toml.'
    }
    elseif (-not (Test-TcpPort -Server $parsed.Server -Port $parsed.Port)) {
        Write-Fail "Nothing is listening on $($parsed.Server):$($parsed.Port)." 'Start the PostgreSQL 18 service.'
    }
    else {
        Write-Ok "PostgreSQL answering on $($parsed.Server):$($parsed.Port)"

        if (Test-Tool 'psql') {
            $previous = $env:PGPASSWORD
            $env:PGPASSWORD = $parsed.Password
            try {
                $answer = (& psql -h $parsed.Server -p $parsed.Port -U $parsed.User -d $parsed.Database -tAc 'select rolcreatedb from pg_roles where rolname = current_user' 2>&1) -join ''
                if ($LASTEXITCODE -ne 0) {
                    Write-Fail "Could not connect to database '$($parsed.Database)' as '$($parsed.User)'." $answer.Trim()
                }
                elseif ($answer.Trim() -eq 't') {
                    Write-Ok "Database '$($parsed.Database)' reachable, and '$($parsed.User)' may create databases"
                }
                else {
                    Write-Warn "'$($parsed.User)' may not create databases." 'The #[sqlx::test] integration tests create one per test and will fail.'
                }
            }
            finally {
                $env:PGPASSWORD = $previous
            }
        }
        else {
            Write-Warn 'psql is not on PATH, so the database and role were not checked.' 'Only the port was probed.'
        }
    }

    Write-Heading 'Object storage'
    Test-Storage `
        -Endpoint (Get-ConfigValue $config 'storage.endpoint' 'http://localhost:9000') `
        -Bucket (Get-ConfigValue $config 'storage.bucket' 'packages') `
        -AccessKey (Get-ConfigValue $config 'storage.access_key') `
        -SecretKey (Get-ConfigValue $config 'storage.secret_key') | Out-Null

    Write-Heading 'API'
    $base = Get-ApiBaseUrl $config
    $live = Invoke-HttpRequest -Uri "$base/health/live" -TimeoutSeconds 3
    if ($live.Failed) { Write-Note "  Not running on $base. Start it with ./Run.ps1 dev." }
    else { Write-Ok "Running on $base - ./Run.ps1 smoke will check its contracts" }

    Write-Host ''
}

# ---------------------------------------------------------------------------
# smoke
# ---------------------------------------------------------------------------

function Invoke-Smoke {
    $config = Get-Config
    $base = Get-ApiBaseUrl $config
    $origin = Get-ConfigValue $config 'web.allowed_origin' 'http://localhost:3000'
    $failures = 0

    # Outlast the server's own request timeout: an unreachable dependency makes
    # /health/ready answer 504 at that bound, and that answer is the diagnosis.
    $patience = [int] (Get-ConfigValue $config 'abuse.request_timeout_seconds' '30') + 15

    Write-Heading "Contract checks against $base"

    $live = Invoke-HttpRequest -Uri "$base/health/live"
    if ($live.Failed) {
        Write-Fail "/health/live is not answering." 'Start the API with ./Run.ps1 dev'
        $script:FailureCode = 1
        throw 'the API is not running'
    }
    if ($live.Content -match '"status":"healthy"') { Write-Ok '/health/live reports healthy' }
    else { Write-Fail '/health/live is not healthy' $live.Content; $failures++ }

    $ready = Invoke-HttpRequest -Uri "$base/health/ready" -TimeoutSeconds $patience
    $readyDetail = "HTTP $($ready.Status) $($ready.Content)".Trim()
    if ($ready.Content -match '"status":"healthy"') { Write-Ok '/health/ready reports healthy' }
    else { Write-Fail '/health/ready is not healthy' "$readyDetail - is PostgreSQL up, and MinIO with the packages bucket?"; $failures++ }

    foreach ($dependency in @('postgresql', 'object_storage')) {
        if ($ready.Content -match "`"$dependency`"") { Write-Ok "/health/ready names $dependency" }
        else { Write-Fail "/health/ready does not name $dependency" $readyDetail; $failures++ }
    }

    $openapi = Invoke-HttpRequest -Uri "$base/openapi/v1.json"
    if ($openapi.Content -match '"openapi"') { Write-Ok '/openapi/v1.json is served' }
    else { Write-Fail '/openapi/v1.json is missing or malformed' $openapi.Error; $failures++ }

    if ((Get-ConfigValue $config 'playground.api.enabled' 'false') -ne 'true') {
        Write-Note '  Skipping the playground contract: playground.api.enabled is false.'
    }
    else {
        $failures += Test-PlaygroundContract -Base $base -Origin $origin
    }

    Write-Host ''
    if ($failures -eq 0) {
        Write-Ok 'Every contract check passed.'
        return
    }

    Write-Fail "$failures contract check(s) failed."
    $script:FailureCode = 1
    throw 'smoke failed'
}

function Test-PlaygroundContract {
    param([Parameter(Mandatory)][string] $Base, [Parameter(Mandatory)][string] $Origin)

    $failures = 0
    $url = "$Base/v1/playground/run"
    $headers = @{ 'content-type' = 'application/json'; 'origin' = $Origin }

    $program = @{
        mode    = 'run'
        profile = 'debug'
        source  = "func Main() -> int {`n    return 0;`n}`n"
    } | ConvertTo-Json -Compress

    $response = Invoke-HttpRequest -Uri $url -Method 'Post' -Headers $headers -Body $program -TimeoutSeconds 60
    if ($response.Failed) {
        Write-Fail 'A valid program did not run.' $response.Error
        $failures++
    }
    else {
        $payload = $response.Content | ConvertFrom-Json
        if ($payload.data.build.success -and $payload.data.program.exit_code -eq 0 -and -not $payload.data.program.timed_out) {
            Write-Ok 'A valid program builds and exits 0'
        }
        else {
            Write-Fail 'A valid program did not build and exit 0' $response.Content
            $failures++
        }
    }

    $broken = @{ mode = 'build'; source = "func Main( -> int {}`n" } | ConvertTo-Json -Compress
    $response = Invoke-HttpRequest -Uri $url -Method 'Post' -Headers $headers -Body $broken -TimeoutSeconds 60
    if (-not $response.Failed) {
        $payload = $response.Content | ConvertFrom-Json
        if (-not $payload.data.build.success -and $payload.data.build.diagnostics.Count -gt 0) {
            Write-Ok 'A syntax error is reported as diagnostics'
        }
        else {
            Write-Fail 'A syntax error did not produce diagnostics' $response.Content
            $failures++
        }
    }
    else {
        Write-Fail 'The build-failure case did not answer' $response.Error
        $failures++
    }

    $response = Invoke-HttpRequest -Uri $url -Method 'Post' -Headers @{ 'content-type' = 'application/json' } -Body $program -TimeoutSeconds 60
    if ($response.Status -eq 403) { Write-Ok 'A request without an Origin header is refused with 403' }
    else { Write-Fail "A request without an Origin header returned $($response.Status), not 403"; $failures++ }

    $limits = Invoke-HttpRequest -Uri "$Base/v1/playground/limits"
    if (-not $limits.Failed -and ($limits.Content | ConvertFrom-Json).data.max_source_bytes -eq 32768) {
        Write-Ok '/v1/playground/limits reports max_source_bytes 32768'
    }
    else {
        Write-Fail '/v1/playground/limits is wrong or missing' $limits.Content
        $failures++
    }

    return $failures
}

# ---------------------------------------------------------------------------
# Dispatch
# ---------------------------------------------------------------------------

function Resolve-CommandName {
    param([Parameter(Mandatory)][string] $Token)

    $name = $Token.TrimStart('-', '/').ToLowerInvariant()
    if ($Aliases.ContainsKey($name)) { return $Aliases[$name] }
    return $name
}

# -Config is pulled out wherever it appears, so it can follow the command's own
# arguments without being forwarded to cargo.
function Split-ConfigArgument {
    param([string[]] $Tokens)

    $rest = @()

    for ($index = 0; $index -lt $Tokens.Count; $index++) {
        $token = $Tokens[$index]

        if ($token -match '^(?i)[-/]{1,2}config=(.+)$') {
            $script:ConfigOverride = $Matches[1]
            continue
        }

        if ($token -match '^(?i)[-/]{1,2}config$') {
            if ($index + 1 -ge $Tokens.Count) { throw '-Config needs a path' }
            $script:ConfigOverride = $Tokens[$index + 1]
            $index++
            continue
        }

        $rest += $token
    }

    return , $rest
}

$tokens = @()
if ($Arguments) { $tokens = @($Arguments | ForEach-Object { [string] $_ }) }
$tokens = Split-ConfigArgument $tokens

$command = if ($tokens.Count -gt 0) { Resolve-CommandName $tokens[0] } else { 'help' }
$tail = Get-Tail $tokens 1
$topic = if ($tail.Count -gt 0) { $tail[0] } else { '' }

Push-Location $RepoRoot
try {
    switch ($command) {
        'help' { Show-Help $topic }
        'doctor' { Invoke-Doctor }
        'config' { Invoke-Config $tail }
        'fmt' { Invoke-Fmt $tail }
        'lint' { Invoke-Lint $tail }
        'test' { Invoke-Test $tail }
        'build' { Invoke-BuildCommand $tail }
        'check' { Invoke-Gates }
        'ci' { Invoke-Ci }
        'dev' { Invoke-Dev $tail }
        'broker' { Invoke-Broker $tail }
        'migrate' { Invoke-Migrate $tail }
        'storage' { Invoke-Storage $tail }
        'playground' { Invoke-Playground $tail }
        'smoke' { Invoke-Smoke }
        'clean' { Invoke-Clean }
        default {
            Write-Fail "Unknown command '$($tokens[0])'." 'Run ./Run.ps1 for the full list.'
            exit 64
        }
    }
}
catch {
    Write-Host ''
    Write-Host "Run.ps1: $($_.Exception.Message)" -ForegroundColor Red
    exit $script:FailureCode
}
finally {
    Pop-Location
}

exit 0
