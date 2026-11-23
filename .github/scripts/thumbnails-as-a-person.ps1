# Install nitid as an ordinary person would have it, ask Windows for thumbnails,
# and uninstall it again.
#
#   .github/scripts/thumbnails-as-a-person.ps1 -Programs <directory of the two>
#
# As somebody else, because of what a runner is: it runs elevated, and Windows
# does not let an elevated process load what a person registered for themselves.
# An account of its own has neither that nor anything else left from before - no
# administrator, its own profile - which is who nitid is installed by.
#
# **For a machine that is thrown away.** It makes an account and leaves it.
param([Parameter(Mandatory)][string]$Programs)
$ErrorActionPreference = "Stop"

$stand = Join-Path $env:SystemDrive "nitid-person"
$repository = Join-Path $PSScriptRoot "..\.."
New-Item -ItemType Directory -Force "$stand\programs", "$stand\tools", "$stand\src\thumbs" | Out-Null
foreach ($program in "nitid.exe", "nitidw.exe") {
    Copy-Item (Join-Path $Programs $program) "$stand\programs"
}
Copy-Item (Join-Path $repository "tools\check-thumbnails.ps1") "$stand\tools"
Copy-Item (Join-Path $repository "src\thumbs\samples.rs") "$stand\src\thumbs"

# What the person does. Whatever it says goes to a file, and then how it ended:
# the output of a process somebody else started does not reach this one.
Set-Content "$stand\person.ps1" @'
$stand = Split-Path $MyInvocation.MyCommand.Path
$ended = "it did not finish"
try {
    & {
        $ErrorActionPreference = "Continue"
        whoami
        whoami /groups | Select-String "Mandatory Label"
        $dll = Join-Path $env:LOCALAPPDATA "Programs\nitid\nitid_thumbs.dll"

        & "$stand\programs\nitid.exe" install
        if ($LASTEXITCODE -ne 0) { throw "nitid install stopped with code $LASTEXITCODE" }
        if (-not (Test-Path $dll)) { throw "the install wrote no $dll" }

        & "$stand\tools\check-thumbnails.ps1"

        # With the DLL in use: the host that drew those still has it.
        & "$stand\programs\nitid.exe" uninstall
        if ($LASTEXITCODE -ne 0) { throw "nitid uninstall stopped with code $LASTEXITCODE" }
        if (Test-Path $dll) { throw "uninstalling left $dll" }
        if (Test-Path "HKCU:\Software\Classes\.jxl\ShellEx") { throw "uninstalling left the thumbnails registered" }
    } *>&1 | Out-File "$stand\said.txt" -Encoding utf8
    $ended = "well"
} catch {
    $ended = "$_"
}
Set-Content "$stand\ended.txt" $ended
'@

$password = "Aa1!" + [guid]::NewGuid().ToString("N").Substring(0, 10)
net user nitid-person $password /add | Out-Null
if ($LASTEXITCODE -ne 0) { throw "the account could not be made" }
icacls $stand /grant "nitid-person:(OI)(CI)F" | Out-Null

$person = New-Object PSCredential ".\nitid-person", (ConvertTo-SecureString $password -AsPlainText -Force)
Start-Process powershell -Credential $person -LoadUserProfile -WorkingDirectory $stand `
    -ArgumentList "-NoProfile -ExecutionPolicy Bypass -File $stand\person.ps1"

for ($i = 0; $i -lt 240 -and -not (Test-Path "$stand\ended.txt"); $i++) { Start-Sleep -Seconds 1 }
# The file appears a moment before what is written to it.
Start-Sleep -Seconds 1
Get-Content "$stand\said.txt" -ErrorAction SilentlyContinue
$ended = Get-Content "$stand\ended.txt" -ErrorAction SilentlyContinue
if ($ended -ne "well") { throw "as a person: $(if ($ended) { $ended } else { 'nothing was heard in four minutes' })" }
