# Windows worker helpers for Control Tower jobs.
#
# A job spans two privilege contexts on a disposable GCE Windows VM:
# 1. LocalSystem startup installs Friendzone's public root CA into LocalMachine\Root.
# 2. The interactive desktop user (`qa`) prepares an isolated job directory,
#    completes Friendzone user setup, runs `qbt` and interactive `cline` against
#    that directory, stops `qbt` cleanly, packages a single `results.tar.zst`,
#    uploads it, and only then reports completion to Control Tower.

Set-StrictMode -Version Latest

function Write-QwanbanAtomicFile {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][AllowEmptyString()][string]$Text
    )
    $directory = [IO.Path]::GetDirectoryName($Path)
    if (-not [string]::IsNullOrEmpty($directory)) {
        [IO.Directory]::CreateDirectory($directory) | Out-Null
    }
    $isPowerShell = $Path.EndsWith('.ps1', [StringComparison]::OrdinalIgnoreCase)
    if ($isPowerShell) {
        $Text = $Text.Replace("`r`n", "`n").Replace("`n", "`r`n")
    }
    $temporary = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        [IO.File]::WriteAllText($temporary, $Text, (New-Object Text.UTF8Encoding($isPowerShell)))
        if ([IO.File]::Exists($Path)) {
            [IO.File]::Move($temporary, $Path, $true)
        } else {
            [IO.File]::Move($temporary, $Path)
        }
    } finally {
        if ([IO.File]::Exists($temporary)) {
            [IO.File]::Delete($temporary)
        }
    }
}

function ConvertTo-QwanbanPowerShellSingleQuoted {
    param([AllowNull()][string]$Value)
    if ($null -eq $Value) { return "''" }
    return "'" + $Value.Replace("'", "''") + "'"
}

function ConvertTo-QwanbanCmdDoubleQuoted {
    param([Parameter(Mandatory)][string]$Value)
    if ($Value -match '["\r\n%&|^<>!]') {
        throw "Path or argument contains characters unsafe for cmd.exe launch commands: $Value"
    }
    return '"' + $Value + '"'
}

function Invoke-QwanbanDirectHttp {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][ValidateSet('GET', 'POST')][string]$Method,
        [Parameter(Mandatory)][string]$Uri,
        [hashtable]$Headers = @{},
        [string]$ContentType,
        [string]$BodyText,
        [string]$InFile,
        [int]$TimeoutSeconds = 30
    )
    $curl = Join-Path $env:SystemRoot 'System32\curl.exe'
    if (-not (Test-Path -LiteralPath $curl)) {
        $curl = 'curl.exe'
    }
    $responseFile = Join-Path ([IO.Path]::GetTempPath()) "qwanban-http-$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        $arguments = @(
            '--silent',
            '--show-error',
            '--noproxy', '*',
            '--max-time', [string]$TimeoutSeconds,
            '--request', $Method,
            '--output', $responseFile,
            '--write-out', '%{http_code}'
        )
        foreach ($entry in $Headers.GetEnumerator()) {
            $arguments += @('--header', "$($entry.Key): $($entry.Value)")
        }
        if (-not [string]::IsNullOrEmpty($ContentType)) {
            $arguments += @('--header', "Content-Type: $ContentType")
        }
        $bodyFile = $null
        try {
            if ($PSBoundParameters.ContainsKey('BodyText')) {
                $bodyFile = Join-Path ([IO.Path]::GetTempPath()) "qwanban-body-$([Guid]::NewGuid().ToString('N')).tmp"
                [IO.File]::WriteAllText($bodyFile, $BodyText, (New-Object Text.UTF8Encoding($false)))
                $arguments += @('--data-binary', "@$bodyFile")
            } elseif (-not [string]::IsNullOrEmpty($InFile)) {
                $arguments += @('--data-binary', "@$InFile")
            }
            $arguments += $Uri
            $stderrLines = @()
            $statusText = & $curl @arguments 2>&1 | ForEach-Object {
                if ($_ -is [Management.Automation.ErrorRecord]) {
                    $stderrLines += $_.ToString()
                } else {
                    $_
                }
            } | Out-String
            $exitCode = $LASTEXITCODE
            $body = if (Test-Path -LiteralPath $responseFile) {
                [IO.File]::ReadAllText($responseFile, [Text.Encoding]::UTF8)
            } else {
                ''
            }
            $statusCode = 0
            [void][int]::TryParse($statusText.Trim(), [ref]$statusCode)
            return [pscustomobject]@{
                ExitCode   = $exitCode
                StatusCode = $statusCode
                Body       = $body
                Error      = ($stderrLines -join "`n").Trim()
            }
        } finally {
            if ($null -ne $bodyFile -and (Test-Path -LiteralPath $bodyFile)) {
                [IO.File]::Delete($bodyFile)
            }
        }
    } finally {
        if (Test-Path -LiteralPath $responseFile) {
            [IO.File]::Delete($responseFile)
        }
    }
}

function Get-QwanbanGceMetadataAttribute {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Name,
        [string]$MetadataBaseUri = 'http://169.254.169.254/computeMetadata/v1/instance/attributes',
        [switch]$Optional,
        [int]$TimeoutSeconds = 10
    )
    $uri = "$($MetadataBaseUri.TrimEnd('/'))/$Name"
    $reply = Invoke-QwanbanDirectHttp -Method GET -Uri $uri -Headers @{ 'Metadata-Flavor' = 'Google' } -TimeoutSeconds $TimeoutSeconds
    if ($reply.ExitCode -eq 0 -and $reply.StatusCode -eq 200) {
        return $reply.Body
    }
    if ($Optional -and $reply.StatusCode -eq 404) {
        return $null
    }
    $detail = if (-not [string]::IsNullOrWhiteSpace($reply.Error)) { $reply.Error } else { $reply.Body }
    throw "Failed to read GCE metadata attribute '$Name' from $uri (HTTP $($reply.StatusCode), exit $($reply.ExitCode)): $detail"
}

function Read-QwanbanWorkerMetadata {
    [CmdletBinding()]
    param(
        [string]$MetadataBaseUri = 'http://169.254.169.254/computeMetadata/v1/instance/attributes',
        [int]$TimeoutSeconds = 10
    )
    $jobIdText = (Get-QwanbanGceMetadataAttribute -Name 'control-tower-job-id' -MetadataBaseUri $MetadataBaseUri -TimeoutSeconds $TimeoutSeconds).Trim()
    $jobGuid = [Guid]::Empty
    if (-not [Guid]::TryParse($jobIdText, [ref]$jobGuid)) {
        throw "Invalid control-tower-job-id metadata value: '$jobIdText'"
    }
    $jobId = $jobGuid.ToString('D').ToLowerInvariant()
    $guestName = 'ct-' + $jobGuid.ToString('N').ToLowerInvariant()

    $workerUrl = (Get-QwanbanGceMetadataAttribute -Name 'control-tower-worker-url' -MetadataBaseUri $MetadataBaseUri -TimeoutSeconds $TimeoutSeconds).Trim().TrimEnd('/')
    $parsedWorkerUri = $null
    if (-not [Uri]::TryCreate($workerUrl, [UriKind]::Absolute, [ref]$parsedWorkerUri) -or ($parsedWorkerUri.Scheme -ne 'http' -and $parsedWorkerUri.Scheme -ne 'https')) {
        throw "Invalid control-tower-worker-url metadata value: '$workerUrl'"
    }

    $prompt = Get-QwanbanGceMetadataAttribute -Name 'control-tower-prompt' -MetadataBaseUri $MetadataBaseUri -Optional -TimeoutSeconds $TimeoutSeconds
    if ($null -eq $prompt) { $prompt = '' }

    $setupScript = Get-QwanbanGceMetadataAttribute -Name 'control-tower-setup-script' -MetadataBaseUri $MetadataBaseUri -Optional -TimeoutSeconds $TimeoutSeconds
    if ($null -eq $setupScript) { $setupScript = '' }

    $bootstrapUrl = Get-QwanbanGceMetadataAttribute -Name 'friendzone-bootstrap-url' -MetadataBaseUri $MetadataBaseUri -Optional -TimeoutSeconds $TimeoutSeconds
    if (-not [string]::IsNullOrWhiteSpace($bootstrapUrl)) {
        $bootstrapUrl = $bootstrapUrl.Trim().TrimEnd('/')
    } else {
        $bootstrapUrl = $null
    }

    return [pscustomobject]@{
        JobId                  = $jobId
        GuestName              = $guestName
        WorkerUrl              = $workerUrl
        Prompt                 = [string]$prompt
        SetupScript            = [string]$setupScript
        FriendzoneBootstrapUrl = $bootstrapUrl
    }
}

function Wait-QwanbanFriendzoneApproval {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$BootstrapUrl,
        [Parameter(Mandatory)][string]$GuestName,
        [int]$TimeoutSeconds = 180,
        [int]$PollIntervalMilliseconds = 1000
    )
    $base = $BootstrapUrl.Trim().TrimEnd('/')
    $escapedGuest = [Uri]::EscapeDataString($GuestName)
    $uri = "$base/bootstrap/hello?container=$escapedGuest"
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($TimeoutSeconds)
    $lastDetail = 'no response yet'

    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        $reply = Invoke-QwanbanDirectHttp -Method GET -Uri $uri -TimeoutSeconds 10
        if ($reply.ExitCode -eq 0 -and $reply.StatusCode -eq 200) {
            try {
                $payload = $reply.Body | ConvertFrom-Json -ErrorAction Stop
                if ($payload.approved -eq $true) {
                    return $payload
                }
                $lastDetail = "approved=$($payload.approved)"
            } catch {
                $lastDetail = "invalid JSON from $uri"
            }
        } else {
            $lastDetail = "HTTP $($reply.StatusCode) (exit $($reply.ExitCode)) $($reply.Error)"
        }
        Start-Sleep -Milliseconds $PollIntervalMilliseconds
    }
    throw "Timed out after ${TimeoutSeconds}s waiting for Friendzone approval of '$GuestName' at ${uri}: $lastDetail"
}

function Initialize-QwanbanWorkerJob {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$RunsRoot,
        [pscustomobject]$Metadata,
        [string]$MetadataBaseUri = 'http://169.254.169.254/computeMetadata/v1/instance/attributes',
        [string]$QbtExecutable = 'C:\Program Files\qwanban\qbt.exe',
        [ValidateRange(1, 65535)][int]$QbtAgentPort = 1234,
        [ValidateRange(0, 65535)][int]$QbtObservatoryPort = 5678,
        [ValidateRange(1, 100000)][int]$QbtMaxScreenshots = 500
    )
    if ($null -eq $Metadata) {
        $Metadata = Read-QwanbanWorkerMetadata -MetadataBaseUri $MetadataBaseUri
    }
    $jobGuid = [Guid]::Empty
    if (-not [Guid]::TryParse([string]$Metadata.JobId, [ref]$jobGuid)) {
        throw "Metadata.JobId must be a valid UUID: '$($Metadata.JobId)'"
    }
    $jobId = $jobGuid.ToString('D').ToLowerInvariant()
    $guestName = if (-not [string]::IsNullOrWhiteSpace($Metadata.GuestName)) {
        [string]$Metadata.GuestName
    } else {
        'ct-' + $jobGuid.ToString('N').ToLowerInvariant()
    }

    [IO.Directory]::CreateDirectory($RunsRoot) | Out-Null
    $resolvedRunsRoot = (Resolve-Path -LiteralPath $RunsRoot).ProviderPath
    $jobRoot = Join-Path $resolvedRunsRoot $jobId
    if (Test-Path -LiteralPath $jobRoot) {
        throw "Job directory already exists at $jobRoot; refusing to mix state across runs"
    }

    $clineRoot = Join-Path $jobRoot 'cline'
    $clineSessionsDir = Join-Path $clineRoot 'sessions'
    $clineDbDir = Join-Path $clineRoot 'db'
    $clineLogsDir = Join-Path $clineRoot 'logs'
    $clineLogPath = Join-Path $clineLogsDir 'cline.log'
    $clineHooksLogPath = Join-Path $clineLogsDir 'hooks.jsonl'
    $qbtRoot = Join-Path $jobRoot 'qbt'
    $outputDir = Join-Path $jobRoot 'output'
    $packageDir = Join-Path $jobRoot 'package'
    $archivePath = Join-Path $packageDir 'results.tar.zst'
    $promptPath = Join-Path $jobRoot 'prompt.txt'
    $setupScriptPath = Join-Path $jobRoot 'setup.ps1'
    $jobEnvPath = Join-Path $jobRoot 'job-env.ps1'
    $runManifestPath = Join-Path $jobRoot 'run.json'

    foreach ($dir in @($jobRoot, $clineRoot, $clineSessionsDir, $clineDbDir, $clineLogsDir, $qbtRoot, $outputDir, $packageDir)) {
        [IO.Directory]::CreateDirectory($dir) | Out-Null
    }

    Write-QwanbanAtomicFile -Path $promptPath -Text ([string]$Metadata.Prompt)
    Write-QwanbanAtomicFile -Path $setupScriptPath -Text ([string]$Metadata.SetupScript)

    # Build a cmd.exe-compatible launch command so Cline's ComputerBackendRestart
    # (`spawn(command, { shell: true })`) restarts qbt against the same job root
    # and bounded disk-backed journal settings.
    $quotedQbt = ConvertTo-QwanbanCmdDoubleQuoted $QbtExecutable
    $quotedQbtRoot = ConvertTo-QwanbanCmdDoubleQuoted $qbtRoot
    $observatoryPortArg = if ($QbtObservatoryPort -gt 0) { " $QbtObservatoryPort" } else { '' }
    $backendCommand = "$quotedQbt serve $QbtAgentPort$observatoryPortArg --artifact-root $quotedQbtRoot --max-screenshots $QbtMaxScreenshots"

    $bypassHosts = @('localhost', '127.0.0.1', '::1', '169.254.169.254', 'metadata.google.internal')
    foreach ($url in @($Metadata.WorkerUrl, $Metadata.FriendzoneBootstrapUrl)) {
        if (-not [string]::IsNullOrWhiteSpace($url)) {
            $uri = $null
            if ([Uri]::TryCreate($url, [UriKind]::Absolute, [ref]$uri) -and -not [string]::IsNullOrWhiteSpace($uri.Host)) {
                if ($bypassHosts -notcontains $uri.Host) {
                    $bypassHosts += $uri.Host
                }
            }
        }
    }
    $noProxyValue = ($bypassHosts -join ',')

    $envLines = @(
        '# Generated per-job environment snapshot for qwanban worker execution.',
        '$env:CONTROL_TOWER_JOB_ID = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $jobId),
        '$env:CONTROL_TOWER_GUEST_NAME = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $guestName),
        '$env:CONTROL_TOWER_WORKER_URL = ' + (ConvertTo-QwanbanPowerShellSingleQuoted ([string]$Metadata.WorkerUrl)),
        '$env:CONTROL_TOWER_JOB_ROOT = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $jobRoot),
        '$env:CONTROL_TOWER_PROMPT_PATH = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $promptPath),
        '$env:CONTROL_TOWER_SETUP_SCRIPT_PATH = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $setupScriptPath),
        '$env:CONTROL_TOWER_OUTPUT_DIR = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $outputDir),
        '$env:QBT_ARTIFACT_ROOT = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $qbtRoot),
        '$env:CLINE_SESSION_BACKEND_MODE = ''local''',
        '$env:CLINE_SESSION_DATA_DIR = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $clineSessionsDir),
        '$env:CLINE_DB_DATA_DIR = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $clineDbDir),
        '$env:CLINE_LOG_PATH = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $clineLogPath),
        '$env:CLINE_HOOKS_LOG_PATH = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $clineHooksLogPath),
        '$env:CLINE_COMPUTER_USE_HOST = ''127.0.0.1''',
        '$env:CLINE_COMPUTER_USE_PORT = ' + (ConvertTo-QwanbanPowerShellSingleQuoted ([string]$QbtAgentPort)),
        '$env:CLINE_COMPUTER_USE_BACKEND_COMMAND = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $backendCommand),
        'if ([string]::IsNullOrWhiteSpace($env:NO_PROXY)) { $env:NO_PROXY = ' + (ConvertTo-QwanbanPowerShellSingleQuoted $noProxyValue) + ' } else {',
        '    $existing = @($env:NO_PROXY -split '','' | ForEach-Object { $_.Trim() } | Where-Object { $_ })',
        '    foreach ($entry in @(' + (($bypassHosts | ForEach-Object { ConvertTo-QwanbanPowerShellSingleQuoted $_ }) -join ', ') + ')) {',
        '        if ($existing -notcontains $entry) { $existing += $entry }',
        '    }',
        '    $env:NO_PROXY = ($existing -join '','')',
        '}',
        '$env:no_proxy = $env:NO_PROXY'
    )
    Write-QwanbanAtomicFile -Path $jobEnvPath -Text ($envLines -join "`n")

    $manifest = [ordered]@{
        version                = 1
        createdAt              = [DateTimeOffset]::UtcNow.ToString('o')
        jobId                  = $jobId
        guestName              = $guestName
        workerUrl              = [string]$Metadata.WorkerUrl
        friendzoneBootstrapUrl = $Metadata.FriendzoneBootstrapUrl
        jobRoot                = $jobRoot
        promptPath             = $promptPath
        setupScriptPath        = $setupScriptPath
        jobEnvPath             = $jobEnvPath
        outputDir              = $outputDir
        packageDir             = $packageDir
        archivePath            = $archivePath
        cline                  = [ordered]@{
            root               = $clineRoot
            sessionBackendMode = 'local'
            sessionDataDir     = $clineSessionsDir
            dbDataDir          = $clineDbDir
            logPath            = $clineLogPath
            hooksLogPath       = $clineHooksLogPath
        }
        qbt                    = [ordered]@{
            executable      = $QbtExecutable
            agentHost       = '127.0.0.1'
            agentPort       = $QbtAgentPort
            observatoryPort = $QbtObservatoryPort
            artifactRoot    = $qbtRoot
            maxScreenshots  = $QbtMaxScreenshots
            backendCommand  = $backendCommand
        }
    }
    Write-QwanbanAtomicFile -Path $runManifestPath -Text ($manifest | ConvertTo-Json -Depth 6)
    return (Get-Content -LiteralPath $runManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json)
}

function Test-QwanbanLoopbackPortOpen {
    param(
        [Parameter(Mandatory)][int]$Port,
        [int]$TimeoutMilliseconds = 250
    )
    $client = New-Object Net.Sockets.TcpClient
    try {
        $connectTask = $client.ConnectAsync('127.0.0.1', $Port)
        if (-not $connectTask.Wait($TimeoutMilliseconds)) {
            return $false
        }
        return $client.Connected
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

function Stop-QwanbanBackend {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][ValidateRange(1, 65535)][int]$Port,
        [int]$TimeoutMilliseconds = 5000
    )
    if (-not (Test-QwanbanLoopbackPortOpen -Port $Port)) {
        return $false
    }
    $client = New-Object Net.Sockets.TcpClient
    try {
        $connectTask = $client.ConnectAsync('127.0.0.1', $Port)
        if (-not $connectTask.Wait($TimeoutMilliseconds)) {
            throw "Timed out connecting to qbt on 127.0.0.1:$Port to request shutdown"
        }
        $stream = $client.GetStream()
        $stream.ReadTimeout = $TimeoutMilliseconds
        $stream.WriteTimeout = $TimeoutMilliseconds
        $utf8NoBom = New-Object Text.UTF8Encoding($false)
        $writer = New-Object IO.StreamWriter($stream, $utf8NoBom, 1024, $true)
        $reader = New-Object IO.StreamReader($stream, [Text.Encoding]::UTF8, $false, 1024, $true)
        $writer.NewLine = "`n"
        $writer.AutoFlush = $true
        $writer.WriteLine('{"id":1,"action":"shutdown_backend"}')
        $line = $reader.ReadLine()
        if ([string]::IsNullOrWhiteSpace($line)) {
            throw "qbt on 127.0.0.1:$Port closed connection without acknowledging shutdown_backend"
        }
        $reply = $line | ConvertFrom-Json -ErrorAction Stop
        if ($reply.ok -ne $true) {
            throw "qbt on 127.0.0.1:$Port rejected shutdown_backend: $line"
        }
    } finally {
        $client.Dispose()
    }

    $deadline = [DateTimeOffset]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    while ([DateTimeOffset]::UtcNow -lt $deadline) {
        if (-not (Test-QwanbanLoopbackPortOpen -Port $Port)) {
            return $true
        }
        Start-Sleep -Milliseconds 50
    }
    throw "qbt on 127.0.0.1:$Port acknowledged shutdown_backend but the port remained open after ${TimeoutMilliseconds}ms"
}

function Assert-QwanbanWritersQuiescent {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$JobRoot,
        [int[]]$WriterProcessIds = @(),
        [int]$QbtAgentPort = 0,
        [switch]$RequireCompleteJournal
    )
    foreach ($pidValue in $WriterProcessIds) {
        if ($pidValue -le 0) { continue }
        $proc = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
        if ($null -ne $proc -and -not $proc.HasExited) {
            throw "Writer process $pidValue ($($proc.ProcessName)) is still running; refusing to package non-quiescent job state"
        }
    }

    if ($QbtAgentPort -gt 0 -and (Test-QwanbanLoopbackPortOpen -Port $QbtAgentPort)) {
        throw "qbt agent port 127.0.0.1:$QbtAgentPort is still open; stop qbt before packaging artifacts"
    }

    $journalRoot = Join-Path $JobRoot 'qbt\journal'
    $tmpPath = Join-Path $journalRoot 'events.tmp'
    $eventsPath = Join-Path $journalRoot 'events.jsonl'
    $deadline = [DateTimeOffset]::UtcNow.AddMilliseconds(2000)
    while ([DateTimeOffset]::UtcNow -lt $deadline -and (Test-Path -LiteralPath $tmpPath)) {
        Start-Sleep -Milliseconds 50
    }
    if (Test-Path -LiteralPath $tmpPath) {
        throw "qbt journal under $journalRoot has an uncommitted events.tmp"
    }

    if ($RequireCompleteJournal) {
        if (-not (Test-Path -LiteralPath $eventsPath)) {
            throw "No qbt journal events.jsonl found under $journalRoot"
        }
        $lines = @(Get-Content -LiteralPath $eventsPath -Encoding UTF8 | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if ($lines.Count -eq 0) {
            throw "qbt journal $eventsPath is empty"
        }
        foreach ($line in $lines) {
            try {
                $parsed = $line | ConvertFrom-Json -ErrorAction Stop
                if ($null -eq $parsed.seq) {
                    throw 'missing seq field'
                }
            } catch {
                throw "qbt journal $eventsPath contains a corrupt line: $($_.Exception.Message)"
            }
        }
    }
}

function New-QwanbanJobArchive {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$JobRoot,
        [int[]]$WriterProcessIds = @(),
        [int]$QbtAgentPort = 0,
        [switch]$StopQbt,
        [switch]$RequireCompleteJournal
    )
    $resolvedJobRoot = (Resolve-Path -LiteralPath $JobRoot).ProviderPath
    $manifestPath = Join-Path $resolvedJobRoot 'run.json'
    $manifest = if (Test-Path -LiteralPath $manifestPath) {
        Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json -ErrorAction Stop
    } else {
        $null
    }
    if ($QbtAgentPort -le 0 -and $null -ne $manifest -and $null -ne $manifest.qbt) {
        $QbtAgentPort = [int]$manifest.qbt.agentPort
    }

    if ($StopQbt -and $QbtAgentPort -gt 0) {
        [void](Stop-QwanbanBackend -Port $QbtAgentPort)
    }

    Assert-QwanbanWritersQuiescent -JobRoot $resolvedJobRoot -WriterProcessIds $WriterProcessIds -QbtAgentPort $QbtAgentPort -RequireCompleteJournal:$RequireCompleteJournal

    $packageDir = Join-Path $resolvedJobRoot 'package'
    [IO.Directory]::CreateDirectory($packageDir) | Out-Null
    $archivePath = Join-Path $packageDir 'results.tar.zst'

    # Once built after quiescence, reuse the exact archive on upload/completion retries.
    if (Test-Path -LiteralPath $archivePath) {
        return $archivePath
    }

    $entries = @('run.json', 'prompt.txt', 'setup.ps1', 'cline', 'qbt', 'output') | Where-Object {
        Test-Path -LiteralPath (Join-Path $resolvedJobRoot $_)
    }
    if ($entries.Count -eq 0) {
        throw "No job entries found under $resolvedJobRoot to package"
    }

    $tarExe = Join-Path $env:SystemRoot 'System32\tar.exe'
    if (-not (Test-Path -LiteralPath $tarExe)) {
        $tarExe = 'tar.exe'
    }
    $tempArchive = "$archivePath.$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        $tarArgs = @('--zstd', '-cf', $tempArchive, '-C', $resolvedJobRoot) + $entries
        $tarOutput = & $tarExe @tarArgs 2>&1 | Out-String
        if ($LASTEXITCODE -ne 0) {
            throw "tar.exe failed with exit code ${LASTEXITCODE}: $($tarOutput.Trim())"
        }
        $stream = [IO.File]::OpenRead($tempArchive)
        try {
            $magic = New-Object byte[] 4
        $read = 0
        while ($read -lt 4) {
            $chunk = $stream.Read($magic, $read, 4 - $read)
            if ($chunk -le 0) { break }
            $read += $chunk
        }
            if ($read -ne 4 -or $magic[0] -ne 0x28 -or $magic[1] -ne 0xB5 -or $magic[2] -ne 0x2F -or $magic[3] -ne 0xFD) {
                throw "Generated archive at $tempArchive does not start with zstd frame magic"
            }
        } finally {
            $stream.Dispose()
        }
        [IO.File]::Move($tempArchive, $archivePath)
    } finally {
        if (Test-Path -LiteralPath $tempArchive) {
            [IO.File]::Delete($tempArchive)
        }
    }
    return $archivePath
}

function Send-QwanbanWorkerArtifact {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$WorkerUrl,
        [Parameter(Mandatory)][string]$JobId,
        [Parameter(Mandatory)][string]$ArchivePath,
        [int]$MaxAttempts = 5,
        [int]$InitialBackoffMilliseconds = 500,
        [int]$TimeoutSeconds = 300
    )
    if (-not (Test-Path -LiteralPath $ArchivePath)) {
        throw "Artifact archive does not exist: $ArchivePath"
    }
    $uri = "$($WorkerUrl.Trim().TrimEnd('/'))/jobs/$JobId/artifact"
    $backoff = $InitialBackoffMilliseconds
    $lastFailure = ''

    for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
        $reply = Invoke-QwanbanDirectHttp -Method POST -Uri $uri -ContentType 'application/zstd' -InFile $ArchivePath -TimeoutSeconds $TimeoutSeconds
        # 201 Created on first commit; 204 No Content on idempotent retry.
        if ($reply.ExitCode -eq 0 -and ($reply.StatusCode -eq 201 -or $reply.StatusCode -eq 204)) {
            return [pscustomobject]@{
                StatusCode = $reply.StatusCode
                Attempts   = $attempt
                Archive    = $ArchivePath
            }
        }
        $lastFailure = "attempt $attempt/$MaxAttempts failed (HTTP $($reply.StatusCode), exit $($reply.ExitCode)): $($reply.Body) $($reply.Error)".Trim()
        if ($attempt -lt $MaxAttempts) {
            Start-Sleep -Milliseconds $backoff
            $backoff = [Math]::Min($backoff * 2, 10000)
        }
    }
    throw "Failed to upload artifact for job $JobId to ${uri}: $lastFailure"
}

function Send-QwanbanWorkerCompletion {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$WorkerUrl,
        [Parameter(Mandatory)][string]$JobId,
        [Parameter(Mandatory)][ValidateSet('completed', 'failed')][string]$Outcome,
        [int]$MaxAttempts = 5,
        [int]$InitialBackoffMilliseconds = 500,
        [int]$TimeoutSeconds = 30
    )
    $uri = "$($WorkerUrl.Trim().TrimEnd('/'))/jobs/$JobId/complete"
    $payload = (@{ outcome = $Outcome } | ConvertTo-Json -Compress)
    $backoff = $InitialBackoffMilliseconds
    $lastFailure = ''

    for ($attempt = 1; $attempt -le $MaxAttempts; $attempt++) {
        $reply = Invoke-QwanbanDirectHttp -Method POST -Uri $uri -ContentType 'application/json' -BodyText $payload -TimeoutSeconds $TimeoutSeconds
        if ($reply.ExitCode -eq 0 -and $reply.StatusCode -eq 202) {
            return [pscustomobject]@{
                StatusCode = $reply.StatusCode
                Outcome    = $Outcome
                Attempts   = $attempt
            }
        }
        $lastFailure = "attempt $attempt/$MaxAttempts failed (HTTP $($reply.StatusCode), exit $($reply.ExitCode)): $($reply.Body) $($reply.Error)".Trim()
        if ($attempt -lt $MaxAttempts) {
            Start-Sleep -Milliseconds $backoff
            $backoff = [Math]::Min($backoff * 2, 10000)
        }
    }
    throw "Failed to report completion ($Outcome) for job $JobId to ${uri}: $lastFailure"
}

function Complete-QwanbanWorkerJob {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$JobRoot,
        [Parameter(Mandatory)][ValidateSet('completed', 'failed')][string]$Outcome,
        [int[]]$WriterProcessIds = @(),
        [switch]$StopQbt,
        [switch]$RequireCompleteJournal,
        [switch]$SkipArtifact,
        [int]$MaxAttempts = 5,
        [int]$InitialBackoffMilliseconds = 500
    )
    $resolvedJobRoot = (Resolve-Path -LiteralPath $JobRoot).ProviderPath
    $manifestPath = Join-Path $resolvedJobRoot 'run.json'
    if (-not (Test-Path -LiteralPath $manifestPath)) {
        throw "Missing run.json in $resolvedJobRoot"
    }
    $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json -ErrorAction Stop
    $workerUrl = [string]$manifest.workerUrl
    $jobId = [string]$manifest.jobId

    $uploadResult = $null
    if (-not $SkipArtifact) {
        $archivePath = New-QwanbanJobArchive `
            -JobRoot $resolvedJobRoot `
            -WriterProcessIds $WriterProcessIds `
            -QbtAgentPort ([int]$manifest.qbt.agentPort) `
            -StopQbt:$StopQbt `
            -RequireCompleteJournal:($RequireCompleteJournal -or ($Outcome -eq 'completed'))
        $uploadResult = Send-QwanbanWorkerArtifact `
            -WorkerUrl $workerUrl `
            -JobId $jobId `
            -ArchivePath $archivePath `
            -MaxAttempts $MaxAttempts `
            -InitialBackoffMilliseconds $InitialBackoffMilliseconds
    }

    $completionResult = Send-QwanbanWorkerCompletion `
        -WorkerUrl $workerUrl `
        -JobId $jobId `
        -Outcome $Outcome `
        -MaxAttempts $MaxAttempts `
        -InitialBackoffMilliseconds $InitialBackoffMilliseconds

    return [pscustomobject]@{
        JobId      = $jobId
        Outcome    = $Outcome
        Artifact   = $uploadResult
        Completion = $completionResult
    }
}