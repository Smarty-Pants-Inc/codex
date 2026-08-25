# Configure a fast drive for Windows CI jobs.
#
# GitHub-hosted Windows runners do not always expose a secondary D: volume. When
# they do not, create a Dev Drive VHD. Older images may not support the
# Format-Volume -DevDrive switch, so use a ReFS fallback while keeping the same
# fast build-root path.

function Test-DevDrive {
    param([string]$Drive)

    & fsutil devdrv query $Drive *> $null
    return $LASTEXITCODE -eq 0
}

function Invoke-BestEffort {
    param([scriptblock]$Script, [string]$Description)

    try {
        & $Script
    } catch {
        Write-Warning "$Description failed: $($_.Exception.Message)"
    }
}

if ((Test-Path "D:\") -and (Test-DevDrive "D:")) {
    Write-Output "Using existing Dev Drive at D:"
    $Drive = "D:"
} else {
    if (Test-Path "D:\") {
        Write-Output "Existing D: volume is not a Dev Drive; provisioning a new Dev Drive VHD."
    }

    try {
        $VhdPath = Join-Path $env:RUNNER_TEMP "codex-dev-drive.vhdx"
        $SizeBytes = 64GB

        if (Test-Path $VhdPath) {
            Remove-Item -Path $VhdPath -Force
        }

        New-VHD -Path $VhdPath -SizeBytes $SizeBytes -Dynamic -ErrorAction Stop | Out-Null
        $Mounted = Mount-VHD -Path $VhdPath -Passthru -ErrorAction Stop
        $Disk = $Mounted | Get-Disk -ErrorAction Stop
        $Disk | Initialize-Disk -PartitionStyle GPT -ErrorAction Stop
        $Partition = $Disk | New-Partition -AssignDriveLetter -UseMaximumSize -ErrorAction Stop
        $SupportsDevDrive = (Get-Command Format-Volume -ErrorAction Stop).Parameters.ContainsKey("DevDrive")
        if ($SupportsDevDrive) {
            $Volume = $Partition | Format-Volume -FileSystem ReFS -NewFileSystemLabel "CodexDevDrive" -DevDrive -Confirm:$false -Force -ErrorAction Stop
        } else {
            Write-Warning "Format-Volume does not support -DevDrive; using a ReFS build volume."
            $Volume = $Partition | Format-Volume -FileSystem ReFS -NewFileSystemLabel "CodexDevDrive" -Confirm:$false -Force -ErrorAction Stop
        }

        $Drive = "$($Volume.DriveLetter):"

        if ($SupportsDevDrive) {
            if (-not (Test-DevDrive $Drive)) {
                throw "Provisioned volume at $Drive did not pass Dev Drive verification."
            }

            Invoke-BestEffort { fsutil devdrv trust $Drive } "Trusting Dev Drive $Drive"
            Invoke-BestEffort { fsutil devdrv enable /disallowAv } "Disabling AV filter attachment for Dev Drives"

            Write-Output "Using Dev Drive at $Drive"
        } else {
            Write-Warning "Using ReFS fallback build volume at $Drive without Dev Drive verification."
        }
    } catch {
        throw "Failed to create Dev Drive: $($_.Exception.Message)"
    }
}

"CI_BUILD_ROOT=$Drive" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append
