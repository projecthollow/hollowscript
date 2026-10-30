# Parse the actual Windows installer without executing it or modifying PATH.
# Run on both Windows PowerShell 5.1 and PowerShell 7 when available.
param([string] $Installer = (Join-Path $PSScriptRoot 'install.ps1'))
$ErrorActionPreference = 'Stop'
$tokens = $null
$errors = $null
$null = [System.Management.Automation.Language.Parser]::ParseFile($Installer, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) {
    foreach ($problem in $errors) {
        Write-Output ('install.ps1:{0}:{1}: {2}: {3}' -f $problem.Extent.StartLineNumber, $problem.Extent.StartColumnNumber, $problem.ErrorId, $problem.Message)
    }
    exit 1
}
Write-Output 'install.ps1: native PowerShell parser passed'
