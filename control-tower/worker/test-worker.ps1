$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$workerScript = Join-Path $PSScriptRoot 'worker.ps1'
$tokens = $null
$parseErrors = $null
[void][Management.Automation.Language.Parser]::ParseFile($workerScript, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -gt 0) {
    throw ($parseErrors | Out-String)
}

. $workerScript

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) "qwanban-worker-test-$PID-$([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())"
[IO.Directory]::CreateDirectory($tempRoot) | Out-Null

# Spin up an in-process background HTTP fixture for metadata, Friendzone hello,
# artifact upload, and completion callbacks.
$listener = [Net.Sockets.TcpListener]::new([IPAddress]::Loopback, 0)
$listener.Start()
$serverPort = ([Net.IPEndPoint]$listener.LocalEndpoint).Port
$listener.Stop()
$baseUri = "http://127.0.0.1:$serverPort"

$serverStatePath = Join-Path $tempRoot 'server-state.json'
$jobUuid = '12345678-1234-4234-8234-123456789abc'
$initialServerState = @{
    jobId              = $jobUuid
    workerUrl          = "$baseUri/worker"
    bootstrapUrl       = "$baseUri/fz"
    prompt             = "Verify checkout flow`nLine 2"
    setupScript        = "Write-Output 'setup ok'"
    helloCalls         = 0
    uploadCalls        = 0
    completeCalls      = 0
    uploadedBytes      = 0
    uploadedSha256     = ''
    completedOutcome   = ''
    order              = @()
}
[IO.File]::WriteAllText($serverStatePath, ($initialServerState | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding($false)))

$serverPowerShell = [PowerShell]::Create()
[void]$serverPowerShell.AddScript({
    param([string]$Prefix, [string]$StatePath)
    $http = [Net.HttpListener]::new()
    $http.Prefixes.Add("$Prefix/")
    $http.Start()
    try {
        while ($http.IsListening) {
            $ctxTask = $http.GetContextAsync()
            $ctx = $ctxTask.GetAwaiter().GetResult()
            $req = $ctx.Request
            $res = $ctx.Response
            $path = $req.Url.AbsolutePath
            $state = [IO.File]::ReadAllText($StatePath, [Text.Encoding]::UTF8) | ConvertFrom-Json

            $writeText = {
                param([int]$Code, [string]$Text, [string]$ContentType = 'text/plain; charset=utf-8')
                $res.StatusCode = $Code
                $res.ContentType = $ContentType
                $bytes = [Text.Encoding]::UTF8.GetBytes($Text)
                $res.ContentLength64 = $bytes.Length
                $res.OutputStream.Write($bytes, 0, $bytes.Length)
                $res.OutputStream.Close()
            }

            if ($path -eq '/shutdown') {
                & $writeText 200 'bye'
                break
            }

            if ($path.StartsWith('/computeMetadata/v1/instance/attributes/')) {
                if ($req.Headers['Metadata-Flavor'] -cne 'Google') {
                    & $writeText 403 'missing Metadata-Flavor: Google'
                    continue
                }
                $attr = $path.Substring('/computeMetadata/v1/instance/attributes/'.Length)
                switch ($attr) {
                    'control-tower-job-id' { & $writeText 200 $state.jobId }
                    'control-tower-worker-url' { & $writeText 200 $state.workerUrl }
                    'control-tower-prompt' { & $writeText 200 $state.prompt }
                    'control-tower-setup-script' { & $writeText 200 $state.setupScript }
                    'friendzone-bootstrap-url' { & $writeText 200 $state.bootstrapUrl }
                    default { & $writeText 404 'not found' }
                }
                continue
            }

            if ($path -eq '/fz/bootstrap/hello') {
                $state.helloCalls = [int]$state.helloCalls + 1
                $approved = ($state.helloCalls -ge 2)
                [IO.File]::WriteAllText($StatePath, ($state | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding($false)))
                $body = (@{ container = $req.QueryString['container']; approved = $approved } | ConvertTo-Json -Compress)
                & $writeText 200 $body 'application/json'
                continue
            }

            if ($path -eq "/worker/jobs/$($state.jobId)/artifact") {
                $ms = New-Object IO.MemoryStream
                $req.InputStream.CopyTo($ms)
                $bytes = $ms.ToArray()
                $state.uploadCalls = [int]$state.uploadCalls + 1
                $state.order = @($state.order) + @("upload:$($state.uploadCalls)")
                if ($state.uploadCalls -eq 1) {
                    [IO.File]::WriteAllText($StatePath, ($state | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding($false)))
                    & $writeText 503 '{"error":"transient budget contention"}' 'application/json'
                    continue
                }
                if ($state.uploadCalls -eq 2) {
                    $sha = [BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($bytes)).Replace('-', '').ToLowerInvariant()
                    $state.uploadedBytes = $bytes.Length
                    $state.uploadedSha256 = $sha
                    [IO.File]::WriteAllText($StatePath, ($state | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding($false)))
                    & $writeText 201 '' 'application/json'
                    continue
                }
                [IO.File]::WriteAllText($StatePath, ($state | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding($false)))
                $res.StatusCode = 204
                $res.Close()
                continue
            }

            if ($path -eq "/worker/jobs/$($state.jobId)/complete") {
                $reader = New-Object IO.StreamReader($req.InputStream, [Text.Encoding]::UTF8)
                $raw = $reader.ReadToEnd()
                $parsed = $raw | ConvertFrom-Json
                $state.completeCalls = [int]$state.completeCalls + 1
                $state.order = @($state.order) + @("complete:$($state.completeCalls)")
                if ([int]$state.uploadedBytes -le 0) {
                    [IO.File]::WriteAllText($StatePath, ($state | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding($false)))
                    & $writeText 409 '{"error":"complete called before artifact upload"}' 'application/json'
                    continue
                }
                if ($state.completeCalls -eq 1) {
                    [IO.File]::WriteAllText($StatePath, ($state | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding($false)))
                    & $writeText 502 '{"error":"transient gateway error"}' 'application/json'
                    continue
                }
                $state.completedOutcome = [string]$parsed.outcome
                [IO.File]::WriteAllText($StatePath, ($state | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding($false)))
                & $writeText 202 '' 'application/json'
                continue
            }

            & $writeText 404 'unknown route'
        }
    } finally {
        $http.Stop()
        $http.Close()
    }
}).AddArgument($baseUri).AddArgument($serverStatePath)
$serverHandle = $serverPowerShell.BeginInvoke()

$savedEnv = @{}
$trackedEnvVars = @(
    'CONTROL_TOWER_JOB_ID',
    'CONTROL_TOWER_GUEST_NAME',
    'CONTROL_TOWER_WORKER_URL',
    'CONTROL_TOWER_JOB_ROOT',
    'CONTROL_TOWER_PROMPT_PATH',
    'CONTROL_TOWER_SETUP_SCRIPT_PATH',
    'CONTROL_TOWER_OUTPUT_DIR',
    'QBT_ARTIFACT_ROOT',
    'CLINE_SESSION_BACKEND_MODE',
    'CLINE_SESSION_DATA_DIR',
    'CLINE_DB_DATA_DIR',
    'CLINE_LOG_PATH',
    'CLINE_HOOKS_LOG_PATH',
    'CLINE_COMPUTER_USE_HOST',
    'CLINE_COMPUTER_USE_PORT',
    'CLINE_COMPUTER_USE_BACKEND_COMMAND',
    'NO_PROXY',
    'no_proxy'
)
foreach ($name in $trackedEnvVars) {
    $savedEnv[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

try {
    # 1. Read GCE metadata and wait for Friendzone approval
    $metadata = Read-QwanbanWorkerMetadata -MetadataBaseUri "$baseUri/computeMetadata/v1/instance/attributes"
    if ($metadata.JobId -cne $jobUuid) { throw "Unexpected JobId: $($metadata.JobId)" }
    if ($metadata.GuestName -cne 'ct-12345678123442348234123456789abc') { throw "Unexpected GuestName: $($metadata.GuestName)" }
    if ($metadata.Prompt -cne "Verify checkout flow`nLine 2") { throw "Unexpected Prompt: $($metadata.Prompt)" }

    $approval = Wait-QwanbanFriendzoneApproval -BootstrapUrl $metadata.FriendzoneBootstrapUrl -GuestName $metadata.GuestName -TimeoutSeconds 5 -PollIntervalMilliseconds 50
    if ($approval.approved -ne $true) { throw 'Expected Friendzone approval' }

    # 2. Initialize isolated job layout and verify duplicate run protection
    $qbtPortListener = [Net.Sockets.TcpListener]::new([IPAddress]::Loopback, 0)
    $qbtPortListener.Start()
    $qbtAgentPort = ([Net.IPEndPoint]$qbtPortListener.LocalEndpoint).Port
    $qbtPortListener.Stop()

    $qbtExe = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..\..\target\debug')).ProviderPath 'qbt.exe'
    if (-not (Test-Path -LiteralPath $qbtExe)) {
        throw "Expected compiled qbt binary at $qbtExe"
    }

    $runsRoot = Join-Path $tempRoot 'runs'
    $manifest = Initialize-QwanbanWorkerJob `
        -RunsRoot $runsRoot `
        -Metadata $metadata `
        -QbtExecutable $qbtExe `
        -QbtAgentPort $qbtAgentPort `
        -QbtObservatoryPort 0

    $duplicateBlocked = $false
    try {
        [void](Initialize-QwanbanWorkerJob -RunsRoot $runsRoot -Metadata $metadata -QbtExecutable $qbtExe -QbtAgentPort $qbtAgentPort)
    } catch {
        $duplicateBlocked = $true
    }
    if (-not $duplicateBlocked) {
        throw 'Expected duplicate Initialize-QwanbanWorkerJob for the same JobId to fail'
    }

    # Dot-source job-env.ps1 and verify environment isolation
    $env:NO_PROXY = 'example.internal'
    . $manifest.jobEnvPath
    if ($env:CLINE_SESSION_BACKEND_MODE -cne 'local') { throw "Expected CLINE_SESSION_BACKEND_MODE=local, got $env:CLINE_SESSION_BACKEND_MODE" }
    if ($env:CLINE_SESSION_DATA_DIR -cne $manifest.cline.sessionDataDir) { throw 'CLINE_SESSION_DATA_DIR mismatch' }
    if ($env:CLINE_DB_DATA_DIR -cne $manifest.cline.dbDataDir) { throw 'CLINE_DB_DATA_DIR mismatch' }
    if ($env:CLINE_LOG_PATH -cne $manifest.cline.logPath) { throw 'CLINE_LOG_PATH mismatch' }
    if ($env:CLINE_COMPUTER_USE_PORT -cne [string]$qbtAgentPort) { throw 'CLINE_COMPUTER_USE_PORT mismatch' }
    if ($env:NO_PROXY -notmatch 'example\.internal' -or $env:NO_PROXY -notmatch '169\.254\.169\.254') {
        throw "NO_PROXY did not preserve existing entry and add metadata bypass: $env:NO_PROXY"
    }

    # Simulate Cline session & log outputs
    Write-QwanbanAtomicFile -Path (Join-Path $manifest.cline.sessionDataDir 'session-1.json') -Text '{"id":"session-1"}'
    Write-QwanbanAtomicFile -Path $manifest.cline.logPath -Text '{"msg":"cline started"}'
    Write-QwanbanAtomicFile -Path (Join-Path $manifest.outputDir 'summary.txt') -Text 'QA passed'

    # 3. Start real `qbt.exe serve --record-journal` on $qbtAgentPort
    $qbtProc = Start-Process -FilePath $qbtExe -ArgumentList @(
        'serve', [string]$qbtAgentPort,
        '--artifact-root', $manifest.qbt.artifactRoot,
        '--max-screenshots', '5',
        '--record-journal'
    ) -PassThru -WindowStyle Hidden

    try {
        $readyDeadline = [DateTimeOffset]::UtcNow.AddSeconds(10)
        while (-not (Test-QwanbanLoopbackPortOpen -Port $qbtAgentPort) -and [DateTimeOffset]::UtcNow -lt $readyDeadline) {
            Start-Sleep -Milliseconds 50
        }
        if (-not (Test-QwanbanLoopbackPortOpen -Port $qbtAgentPort)) {
            throw "qbt did not start listening on $qbtAgentPort"
        }

        # Send a publish_event over TCP to the running qbt instance
        $tcp = New-Object Net.Sockets.TcpClient('127.0.0.1', $qbtAgentPort)
        try {
            $stream = $tcp.GetStream()
            $writer = New-Object IO.StreamWriter($stream, (New-Object Text.UTF8Encoding($false)), 1024, $true)
            $reader = New-Object IO.StreamReader($stream, [Text.Encoding]::UTF8, $false, 1024, $true)
            $writer.NewLine = "`n"
            $writer.AutoFlush = $true
            $writer.WriteLine('{"id":1,"action":"publish_event","kind":"transcript.message","payload":{"text":"recorded event"}}')
            $reply = $reader.ReadLine() | ConvertFrom-Json
            if ($reply.ok -ne $true) { throw "publish_event failed: $($reply | ConvertTo-Json -Compress)" }
        } finally {
            $tcp.Dispose()
        }

        # Verify packaging refuses while qbt is still running (and status.json is complete=false)
        $liveRejected = $false
        try {
            [void](New-QwanbanJobArchive -JobRoot $manifest.jobRoot -QbtAgentPort $qbtAgentPort -RequireCompleteJournal)
        } catch {
            $liveRejected = $true
        }
        if (-not $liveRejected) {
            throw 'Expected New-QwanbanJobArchive to reject while qbt port is still open'
        }

        # Also verify packaging refuses while a tracked writer PID is still alive
        $writerRejected = $false
        try {
            [void](Assert-QwanbanWritersQuiescent -JobRoot $manifest.jobRoot -WriterProcessIds @($PID))
        } catch {
            $writerRejected = $true
        }
        if (-not $writerRejected) {
            throw 'Expected Assert-QwanbanWritersQuiescent to reject a running writer PID'
        }

        # 4. Run Complete-QwanbanWorkerJob with -StopQbt: stops qbt cleanly via shutdown_backend,
        #    waits for process exit, verifies status.json complete=true, packages tar.zst,
        #    retries upload (503 -> 201), and retries completion (502 -> 202).
        $result = Complete-QwanbanWorkerJob `
            -JobRoot $manifest.jobRoot `
            -Outcome 'completed' `
            -StopQbt `
            -RequireCompleteJournal `
            -InitialBackoffMilliseconds 25

        $qbtProc.WaitForExit(5000) | Out-Null
        if ($qbtProc.ExitCode -ne 0) {
            throw "qbt exited with non-zero code $($qbtProc.ExitCode)"
        }
    } finally {
        if ($null -ne $qbtProc -and -not $qbtProc.HasExited) {
            Stop-Process -Id $qbtProc.Id -Force -ErrorAction SilentlyContinue
        }
    }

    if ($result.Artifact.StatusCode -ne 201 -or $result.Artifact.Attempts -ne 2) {
        throw "Unexpected artifact upload result: $($result.Artifact | ConvertTo-Json -Compress)"
    }
    if ($result.Completion.StatusCode -ne 202 -or $result.Completion.Attempts -ne 2) {
        throw "Unexpected completion result: $($result.Completion | ConvertTo-Json -Compress)"
    }

    # Verify archive immutability on retry: mutating a file after archive creation
    # must not rebuild results.tar.zst, and a second Complete-QwanbanWorkerJob
    # call receives 204 No Content for the identical archive.
    $firstHash = (Get-FileHash -LiteralPath $manifest.archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    Write-QwanbanAtomicFile -Path (Join-Path $manifest.outputDir 'summary.txt') -Text 'post-archive mutation'
    $retryResult = Complete-QwanbanWorkerJob `
        -JobRoot $manifest.jobRoot `
        -Outcome 'completed' `
        -RequireCompleteJournal `
        -InitialBackoffMilliseconds 25
    $secondHash = (Get-FileHash -LiteralPath $manifest.archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($firstHash -cne $secondHash) {
        throw "Archive hash changed across retry ($firstHash vs $secondHash)"
    }
    if ($retryResult.Artifact.StatusCode -ne 204) {
        throw "Expected 204 on idempotent upload retry, got $($retryResult.Artifact.StatusCode)"
    }

    # Verify server recorded upload before completion and exact SHA-256
    $finalState = [IO.File]::ReadAllText($serverStatePath, [Text.Encoding]::UTF8) | ConvertFrom-Json
    if ($finalState.uploadedSha256 -cne $firstHash) {
        throw "Uploaded SHA-256 ($($finalState.uploadedSha256)) did not match archive ($firstHash)"
    }
    $expectedOrder = @('upload:1', 'upload:2', 'complete:1', 'complete:2', 'upload:3', 'complete:3')
    if (($finalState.order -join ',') -cne ($expectedOrder -join ',')) {
        throw "Unexpected request order: $($finalState.order -join ',')"
    }

    # Extract archive to verify contents
    $extractDir = Join-Path $tempRoot 'extracted'
    [IO.Directory]::CreateDirectory($extractDir) | Out-Null
    & "$env:SystemRoot\System32\tar.exe" --zstd -xf $manifest.archivePath -C $extractDir
    if ($LASTEXITCODE -ne 0) { throw "Failed to extract $manifest.archivePath" }

    if ((Get-Content -LiteralPath (Join-Path $extractDir 'output\summary.txt') -Raw).Trim() -cne 'QA passed') {
        throw 'Extracted archive did not preserve pre-mutation output/summary.txt'
    }
    if (-not (Test-Path -LiteralPath (Join-Path $extractDir 'cline\sessions\session-1.json'))) {
        throw 'Extracted archive missing cline/sessions/session-1.json'
    }
    $extractedJournals = @(Get-ChildItem -LiteralPath (Join-Path $extractDir 'qbt\journal') -Directory)
    if ($extractedJournals.Count -ne 1) {
        throw "Expected 1 extracted qbt journal directory, found $($extractedJournals.Count)"
    }
    $extractedStatus = Get-Content -LiteralPath (Join-Path $extractedJournals[0].FullName 'status.json') -Raw | ConvertFrom-Json
    if ($extractedStatus.complete -ne $true -or $extractedStatus.events -ne 1) {
        throw "Unexpected extracted journal status: $($extractedStatus | ConvertTo-Json -Compress)"
    }

    # 5. Verify incomplete journal recording blocks 'completed' outcome but allows 'failed' outcome
    $failedUuid = '87654321-4321-4321-8321-cba987654321'
    $state = [IO.File]::ReadAllText($serverStatePath, [Text.Encoding]::UTF8) | ConvertFrom-Json
    $state.jobId = $failedUuid
    $state.uploadCalls = 1
    $state.completeCalls = 1
    $state.uploadedBytes = 0
    $state.uploadedSha256 = ''
    $state.completedOutcome = ''
    [IO.File]::WriteAllText($serverStatePath, ($state | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding($false)))

    $failedMeta = Read-QwanbanWorkerMetadata -MetadataBaseUri "$baseUri/computeMetadata/v1/instance/attributes"
    $failedManifest = Initialize-QwanbanWorkerJob `
        -RunsRoot $runsRoot `
        -Metadata $failedMeta `
        -QbtExecutable $qbtExe `
        -QbtAgentPort $qbtAgentPort `
        -QbtObservatoryPort 0
    $badJournalDir = Join-Path $failedManifest.qbt.artifactRoot 'journal\999-1-0'
    [IO.Directory]::CreateDirectory($badJournalDir) | Out-Null
    Write-QwanbanAtomicFile -Path (Join-Path $badJournalDir 'status.json') -Text '{"version":1,"complete":false,"events":3,"bytes":120,"error":"journal byte limit reached"}'
    Write-QwanbanAtomicFile -Path (Join-Path $badJournalDir 'events.jsonl') -Text '{"seq":1}'

    $completedBlocked = $false
    try {
        [void](Complete-QwanbanWorkerJob -JobRoot $failedManifest.jobRoot -Outcome 'completed' -InitialBackoffMilliseconds 25)
    } catch {
        $completedBlocked = $true
    }
    if (-not $completedBlocked) {
        throw 'Expected Complete-QwanbanWorkerJob -Outcome completed to reject an incomplete qbt journal'
    }

    $failedResult = Complete-QwanbanWorkerJob -JobRoot $failedManifest.jobRoot -Outcome 'failed' -InitialBackoffMilliseconds 25
    if ($failedResult.Artifact.StatusCode -ne 201 -or $failedResult.Completion.StatusCode -ne 202 -or $failedResult.Outcome -cne 'failed') {
        throw "Expected failed job diagnostic upload + completion to succeed, got: $($failedResult | ConvertTo-Json -Compress)"
    }

    Write-Output 'PASS: metadata snapshot, Friendzone wait, job isolation, qbt durable recording, quiescence checks, immutable tar.zst packaging, and ordered retry-safe upload/completion'
} finally {
    foreach ($entry in $savedEnv.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
    }
    try { [void](Invoke-QwanbanDirectHttp -Method GET -Uri "$baseUri/shutdown" -TimeoutSeconds 2) } catch {}
    try { [void]$serverPowerShell.EndInvoke($serverHandle) } catch {}
    $serverPowerShell.Dispose()
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}