$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# This subprocess emits only matched PIDs. Process command lines never leave it.
function Test-CodexName([string]$Name, [string]$Configured) {
    $leaf = ($Name -split '[\\/]')[-1].ToLowerInvariant()
    $leaf = $leaf -replace '\.exe$', ''
    $configuredLeaf = $Configured.ToLowerInvariant() -replace '\.exe$', ''
    return $leaf -eq 'codex' -or $leaf -match '^codex-(aarch64|x86_64|arm64|amd64|x64|[0-9])' -or $leaf -in @('codex.js', 'codex.cmd', 'codex.bat', 'codex.ps1') -or $leaf -eq $configuredLeaf
}
try {
    $configured = $env:XSWAP_PROCESS_EXECUTABLE
    $parentPid = [uint32]$env:XSWAP_PROCESS_PARENT
    $currentSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $matchedPids = [Collections.Generic.List[uint32]]::new()
    $processes = @(Get-CimInstance Win32_Process -Property Name, ProcessId -OperationTimeoutSec 5)
    if ($processes.Count -eq 0) { throw 'No process enumeration results' }
    foreach ($candidate in $processes) {
        $candidatePid = [uint32]$candidate.ProcessId
        if ($candidatePid -eq $PID -or $candidatePid -eq $parentPid) { continue }
        $native = Test-CodexName $candidate.Name $configured
        $launcher = $candidate.Name.ToLowerInvariant() -in @('node.exe', 'nodejs.exe', 'cmd.exe', 'powershell.exe', 'pwsh.exe', 'bash.exe', 'sh.exe')
        if (-not $native -and -not $launcher) { continue }
        try {
            $owner = Invoke-CimMethod -InputObject $candidate -MethodName GetOwnerSid -OperationTimeoutSec 5
            if ($owner.ReturnValue -ne 0 -or -not $owner.Sid) { throw 'Process owner is unavailable' }
        } catch {
            # An exit between enumeration and inspection is harmless; a query denial is not.
            $remaining = Get-CimInstance Win32_Process -Filter "ProcessId = $candidatePid" -Property ProcessId -OperationTimeoutSec 5
            if (-not $remaining) { continue }
            throw
        }
        if ($owner.Sid -ne $currentSid) { continue }
        if ($native) {
            $matchedPids.Add($candidatePid)
            continue
        }
        $details = Get-CimInstance Win32_Process -Filter "ProcessId = $candidatePid" -Property CommandLine -OperationTimeoutSec 5
        if (-not $details) { continue }
        if (-not $details.CommandLine) { throw 'Launcher command line is unavailable' }
        # Match the actual launcher entrypoint only, never prompts or inline program text.
        $entrypoint = $null
        if ($candidate.Name -ieq 'cmd.exe') {
            $command = [regex]::Match($details.CommandLine, '(?is)(?:^|\s)/[ck]\s+(.*)$')
            if ($command.Success) {
                $tail = $command.Groups[1].Value.TrimStart()
                # cmd wraps an already-quoted batch path in a second outer quote pair.
                if ($tail.StartsWith('""')) { $tail = $tail.Substring(1) }
                $first = [regex]::Match($tail, '^(?:"([^"]+)"|(\S+))')
                if ($first.Success) {
                    $entrypoint = if ($first.Groups[1].Success) { $first.Groups[1].Value } else { $first.Groups[2].Value.Trim([char]34) }
                }
            }
        } else {
            $words = @([regex]::Matches($details.CommandLine, '"[^"\r\n]*"|[^\s]+') | ForEach-Object { $_.Value.Trim([char]34, [char]39) })
            for ($index = 1; $index -lt $words.Count; $index++) {
                $word = $words[$index]
                if ($word -in @('-e', '--eval', '-p', '--print', '-c', '-Command', '-EncodedCommand')) { break }
                if ($word -in @('-r', '--require', '--import', '--loader', '--experimental-loader', '--conditions', '--title')) { $index++; continue }
                if ($word.StartsWith('-') -or $word.StartsWith('/')) { continue }
                $entrypoint = $word
                break
            }
        }
        if ($entrypoint -and (Test-CodexName $entrypoint $configured)) { $matchedPids.Add($candidatePid) }

    }
    ConvertTo-Json -InputObject @($matchedPids.ToArray()) -Compress
} catch {
    # Caller reports a fixed actionable error, without leaking command lines or CIM data.
    exit 1
}
