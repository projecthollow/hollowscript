#Requires -Version 5.1
<#
.SYNOPSIS
    Installs the HollowScript command line tool on Windows.

.DESCRIPTION
    Installs hollowscript.exe and hollowscript-lsp.exe (the language server
    `hollowscript lsp` starts) into one directory and adds that directory to
    the user's PATH.

        irm https://get.hollowscript.org/install.ps1 | iex

    Safe to run that way: everything is inside one function, and every exit
    from it is `return`, never `exit`, so a failure does not close the
    PowerShell session that piped the script into `iex`, and
    $ErrorActionPreference and StrictMode are set only inside that function.

    Verification: every download is checked against SHA256SUMS, and
    SHA256SUMS is signed. The signature, and the version it was signed for,
    are verified before anything is installed, with minisign if it is
    installed, otherwise with ssh-keygen from the OpenSSH client that
    Windows 10 and 11 include (an optional feature on some builds; it is
    needed for OpenSSH 8.0 or newer). Windows PowerShell 5.1 has no Ed25519
    of its own. With neither, the install is refused, because a SHA256SUMS
    fetched from the server the archive came from proves nothing about that
    server: install minisign (winget install jedisct1.minisign, or scoop
    install minisign) or the OpenSSH client, or accept the risk for one
    install with -AllowUnsigned (HOLLOWSCRIPT_ALLOW_UNSIGNED=1, exactly 1).

    See docs/design/release.md, section 5.

.PARAMETER Version
    A release to install, for example v0.1.0. Defaults to the
    HOLLOWSCRIPT_VERSION environment variable, then to the latest release.

.PARAMETER InstallDir
    Where the binaries go. Defaults to HOLLOWSCRIPT_INSTALL_DIR, then to
    %LOCALAPPDATA%\hollowscript\bin.

.PARAMETER BaseUrl
    Where releases are served from. Defaults to HOLLOWSCRIPT_BASE_URL, then
    to https://get.hollowscript.org. Must be https:// (plain http:// is
    accepted for 127.0.0.1, localhost and [::1] only, for local testing).

.PARAMETER PublicKeyFile
    A minisign public key trusted in addition to (never instead of) the key
    built into this script, for a key rotation. Defaults to
    HOLLOWSCRIPT_PUBLIC_KEY_FILE.

.PARAMETER RequireSignature
    Refuse to install unless the signature can be verified here, even if
    -AllowUnsigned is also given. Also set by HOLLOWSCRIPT_REQUIRE_SIGNATURE
    (1, true or yes).

.PARAMETER AllowUnsigned
    Install on the strength of SHA256SUMS alone when this machine has no
    signature verifier. Also set by HOLLOWSCRIPT_ALLOW_UNSIGNED=1; only
    exactly 1 is accepted from the environment.

.PARAMETER AllowDowngrade
    Let the latest release replace a newer installed one. Also set by
    HOLLOWSCRIPT_ALLOW_DOWNGRADE (1, true or yes). An explicit -Version is
    always installed as asked.

.PARAMETER NoModifyPath
    Do not add the install directory to the user's PATH. Also set by
    HOLLOWSCRIPT_NO_MODIFY_PATH (1, true or yes).

.PARAMETER Uninstall
    Remove the installed binaries and the PATH entry this script added.
#>
[CmdletBinding()]
param(
    [switch] $Uninstall,
    [string] $Version = $env:HOLLOWSCRIPT_VERSION,
    [string] $InstallDir = $env:HOLLOWSCRIPT_INSTALL_DIR,
    [string] $BaseUrl = $(if ($env:HOLLOWSCRIPT_BASE_URL) { $env:HOLLOWSCRIPT_BASE_URL } else { 'https://get.hollowscript.org' }),
    [string] $PublicKeyFile = $env:HOLLOWSCRIPT_PUBLIC_KEY_FILE,
    # 1, true and yes turn these on: `[bool]$env:X` would read "0" as true.
    # (-match is case-insensitive.) Only exactly 1 turns the unsigned opt-out
    # on. The function below refuses any other value of these variables.
    [switch] $RequireSignature = [bool]($env:HOLLOWSCRIPT_REQUIRE_SIGNATURE -match '^(1|true|yes)$'),
    [switch] $AllowUnsigned = [bool]($env:HOLLOWSCRIPT_ALLOW_UNSIGNED -ceq '1'),
    [switch] $AllowDowngrade = [bool]($env:HOLLOWSCRIPT_ALLOW_DOWNGRADE -match '^(1|true|yes)$'),
    [switch] $NoModifyPath = [bool]($env:HOLLOWSCRIPT_NO_MODIFY_PATH -match '^(1|true|yes)$')
)

function Install-HollowScript {
    [CmdletBinding()]
    param(
        [switch] $Uninstall,
        [string] $Version,
        [string] $InstallDir,
        [string] $BaseUrl,
        [string] $PublicKeyFile,
        [switch] $RequireSignature,
        [switch] $AllowUnsigned,
        [switch] $AllowDowngrade,
        [switch] $NoModifyPath
    )

    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version Latest

    # Constrained Language Mode blocks setting .NET properties (TLS below)
    # and registry writes: refuse before attempting anything.
    if ($ExecutionContext.SessionState.LanguageMode -eq 'ConstrainedLanguage') {
        Write-Error 'install.ps1: this session is in Constrained Language Mode; refusing to run rather than partially install. Run it from an unrestricted session, or ask an administrator to install hollowscript.'
        return 1
    }

    # TLS 1.2 or newer, and nothing older: assigned, not added to what is
    # there, since Windows PowerShell 5.1's default may still offer SSL 3,
    # TLS 1.0 and TLS 1.1. TLS 1.3 is added where this .NET knows it.
    try {
        $protocols = [Net.SecurityProtocolType]::Tls12
        if ([Enum]::GetNames([Net.SecurityProtocolType]) -contains 'Tls13') {
            $protocols = $protocols -bor [Net.SecurityProtocolType]::Tls13
        }
        [Net.ServicePointManager]::SecurityProtocol = $protocols
    } catch {
        Write-Error "install.ps1: could not set TLS 1.2 or newer: $_"
        return 1
    }

    function Info([string] $msg) { Write-Host "install.ps1: $msg" }
    function Fail([string] $msg) { throw "install.ps1: $msg" }

    # A setting that changes what is trusted is not silently ignored because
    # of a typo.
    foreach ($flagName in 'HOLLOWSCRIPT_REQUIRE_SIGNATURE', 'HOLLOWSCRIPT_ALLOW_DOWNGRADE', 'HOLLOWSCRIPT_NO_MODIFY_PATH') {
        $flagValue = [Environment]::GetEnvironmentVariable($flagName)
        if ($flagValue -and $flagValue -notmatch '^(1|true|yes|0|false|no)$') {
            Fail "$flagName must be 1, true or yes (or 0, false or no), not '$flagValue'"
        }
    }
    $unsignedValue = $env:HOLLOWSCRIPT_ALLOW_UNSIGNED
    if ($unsignedValue -and $unsignedValue -cne '0' -and $unsignedValue -cne '1') {
        Fail "HOLLOWSCRIPT_ALLOW_UNSIGNED is accepted only as exactly 1 (or 0), not '$unsignedValue'"
    }

    $onWindows = ($PSVersionTable.PSVersion.Major -lt 6) -or $IsWindows
    $names = @('hollowscript.exe', 'hollowscript-lsp.exe')

    if (-not $InstallDir) {
        $localAppData = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { [Environment]::GetFolderPath('LocalApplicationData') }
        $InstallDir = Join-Path (Join-Path $localAppData 'hollowscript') 'bin'
    }
    $InstallDir = $InstallDir.TrimEnd('\', '/')

    # --- the user PATH, read and written raw in the registry --------------
    # [Environment]::GetEnvironmentVariable('Path', 'User') expands
    # %VARIABLES% and SetEnvironmentVariable would store the result as a
    # plain string, silently converting every entry that used one; and
    # $env:Path is the machine PATH and the user PATH joined, which must
    # never be written back as the user PATH. So the one value is read with
    # its variables unexpanded and written back with its own value kind.
    function Get-UserPathRaw {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $false)
        if (-not $key) { return '' }
        try {
            return [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        } finally {
            $key.Close()
        }
    }
    function Set-UserPathRaw([string] $value) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
        try {
            $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
            if ($key.GetValueNames() -contains 'Path') { $kind = $key.GetValueKind('Path') }
            if ($value) {
                $key.SetValue('Path', $value, $kind)
            } else {
                $key.DeleteValue('Path', $false)
            }
        } finally {
            $key.Close()
        }
        # Tell running programs (Explorer, so new terminals) the environment
        # changed: setting any user variable through .NET broadcasts
        # WM_SETTINGCHANGE, so a throwaway one is set and removed.
        [Environment]::SetEnvironmentVariable('HOLLOWSCRIPT_INSTALL_REFRESH', '1', 'User')
        [Environment]::SetEnvironmentVariable('HOLLOWSCRIPT_INSTALL_REFRESH', $null, 'User')
    }
    function Split-PathList([string] $value) {
        return @($value -split ';' | Where-Object { $_ -ne '' })
    }
    function Test-SameDir([string] $a, [string] $b) {
        return [string]::Equals($a.TrimEnd('\', '/'), $b.TrimEnd('\', '/'), [StringComparison]::OrdinalIgnoreCase)
    }

    if ($Uninstall) {
        foreach ($name in $names + @('.hollowscript.previous.exe', '.hollowscript-lsp.previous.exe', 'hollowscript.exe.new', 'hollowscript-lsp.exe.new')) {
            $path = Join-Path $InstallDir $name
            if (Test-Path -LiteralPath $path) {
                Remove-Item -Force -LiteralPath $path
                Info "removed $path"
            }
        }
        if ((Test-Path -LiteralPath $InstallDir) -and -not (Get-ChildItem -Force -LiteralPath $InstallDir)) {
            Remove-Item -Force -LiteralPath $InstallDir
            $parent = Split-Path -Parent $InstallDir
            if ((Split-Path -Leaf $parent) -eq 'hollowscript' -and -not (Get-ChildItem -Force -LiteralPath $parent)) {
                Remove-Item -Force -LiteralPath $parent
            }
        }
        if ($onWindows) {
            $entries = Split-PathList (Get-UserPathRaw)
            $kept = @($entries | Where-Object { -not (Test-SameDir ([Environment]::ExpandEnvironmentVariables($_)) $InstallDir) })
            if ($kept.Count -ne $entries.Count) {
                Set-UserPathRaw ($kept -join ';')
                Info "removed $InstallDir from your user PATH"
            }
        }
        Info 'uninstalled. Open a new terminal for the PATH change to take effect.'
        return 0
    }

    # --- where to download from: https only -------------------------------
    # Parsed, not pattern-matched: a prefix test would take
    # http://127.0.0.1:1@example.org, whose host is example.org. A user name
    # in the URL, a space, a backslash, a query or a fragment is refused.
    $BaseUrl = $BaseUrl.TrimEnd('/')
    $baseUri = $null
    if (-not [Uri]::TryCreate($BaseUrl, [UriKind]::Absolute, [ref] $baseUri) -or
        $BaseUrl -match '[\s\\?#]' -or $BaseUrl -match '^[A-Za-z][A-Za-z0-9+.-]*://[^/]*@' -or -not $baseUri.Host) {
        Fail "base URL '$BaseUrl' is not a plain URL (no user name, spaces, backslashes, query or fragment); refusing to download from it"
    }
    $loopbackHosts = @('127.0.0.1', 'localhost', '[::1]', '::1')
    if ($baseUri.Scheme -eq 'https') {
        $secureTransport = $true
    } elseif ($baseUri.Scheme -eq 'http' -and ($loopbackHosts -contains $baseUri.Host)) {
        $secureTransport = $false
    } else {
        Fail "base URL '$BaseUrl' is not https:// (plain http:// is accepted only for 127.0.0.1, localhost and [::1]); refusing to download over an insecure channel"
    }

    # --- platform ---------------------------------------------------------
    # The one Windows build is x86_64. Windows 11 on ARM runs it under its
    # own x64 emulation.
    $arch = $env:PROCESSOR_ARCHITECTURE
    if ($arch -eq 'ARM64') {
        Info 'ARM64 Windows: installing the x86_64 build, which runs under Windows x64 emulation'
    } elseif ($arch -ne 'AMD64') {
        Fail "unsupported architecture: $arch"
    }
    $target = 'x86_64-pc-windows-gnu'

    # The oldest release an install that asked for "the latest" will accept.
    # latest.txt is not signed, so whoever can write to the release bucket
    # could otherwise point a fresh install at any older, validly signed
    # release. This floor is part of this script (and of install.sh), which
    # is served from a different place than the bucket and carries the pinned
    # key, so the bucket alone cannot lower it. It is raised, in install.sh
    # too, in the change that releases a security fix; release-upload.sh
    # refuses to move latest.txt below it.
    $releaseFloor = 'v0.1.0'

    function Get-VersionKey([string] $v) {
        $core = ($v.TrimStart('v') -split '-')[0] -split '\.'
        return [version]::new([int]$core[0], [int]$core[1], [int]$core[2])
    }
    # True if version $a is older than $b: X.Y.Z numerically, and for an
    # equal X.Y.Z a prerelease is older than the release itself.
    function Test-VersionOlder([string] $a, [string] $b) {
        $ka = Get-VersionKey $a
        $kb = Get-VersionKey $b
        return ($ka -lt $kb) -or (($ka -eq $kb) -and $a.Contains('-') -and -not $b.Contains('-'))
    }

    $workDir = Join-Path ([System.IO.Path]::GetTempPath()) "hollowscript-install-$([System.Guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Path $workDir | Out-Null
    $staged = @()
    $swapped = $false
    $hadBefore = @{}
    try {
        # Downloads one file. Redirects are followed here, one at a time, not
        # by Invoke-WebRequest: Windows PowerShell 5.1 follows a redirect
        # from https to plain http without a word, which would hand the
        # download to anyone on the path. Every hop must be https (or, only
        # when the base URL itself is plain loopback http, loopback http),
        # at most five hops.
        function Get-File([string] $url, [string] $out) {
            $current = [Uri] $url
            for ($hop = 0; $hop -le 5; $hop++) {
                $isLoopbackHttp = ($current.Scheme -eq 'http') -and ($loopbackHosts -contains $current.Host)
                if ($current.Scheme -ne 'https' -and -not (-not $secureTransport -and $isLoopbackHttp)) {
                    Fail "refusing to download from ${current}: it is not https://"
                }
                $request = [System.Net.HttpWebRequest]::Create($current)
                $request.AllowAutoRedirect = $false
                $request.UserAgent = 'hollowscript-install.ps1'
                $request.Timeout = 60000
                $request.ReadWriteTimeout = 60000
                $response = $null
                try {
                    try {
                        $response = $request.GetResponse()
                    } catch [System.Net.WebException] {
                        # A 4xx or 5xx arrives as an exception holding the response.
                        $response = $_.Exception.Response
                        if (-not $response) { throw }
                    }
                    $code = [int] $response.StatusCode
                    if ($code -ge 300 -and $code -lt 400) {
                        $location = $response.Headers['Location']
                        if (-not $location) { Fail "$current redirected without saying where" }
                        $current = [Uri]::new($current, [string] $location)
                        continue
                    }
                    if ($code -ne 200) { Fail "$current answered HTTP $code" }
                    $stream = $response.GetResponseStream()
                    $file = [System.IO.File]::Create($out)
                    try {
                        $stream.CopyTo($file)
                    } finally {
                        $file.Close()
                        $stream.Close()
                    }
                    return
                } finally {
                    if ($response) { $response.Close() }
                }
            }
            Fail "too many redirects fetching $url"
        }

        # --- the version: exactly vX.Y.Z or vX.Y.Z-suffix -------------------
        $versionExplicit = [bool]$Version
        if (-not $Version) {
            $latestPath = Join-Path $workDir 'latest.txt'
            try {
                Get-File "$BaseUrl/latest.txt" $latestPath
            } catch {
                Fail "could not fetch $BaseUrl/latest.txt: $_"
            }
            $Version = ([System.IO.File]::ReadAllText($latestPath)).Trim()
        }
        if ($Version -cnotmatch '^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.]+)?$') {
            Fail "'$Version' is not a release version (expected vX.Y.Z or vX.Y.Z-suffix, letters, digits, '.' and '-' only); refusing to use it"
        }
        $versionNoV = $Version.Substring(1)
        if (Test-VersionOlder $Version $releaseFloor) {
            if (-not $versionExplicit) {
                Fail "$BaseUrl/latest.txt names $Version, older than $releaseFloor, the oldest release this installer accepts as the latest; the release channel may have been tampered with. Refusing. (A newer installer may be needed; to install $Version anyway, pass -Version $Version)"
            }
            Info "warning: $Version is older than $releaseFloor, the oldest release this installer accepts as the latest; installing it because it was asked for by name"
        }

        # --- what can check a signature here? Settled before anything big is fetched
        $minisign = Get-Command -Name 'minisign.exe', 'minisign' -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        $sshKeygen = $null
        if (-not $minisign) {
            $candidate = Get-Command -Name 'ssh-keygen.exe', 'ssh-keygen' -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
            # Asked with a bare -Y: an ssh-keygen older than OpenSSH 8.0 rejects
            # it as an unknown option, and a newer one answers with a usage
            # that lists the verify form. Native stderr is read with errors
            # not terminating (Windows PowerShell 5.1 would otherwise stop).
            if ($candidate) {
                $previousPreference = $ErrorActionPreference
                $ErrorActionPreference = 'Continue'
                try {
                    $usage = (& $candidate.Source -Y 2>&1 | Out-String)
                    if ($usage -match 'Y verify') { $sshKeygen = $candidate }
                } catch {
                    $sshKeygen = $null
                } finally {
                    $ErrorActionPreference = $previousPreference
                }
            }
        }
        $verifier = ''
        if ($minisign) { $verifier = 'minisign' } elseif ($sshKeygen) { $verifier = 'ssh-keygen' }
        if (-not $verifier) {
            # Fail closed. SHA256SUMS comes from the same server as the
            # archive, so matching it proves nothing about that server.
            Info 'no signature verifier is available on this machine: there is no minisign and no ssh-keygen'
            Info '  from OpenSSH 8.0 or newer. Without one, a release cannot be told from a forgery. Install'
            Info '  one and run this installer again: minisign (winget install jedisct1.minisign, or scoop'
            Info '  install minisign), or the OpenSSH client (Settings, Apps, Optional features).'
            if ($RequireSignature) {
                Fail 'no signature verifier is available, and -RequireSignature (HOLLOWSCRIPT_REQUIRE_SIGNATURE) is set; refusing to install'
            }
            if (-not $AllowUnsigned) {
                Fail "no signature verifier is available; refusing to install an unverified release (to install on the strength of SHA256SUMS alone, knowing a compromised $BaseUrl defeats it, pass -AllowUnsigned or set HOLLOWSCRIPT_ALLOW_UNSIGNED=1)"
            }
            Info "WARNING: -AllowUnsigned is set. Nothing downloaded will be authenticated beyond the TLS connection to $BaseUrl and the SHA256SUMS it serves."
        }

        Info "installing hollowscript $Version for $target"

        $archiveName = "hollowscript-$versionNoV-$target.zip"
        $archivePath = Join-Path $workDir $archiveName
        $sumsPath = Join-Path $workDir 'SHA256SUMS'
        Get-File "$BaseUrl/$Version/$archiveName" $archivePath
        Get-File "$BaseUrl/$Version/SHA256SUMS" $sumsPath

        # --- the signature over SHA256SUMS ----------------------------------
        # Kept byte-identical to scripts/hollowscript-release.pub and to the
        # copy in install.sh; a key rotation changes it here, never by
        # fetching a key from the server this script downloads from.
        $pinnedPublicKey = "untrusted comment: minisign public key for HollowScript releases`nRWTEd/MMbolYkjq8slz9NEko74kUDEwMdcjcsNLSY68z9K1MZpq0y4mK`n"
        $pinnedKeyFile = Join-Path $workDir 'pinned.pub'
        [System.IO.File]::WriteAllText($pinnedKeyFile, $pinnedPublicKey)
        $keyFiles = @($pinnedKeyFile)
        if ($PublicKeyFile) {
            if (-not (Test-Path -LiteralPath $PublicKeyFile)) { Fail "-PublicKeyFile '$PublicKeyFile' does not exist" }
            $keyFiles += $PublicKeyFile
        }

        # The key in a minisign public key file ("Ed", an 8 byte key id, the
        # 32 byte key) as an allowed signers file for ssh-keygen: one pinned
        # key, and no second copy of it to fall out of step.
        function Write-AllowedSigners([string] $keyFile, [string] $outFile) {
            try {
                $lines = [System.IO.File]::ReadAllLines($keyFile)
                if ($lines.Count -lt 2) { return $false }
                $raw = [Convert]::FromBase64String($lines[1].Trim())
            } catch {
                return $false
            }
            if ($raw.Length -ne 42 -or $raw[0] -ne 0x45 -or $raw[1] -ne 0x64) { return $false }
            # The SSH wire form of the key: string "ssh-ed25519", then string key.
            $blob = New-Object System.Collections.Generic.List[byte]
            $blob.AddRange([byte[]] (0, 0, 0, 11))
            $blob.AddRange([System.Text.Encoding]::ASCII.GetBytes('ssh-ed25519'))
            $blob.AddRange([byte[]] (0, 0, 0, 32))
            $blob.AddRange([byte[]] $raw[10..41])
            [System.IO.File]::WriteAllText($outFile, "hollowscript-release ssh-ed25519 $([Convert]::ToBase64String($blob.ToArray()))`n")
            return $true
        }

        if ($verifier) {
            $sigName = if ($verifier -eq 'minisign') { 'SHA256SUMS.minisig' } else { 'SHA256SUMS.sshsig' }
            $sigPath = Join-Path $workDir $sigName
            try {
                Get-File "$BaseUrl/$Version/$sigName" $sigPath
            } catch {
                Fail "could not fetch $BaseUrl/$Version/$sigName (a release made before SSH signatures were published has no .sshsig; install minisign to verify it): $_"
            }
            $verified = $false
            $keyNumber = 0
            foreach ($keyFile in $keyFiles) {
                $keyNumber++
                # A try, because Windows PowerShell 5.1 can turn a native
                # command's standard error into a terminating error under
                # $ErrorActionPreference = 'Stop'; a failed check is a
                # failed check either way.
                try {
                    if ($verifier -eq 'minisign') {
                        & $minisign.Source -V -q -p $keyFile -m $sumsPath -x $sigPath 2>$null | Out-Null
                        $ok = ($LASTEXITCODE -eq 0)
                    } else {
                        $allowedPath = Join-Path $workDir "allowed_signers.$keyNumber"
                        $ok = $false
                        if (Write-AllowedSigners $keyFile $allowedPath) {
                            # The signature is made in the namespace
                            # hollowscript-release-VERSION and verified in
                            # exactly that one, which ties it to this version.
                            # The message is the file's own bytes on standard
                            # input, redirected from the file so that nothing
                            # re-encodes it.
                            $argLine = "-Y verify -f `"$allowedPath`" -I hollowscript-release -n hollowscript-release-$Version -s `"$sigPath`""
                            $process = Start-Process -FilePath $sshKeygen.Source -ArgumentList $argLine `
                                -RedirectStandardInput $sumsPath `
                                -RedirectStandardOutput (Join-Path $workDir 'ssh-keygen.out') `
                                -RedirectStandardError (Join-Path $workDir 'ssh-keygen.err') `
                                -NoNewWindow -Wait -PassThru
                            $ok = ($process.ExitCode -eq 0)
                        }
                    }
                } catch {
                    $ok = $false
                }
                if ($ok) {
                    $verified = $true
                    break
                }
            }
            if (-not $verified) { Fail "the signature on SHA256SUMS does not verify ($verifier); refusing to install" }
            if ($verifier -eq 'minisign') {
                # Covered by the signature just verified; must name exactly this version.
                $comment = ([System.IO.File]::ReadAllLines($sigPath))[2]
                $expectedComment = "trusted comment: hollowscript SHA256SUMS, version $Version"
                if ($comment -cne $expectedComment) {
                    Fail "SHA256SUMS was not signed for $Version (its signed comment is '$($comment -replace '^trusted comment: ', '')'); refusing to install"
                }
            }
            Info "signature verified ($verifier), signed for $Version"
        }

        # --- the archive against SHA256SUMS ---------------------------------
        $expectedSum = $null
        foreach ($line in [System.IO.File]::ReadAllLines($sumsPath)) {
            $parts = $line -split '\s+'
            if ($parts.Count -eq 2 -and ($parts[1] -ceq $archiveName -or $parts[1] -ceq "*$archiveName")) {
                $expectedSum = $parts[0].ToLowerInvariant()
            }
        }
        if (-not $expectedSum) { Fail "SHA256SUMS has no entry for $archiveName" }
        $actualSum = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
        if ($actualSum -ne $expectedSum) {
            Fail "checksum mismatch for ${archiveName}: expected $expectedSum, got $actualSum; not installing it"
        }
        Info 'checksum verified'
        if (-not $verifier) {
            $transport = if ($secureTransport) { 'over HTTPS' } else { 'over plain http (loopback testing)' }
            Info "NOTICE: the archive matches SHA256SUMS, fetched $transport from $BaseUrl, but"
            Info '  the signature on SHA256SUMS was NOT checked (-AllowUnsigned). To verify this release,'
            Info '  install minisign (winget install jedisct1.minisign, or scoop install minisign) and check it by hand:'
            Info "    minisign -Vm SHA256SUMS -P RWTEd/MMbolYkjq8slz9NEko74kUDEwMdcjcsNLSY68z9K1MZpq0y4mK"
        }

        # --- never move an unasked-for install backwards --------------------
        $installedExe = Join-Path $InstallDir 'hollowscript.exe'
        if ((-not $versionExplicit) -and (Test-Path -LiteralPath $installedExe)) {
            $installed = $null
            try { $installed = ((& $installedExe --version) -split '\s+')[1] } catch { $installed = $null }
            if ($installed -and $installed -match '^[0-9]+\.[0-9]+\.[0-9]+') {
                if (Test-VersionOlder $Version $installed) {
                    if (-not $AllowDowngrade) {
                        Fail "$BaseUrl/latest.txt names $Version, older than the installed $installed; refusing to downgrade (pass -Version $Version, or set HOLLOWSCRIPT_ALLOW_DOWNGRADE=1, to install it anyway)"
                    }
                    Info "installing $Version over the newer $installed`: -AllowDowngrade is set"
                }
            }
        }

        # --- extract, and prove the binary runs and is the version asked for
        Expand-Archive -LiteralPath $archivePath -DestinationPath $workDir -Force
        $extracted = Join-Path $workDir "hollowscript-$versionNoV-$target"
        foreach ($name in $names) {
            if (-not (Test-Path -LiteralPath (Join-Path $extracted $name))) { Fail "the archive has no $name" }
        }
        $reported = $null
        try {
            $reported = (& (Join-Path $extracted 'hollowscript.exe') --version | Out-String).Trim()
        } catch {
            Fail "the downloaded hollowscript.exe does not run on this machine ($_); not installing it"
        }
        if ($reported -cne "hollowscript $versionNoV") {
            Fail "the downloaded hollowscript.exe reports '$reported', not 'hollowscript $versionNoV'; not installing it"
        }
        Info "downloaded binary runs ($reported)"

        # --- install: stage both, move the old ones aside, rename into place
        New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
        foreach ($name in $names) {
            $new = Join-Path $InstallDir "$name.new"
            Copy-Item -Force -LiteralPath (Join-Path $extracted $name) -Destination $new
            $staged += $name
        }
        $swapped = $true
        foreach ($name in $names) {
            $live = Join-Path $InstallDir $name
            $backup = Join-Path $InstallDir ('.' + [System.IO.Path]::GetFileNameWithoutExtension($name) + '.previous.exe')
            Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath $backup
            # A rename onto a vacated path, not Move-Item -Force onto an
            # existing file (a delete then a copy, not atomic).
            $hadBefore[$name] = (Test-Path -LiteralPath $live)
            if ($hadBefore[$name]) { Move-Item -LiteralPath $live -Destination $backup }
            Move-Item -LiteralPath (Join-Path $InstallDir "$name.new") -Destination $live
        }
        & $installedExe --version | Out-Null
        if ($LASTEXITCODE -ne 0) { Fail 'the installed hollowscript.exe does not run; restoring the previous install' }
        $swapped = $false
        $staged = @()
        foreach ($name in $names) {
            Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath (Join-Path $InstallDir ('.' + [System.IO.Path]::GetFileNameWithoutExtension($name) + '.previous.exe'))
        }
        Info "installed hollowscript $Version in $InstallDir"

        # --- PATH ---------------------------------------------------------
        $inSession = @($env:Path -split [System.IO.Path]::PathSeparator | Where-Object { $_ -and (Test-SameDir $_ $InstallDir) }).Count -gt 0
        if ($NoModifyPath) {
            if (-not $inSession) { Info "PATH was not changed (-NoModifyPath). Add $InstallDir to your PATH to use hollowscript." }
        } elseif (-not $onWindows) {
            if (-not $inSession) { Info "not on Windows: add $InstallDir to PATH yourself" }
        } else {
            $entries = Split-PathList (Get-UserPathRaw)
            $present = @($entries | Where-Object { Test-SameDir ([Environment]::ExpandEnvironmentVariables($_)) $InstallDir }).Count -gt 0
            if (-not $present) {
                Set-UserPathRaw ((@($InstallDir) + $entries) -join ';')
                Info "added $InstallDir to your user PATH"
            }
            if (-not $inSession) {
                $env:Path = "$InstallDir;$env:Path"
                Info 'this session has it now; other open terminals need restarting'
            }
        }
        return 0
    } finally {
        # On any failure after staging: remove what was staged and, if the
        # old binaries were already moved aside, put them back.
        foreach ($name in $names) {
            $backup = Join-Path $InstallDir ('.' + [System.IO.Path]::GetFileNameWithoutExtension($name) + '.previous.exe')
            if ($staged -contains $name) {
                Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath (Join-Path $InstallDir "$name.new")
            }
            if ($swapped -and (Test-Path -LiteralPath $backup)) {
                Move-Item -Force -LiteralPath $backup -Destination (Join-Path $InstallDir $name)
                Write-Host "install.ps1: restored the previous $name"
            } elseif ($swapped -and $hadBefore.ContainsKey($name) -and -not $hadBefore[$name] -and (Test-Path -LiteralPath (Join-Path $InstallDir $name))) {
                # A fresh install: there was nothing before it, so the only
                # thing to restore is that nothing. Leaving the new binary
                # would leave one that has just failed its check.
                Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath (Join-Path $InstallDir $name)
                Write-Host "install.ps1: removed the new $name, which was not in place before"
            }
        }
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -LiteralPath $workDir
    }
}

# The one call. A failure inside is reported here rather than left to end
# the caller's session, which a terminating error under `iex` can do.
try {
    Install-HollowScript -Uninstall:$Uninstall -Version $Version -InstallDir $InstallDir -BaseUrl $BaseUrl -PublicKeyFile $PublicKeyFile -RequireSignature:$RequireSignature -AllowUnsigned:$AllowUnsigned -AllowDowngrade:$AllowDowngrade -NoModifyPath:$NoModifyPath | Out-Null
} catch {
    Write-Error "$_"
}
