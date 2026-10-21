[CmdletBinding()]
param(
    [switch]$Interactive,
    [string]$Profile = $env:METRIC_PROFILE,
    [string]$Backend = $env:METRIC_DATABASE_BACKEND,
    [string]$Database = $env:METRIC_DATABASE_MODE,
    [string]$InstallDir = $env:METRIC_INSTALL_DIR,
    [string]$HttpPort = $env:METRIC_HTTP_PORT,
    [string]$Tls = $env:METRIC_DATABASE_TLS,
    [string]$TlsCa = $env:METRIC_DATABASE_TLS_CA,
    [string]$TlsClientCert = $env:METRIC_DATABASE_TLS_CLIENT_CERT,
    [string]$TlsClientKey = $env:METRIC_DATABASE_TLS_CLIENT_KEY,
    [switch]$Help
)
$ErrorActionPreference = 'Stop'
if ($Help) {
    Write-Output @'
Metric installer (0.1.9 source; new options require this updated script)
Without options: unattended Medium profile with bundled MongoDB.
 -Interactive                      METRIC_INTERACTIVE=1
 -Profile min|low|medium|high        METRIC_PROFILE
 -Backend mongodb|postgres          METRIC_DATABASE_BACKEND
 -Database bundled|external         METRIC_DATABASE_MODE
 -InstallDir PATH                   METRIC_INSTALL_DIR
 -HttpPort PORT                     METRIC_HTTP_PORT
 -Tls on|off                        METRIC_DATABASE_TLS (external only)
 -TlsCa PATH                        METRIC_DATABASE_TLS_CA
 -TlsClientCert PATH                METRIC_DATABASE_TLS_CLIENT_CERT
 -TlsClientKey PATH                 METRIC_DATABASE_TLS_CLIENT_KEY
 -Help
Also: METRIC_VERSION, METRIC_DOWNLOAD_BASE, METRIC_IMAGE.
External credentials: METRIC_POSTGRES_URI or MONGODB_URI (environment only).
Mongo database: METRIC_MONGO_DATABASE, otherwise URI database or metric.
URI input is visible. Existing configuration and secrets are retained.
'@
    return
}
$version = if ($env:METRIC_VERSION) { $env:METRIC_VERSION } else { '0.1.9' }
$downloadBase = if ($env:METRIC_DOWNLOAD_BASE) { $env:METRIC_DOWNLOAD_BASE } else {
    "https://raw.githubusercontent.com/biosshot/metric/v$version/deploy"
}
if (-not $Profile) { $Profile = 'medium' }
if (-not $HttpPort) { $HttpPort = '4001' }
if (-not $InstallDir) {
    $InstallDir = if ((Test-Path -LiteralPath './compose.yml') -and (
        (Test-Path -LiteralPath './metric.toml') -or (Test-Path -LiteralPath './.env'))) { '.' } else { 'metric' }
}
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
$envFile = Join-Path $InstallDir '.env'
$utf8 = [Text.UTF8Encoding]::new($false)
function Read-EnvValue([string]$Path, [string]$Name) {
    foreach ($line in [IO.File]::ReadAllLines($Path)) {
        if ($line.StartsWith("$Name=")) {
            return $line.Substring($Name.Length + 1).Trim("'")
        }
    }
    return ''
}
function Write-Text([string]$Path, [string]$Content) {
    [IO.File]::WriteAllText($Path, $Content.Replace("`r`n", "`n"), $utf8)
}
function Env-Line([string]$Name, [string]$Value) {
    if ($Value.IndexOfAny([char[]]"`r`n") -ge 0 -or $Value.Contains([string][char]0)) {
        throw 'Installation environment values must be one line.'
    }
    return "$Name='" + $Value.Replace("'", "\'") + "'`n"
}
function Protect-Private([string]$Path, [bool]$Directory = $false) {
    if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
        $acl = Get-Acl -LiteralPath $Path
        $acl.SetAccessRuleProtection($true, $false)
        $identity = [Security.Principal.WindowsIdentity]::GetCurrent().User
        if ($Directory) {
            $rule = [Security.AccessControl.FileSystemAccessRule]::new($identity, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
        } else { $rule = [Security.AccessControl.FileSystemAccessRule]::new($identity, 'FullControl', 'Allow') }
        $acl.AddAccessRule($rule)
        Set-Acl -LiteralPath $Path -AclObject $acl
    } else {
        & chmod $(if ($Directory) { '700' } else { '600' }) -- $Path
        if ($LASTEXITCODE -ne 0) { throw 'Cannot protect installation secrets.' }
    }
}
$existing = Test-Path -LiteralPath $envFile
if ($existing) {
    $savedBackend = Read-EnvValue $envFile METRIC_DATABASE_BACKEND
    $savedMode = Read-EnvValue $envFile METRIC_DATABASE_MODE
    if (-not $savedBackend) { $savedBackend = 'mongodb' }
    if (-not $savedMode) { $savedMode = 'bundled' }
    if ($Backend -and $Backend -ne $savedBackend) { throw 'Existing installation keeps its backend. Switching databases is not a migration.' }
    if ($Database -and $Database -ne $savedMode) { throw 'Existing installation keeps its database placement.' }
    $Backend = $savedBackend; $Database = $savedMode
}
if (-not $Backend) { $Backend = 'mongodb' }
if (-not $Database) { $Database = 'bundled' }
if ($env:METRIC_INTERACTIVE -and $env:METRIC_INTERACTIVE -notin @('0', '1')) { throw 'METRIC_INTERACTIVE must be 0 or 1.' }
$wizard = $Interactive -or $env:METRIC_INTERACTIVE -eq '1'
function Ask([string]$Label, [string]$Default) {
    $response = Read-Host "$Label [$Default]"
    if ($response) { return $response }
    return $Default
}
$uri = if ($Backend -eq 'postgres') { $env:METRIC_POSTGRES_URI } else { $env:MONGODB_URI }
if ($wizard -and -not $existing) {
    if ([Console]::IsInputRedirected) { throw 'Interactive setup needs a terminal. Supply environment values instead.' }
    Write-Host 'Metric setup'
    $Backend = Ask 'Database (mongodb/postgres)' $Backend
    $Database = Ask 'Database placement (bundled/external)' $Database
    $uri = if ($Backend -eq 'postgres') { $env:METRIC_POSTGRES_URI } else { $env:MONGODB_URI }
    if ($Database -eq 'external') {
        if (-not $uri) { $uri = Ask 'Connection URI (input is visible)' '' }
        else { Write-Host 'Using connection URI from environment (credentials omitted).' }
    }
    $Profile = Ask 'Resource profile (min/low/medium/high)' $Profile
    $HttpPort = Ask 'Metric HTTP port (published on all host interfaces)' $HttpPort
}
$Profile = $Profile.ToLowerInvariant()
$Backend = $Backend.ToLowerInvariant()
$Database = $Database.ToLowerInvariant()
if ($Tls) { $Tls = $Tls.ToLowerInvariant() }
if ($Profile -notin @('min', 'low', 'medium', 'high')) { throw 'METRIC_PROFILE must be min, low, medium or high.' }
if ($Backend -notin @('mongodb', 'postgres')) { throw 'Database backend must be mongodb or postgres.' }
if ($Database -notin @('bundled', 'external')) { throw 'Database placement must be bundled or external.' }
if (-not $existing -and $Database -eq 'bundled' -and ($Tls -or $TlsCa -or $TlsClientCert -or $TlsClientKey)) {
    throw 'TLS options configure an existing database only. The bundled database uses internal Docker TCP.'
}
$portNumber = 0
if (-not [int]::TryParse($HttpPort, [ref]$portNumber) -or $portNumber -lt 1 -or $portNumber -gt 65535) { throw 'HTTP port must be from 1 to 65535.' }
function Query-Value([string]$Name) {
    $pieces = $uri.Split('?', 2)
    if ($pieces.Count -eq 2) {
        foreach ($item in $pieces[1].Split([char[]]'&;')) {
            $pair = $item.Split('=', 2)
            if ($pair.Count -eq 2 -and $pair[0] -ieq $Name) { return $pair[1] }
        }
    }
    return ''
}
$mongoDatabase = $env:METRIC_MONGO_DATABASE
$pgSsl = 'verify-full'
if ($Database -eq 'external' -and -not $existing) {
    if (-not $uri) { throw 'External database requires METRIC_POSTGRES_URI or MONGODB_URI.' }
    if ($uri.IndexOfAny([char[]]"`r`n") -ge 0 -or $uri.Contains([string][char]0)) { throw 'Connection URI must be one line.' }
    if (($Backend -eq 'postgres' -and $uri -notmatch '^postgres(ql)?://') -or
        ($Backend -eq 'mongodb' -and $uri -notmatch '^mongodb(\+srv)?://')) { throw 'Connection URI scheme does not match the selected backend.' }
    $inferred = ''
    if ($Backend -eq 'postgres') {
        $uriTls = Query-Value sslmode
        if ($uriTls) {
            if ($uriTls -notin @('disable', 'require', 'verify-ca', 'verify-full')) { throw 'Unsupported PostgreSQL sslmode.' }
            $pgSsl = $uriTls
            $inferred = if ($uriTls -eq 'disable') { 'off' } else { 'on' }
        }
    } else {
        if (-not $mongoDatabase) {
            if ($uri -match '^[^:]+://[^/]+/([^?]+)') { $mongoDatabase = [Uri]::UnescapeDataString($Matches[1]) }
            elseif ($wizard) { $mongoDatabase = Ask 'Mongo database for Metric data' 'metric' }
            else { $mongoDatabase = 'metric' }
        }
        $uriTls = Query-Value tls
        if (-not $uriTls) { $uriTls = Query-Value ssl }
        switch ($uriTls) {
            'true' { $inferred = 'on' }
            'false' { $inferred = 'off' }
            '' { if ($uri.StartsWith('mongodb+srv://')) { $inferred = 'on' } }
            default { throw 'Mongo TLS must be true or false.' }
        }
        if (-not $TlsCa) { $TlsCa = [Uri]::UnescapeDataString((Query-Value tlsCAFile)) }
        if (-not $TlsClientCert) { $TlsClientCert = [Uri]::UnescapeDataString((Query-Value tlsCertificateKeyFile)) }
    }
    if ($Tls -and $inferred -and $Tls -ne $inferred) { throw 'URI TLS setting conflicts with the selected TLS mode.' }
    if (-not $Tls) { $Tls = $inferred }
    if (-not $Tls -and $wizard) { $Tls = Ask 'Database TLS (on=verified TLS, off=plain TCP)' 'on' }
    if (-not $Tls) { $Tls = 'on' }
    if ($Tls -notin @('on', 'off')) { throw 'TLS must be on or off.' }
    if ($Tls -eq 'on' -and $wizard) {
        if (-not $TlsCa) { $TlsCa = Ask 'Custom CA file (Enter for standard trust)' '' }
        if (-not $TlsClientCert) { $TlsClientCert = Ask 'Client certificate file (Enter if not required)' '' }
        if ($TlsClientCert -and -not $TlsClientKey) { $TlsClientKey = Ask 'Separate client key file (Mongo combined PEM: Enter)' '' }
    }
    if ($Tls -eq 'off') {
        if ($TlsCa -or $TlsClientCert -or $TlsClientKey) { throw 'Certificate files cannot be used with TLS off.' }
        $pgSsl = 'disable'
    }
}
if (-not (Get-Command docker -ErrorAction SilentlyContinue)) { throw 'Docker is required.' }
function Docker-Run([string[]]$Arguments) {
    & docker @Arguments
    if ($LASTEXITCODE -ne 0) { throw 'Docker operation failed. Check configuration, database reachability and container status.' }
}
Docker-Run @('compose', 'version')
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
if (-not $existing) {
    # Listing succeeds for an empty daemon, unlike inspect of a missing volume.
    # It also avoids PowerShell 5.1 treating expected native stderr as terminating.
    $volumes = @(Docker-Run @('volume', 'ls', '--format', '{{.Name}}'))
    if ($volumes -contains 'metric_mongo-data') { throw 'MongoDB data already exists, but .env is missing. Restore the original .env. The installer will not generate a different password for existing data.' }
    $project = if ($env:COMPOSE_PROJECT_NAME) { $env:COMPOSE_PROJECT_NAME } else { 'metric' }
    foreach ($volume in @('mongo-data', 'postgres-data', 'blob-data')) {
        if ($volumes -contains "${project}_$volume") { throw 'Installation data exists, but .env is missing. Restore the original secrets.' }
    }
}
function Random-Hex([int]$Count) {
    $bytes = New-Object byte[] $Count
    $random = [Security.Cryptography.RandomNumberGenerator]::Create()
    try { $random.GetBytes($bytes) } finally { $random.Dispose() }
    return [BitConverter]::ToString($bytes).Replace('-', '').ToLowerInvariant()
}
if (-not $existing) {
    $stage = Join-Path $InstallDir ('.install.' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stage | Out-Null
    Protect-Private $stage $true
    try {
        $composeAsset = if ($Backend -eq 'mongodb' -and $Database -eq 'bundled') { 'compose.yml' } else { "compose.$Backend.$Database.yml" }
        $configAsset = if ($Backend -eq 'mongodb') { "profiles/$Profile.toml" } else { "profiles/postgres/$Profile.toml" }
        foreach ($asset in @(@($composeAsset, 'compose.yml'), @($configAsset, 'metric.toml'), @('symbolicator.yml', 'symbolicator.yml'), @("profiles/$Profile.env.example", 'template.env'))) {
            Invoke-WebRequest -UseBasicParsing -Uri "$downloadBase/$($asset[0])" -OutFile (Join-Path $stage $asset[1]) | Out-Null
        }
        $hmac = if ($env:METRIC_SCRUB_HMAC_KEY) { $env:METRIC_SCRUB_HMAC_KEY } else { Random-Hex 32 }
        if ($hmac -cnotmatch '^[0-9a-f]{64}$') { throw 'HMAC key must contain 64 lowercase hexadecimal characters.' }
        $password = Random-Hex 24
        $template = [IO.File]::ReadAllText((Join-Path $stage 'template.env'))
        $environment = $template.Replace('replace-with-a-long-url-safe-random-password', $password).Replace('replace-with-64-lowercase-hex-characters', $hmac)
        $environment = [regex]::Replace($environment, '(?m)^METRIC_(HTTP_PORT|IMAGE)=.*\r?\n', '')
        $environment += Env-Line METRIC_HTTP_PORT $HttpPort
        $image = if ($env:METRIC_IMAGE) { $env:METRIC_IMAGE } else { "ghcr.io/biosshot/metric:$version" }
        $environment += Env-Line METRIC_IMAGE $image
        $environment += Env-Line METRIC_DATABASE_BACKEND $Backend
        $environment += Env-Line METRIC_DATABASE_MODE $Database
        $generated = @{ Compose = [IO.File]::ReadAllText((Join-Path $stage 'compose.yml')); Connection = '' }
        if ($Backend -eq 'postgres') {
            $environment += Env-Line METRIC_POSTGRES_PASSWORD $password
            $environment += Env-Line METRIC_POSTGRES_MEMORY_LIMIT (Read-EnvValue (Join-Path $stage 'template.env') METRIC_MONGO_MEMORY_LIMIT)
            if ($Database -eq 'bundled') { $pgSsl = 'disable' }
            $generated.Connection += Env-Line APP__POSTGRES__SSL_MODE $pgSsl
            if ($pgSsl -ne 'verify-full') { $generated.Connection += Env-Line APP__POSTGRES__ALLOW_INSECURE_CONNECTION 'true' }
        }
        $config = [IO.File]::ReadAllText((Join-Path $stage 'metric.toml'))
        if ($Backend -eq 'mongodb' -and $Database -eq 'external') {
            $dbQuoted = ConvertTo-Json -InputObject $mongoDatabase -Compress
            $config = [regex]::Replace($config, '(?m)^database = .*$', [Text.RegularExpressions.MatchEvaluator]{ param($m) "database = $dbQuoted" })
            if (-not (Query-Value tls) -and -not (Query-Value ssl)) {
                $separator = if ($uri.Contains('?')) { '&' } else { '?' }
                $enabled = if ($Tls -eq 'on') { 'true' } else { 'false' }
                $uri += "${separator}tls=$enabled"
            }
        }
        $certDirectory = Join-Path $InstallDir 'certs'
        New-Item -ItemType Directory -Path $certDirectory -Force | Out-Null
        Protect-Private $certDirectory $true
        function Mount-Cert([string]$Source, [string]$Name) {
            if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) { throw 'Certificate/key file does not exist or cannot be read.' }
            $destination = Join-Path $certDirectory $Name
            Copy-Item -LiteralPath $Source -Destination $destination
            if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { & chmod 644 -- $destination }
            $line = '      - ./metric.toml:/etc/metric/metric.toml:ro'
            $generated.Compose = $generated.Compose.Replace($line, "$line`n      - ./certs/${Name}:/run/metric-certs/${Name}:ro")
        }
        function Mongo-File([string]$Name, [string]$Value) {
            if (Query-Value $Name) {
                $pattern = '([?&;])' + [regex]::Escape($Name) + '=[^&;]*'
                return [regex]::Replace($uri, $pattern, [Text.RegularExpressions.MatchEvaluator]{ param($m) "$($m.Groups[1].Value)$Name=$Value" }, [Text.RegularExpressions.RegexOptions]::IgnoreCase)
            }
            return "$uri&$Name=$Value"
        }
        if ($TlsCa) {
            Mount-Cert $TlsCa ca.pem
            if ($Backend -eq 'postgres') { $generated.Connection += Env-Line APP__POSTGRES__SSL_ROOT_CERT '/run/metric-certs/ca.pem' }
            else { $uri = Mongo-File tlsCAFile '/run/metric-certs/ca.pem' }
        }
        if ($TlsClientCert) {
            Mount-Cert $TlsClientCert client.pem
            if ($Backend -eq 'postgres') {
                if (-not $TlsClientKey) { throw 'PostgreSQL client certificate requires a separate key file.' }
                Mount-Cert $TlsClientKey client.key
                $generated.Connection += Env-Line PGSSLCERT '/run/metric-certs/client.pem'
                $generated.Connection += Env-Line PGSSLKEY '/run/metric-certs/client.key'
            } else {
                if ($TlsClientKey) { [IO.File]::AppendAllText((Join-Path $certDirectory 'client.pem'), "`n" + [IO.File]::ReadAllText($TlsClientKey), $utf8) }
                $uri = Mongo-File tlsCertificateKeyFile '/run/metric-certs/client.pem'
            }
        } elseif ($TlsClientKey) { throw 'A client key requires a client certificate.' }
        if ($Database -eq 'external') {
            $uri = [regex]::Replace($uri, '([@/])(localhost|127\.0\.0\.1)(?=[:/?]|$)', '${1}host.docker.internal')
            $uriName = if ($Backend -eq 'postgres') { 'METRIC_POSTGRES_URI' } else { 'MONGODB_URI' }
            $environment += Env-Line $uriName $uri
            if ($uri.Contains('host.docker.internal')) {
                $dockerOs = & docker info --format '{{.OperatingSystem}}'
                if ($LASTEXITCODE -ne 0) { throw 'Docker engine is unavailable.' }
                if ($dockerOs -notmatch 'Docker Desktop') { $generated.Compose = $generated.Compose.Replace('  metric:', "  metric:`n    extra_hosts: [`"host.docker.internal:host-gateway`"]") }
            }
        }
        Write-Text (Join-Path $stage 'compose.yml') $generated.Compose
        Write-Text (Join-Path $stage 'metric.toml') $config
        Write-Text (Join-Path $stage 'connection.env') $generated.Connection
        Write-Text (Join-Path $stage '.env') $environment
        foreach ($file in @('compose.yml', 'metric.toml', 'symbolicator.yml')) {
            $destination = Join-Path $InstallDir $file
            if (-not (Test-Path -LiteralPath $destination)) {
                Copy-Item -LiteralPath (Join-Path $stage $file) -Destination $destination
                # Config files contain references only and must be readable by the app.
                if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { & chmod 644 -- $destination }
                else { $acl = Get-Acl -LiteralPath $destination; $acl.SetAccessRuleProtection($false, $true); Set-Acl -LiteralPath $destination -AclObject $acl }
            }
        }
        foreach ($file in @('connection.env', '.env')) {
            $destination = Join-Path $InstallDir $file
            Copy-Item -LiteralPath (Join-Path $stage $file) -Destination $destination
            Protect-Private $destination
        }
    } finally {
        # Checked absolute staging path, confined to this install invocation.
        $stageResolved = [IO.Path]::GetFullPath($stage)
        if ($stageResolved.StartsWith($InstallDir.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar) -and
            [IO.Path]::GetFileName($stageResolved) -match '^\.install\.[0-9a-f]{32}$') {
            Remove-Item -LiteralPath $stageResolved -Recurse -Force
        }
    }
}
$activeProfile = Read-EnvValue $envFile METRIC_PROFILE
if ($activeProfile -ne $Profile) { Write-Warning "Existing installation keeps profile $activeProfile; requested $Profile was not applied." }
Push-Location $InstallDir
$composeInputs = @('MONGODB_URI', 'METRIC_POSTGRES_URI', 'METRIC_HTTP_PORT', 'METRIC_IMAGE', 'METRIC_SCRUB_HMAC_KEY', 'METRIC_MONGO_PASSWORD', 'METRIC_POSTGRES_PASSWORD')
$previousInputs = @{}
foreach ($name in $composeInputs) {
    $previousInputs[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
    Remove-Item -LiteralPath ("Env:" + $name) -ErrorAction SilentlyContinue
}
try {
    Docker-Run @('compose', 'config', '--quiet')
    Docker-Run @('compose', 'pull')
    Docker-Run @('compose', 'run', '--rm', '--no-deps', 'metric', '--config', '/etc/metric/metric.toml', '--check-config')
    Docker-Run @('compose', 'up', '-d', '--wait', '--wait-timeout', '120')
    Docker-Run @('compose', 'ps')
} finally {
    foreach ($name in $composeInputs) {
        if ($null -eq $previousInputs[$name]) { Remove-Item -LiteralPath ("Env:" + $name) -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $previousInputs[$name], 'Process') }
    }
    Pop-Location
}
$activePort = Read-EnvValue $envFile METRIC_HTTP_PORT
Write-Host "`nProfile: $activeProfile`nDatabase: $Backend ($Database)`nMetric is ready at http://localhost:$activePort"
Write-Host "Configuration: $(Join-Path $InstallDir 'metric.toml')"
Write-Host "First setup token: cd $InstallDir; docker compose logs metric"
