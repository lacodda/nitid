# nitid installer for Windows:
#   irm https://raw.githubusercontent.com/lacodda/nitid/main/tools/install.ps1 | iex
#
# Installs - or updates - nitid for the current user, from the latest release:
# no administrator, nothing outside your own account. It downloads the
# release's zip and runs `nitid install` from it, which is what copies nitid to
# %LOCALAPPDATA%\Programs\nitid, registers the file types and the folder menu,
# puts nitid on your PATH and starts it in the background for Explorer's space
# bar. A viewer that is open while it runs keeps going; the next picture opened
# is the new version.
$ErrorActionPreference = "Stop"

$repo = "lacodda/nitid"

# The tag comes from the /releases/latest redirect rather than the REST API:
# unauthenticated API calls are capped at 60 per hour per IP, and an installer
# that fails because someone else on the same address ran it is no installer.
# $env:NITID_VERSION pins a specific release.
$tag = $env:NITID_VERSION
if (-not $tag) {
    $request = [Net.HttpWebRequest]::Create("https://github.com/$repo/releases/latest")
    $request.AllowAutoRedirect = $false
    $request.UserAgent = "nitid-installer"
    try {
        $response = $request.GetResponse()
        $tag = ($response.Headers["Location"] -split "/")[-1]
        $response.Close()
    } catch {
        throw "Cannot resolve the latest release of ${repo}: $($_.Exception.Message)"
    }
}
if (-not $tag -or $tag -notmatch '^v\d+\.\d+\.\d+$') {
    throw "Cannot resolve the latest release of $repo - set `$env:NITID_VERSION to a release tag (vX.Y.Z)"
}
$version = $tag.Substring(1)

$name = "nitid-$tag-x86_64-pc-windows-msvc.zip"
$url = "https://github.com/$repo/releases/download/$tag/$name"
$tmp = Join-Path ([IO.Path]::GetTempPath()) "nitid-install-$([guid]::NewGuid())"
New-Item -ItemType Directory -Force $tmp | Out-Null

# Where `nitid install` puts it. Not a choice: the registration, the PATH entry
# and the sign-in entry all name this directory, and `nitid uninstall` finds
# it there.
$target = Join-Path $env:LOCALAPPDATA "Programs\nitid\nitid.exe"

try {
    $zip = Join-Path $tmp $name
    Write-Host "Downloading $url"
    # The progress bar of Windows PowerShell slows the download itself down by
    # an order of magnitude.
    $progress = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try {
        Invoke-WebRequest $url -OutFile $zip -UseBasicParsing
    } finally {
        $ProgressPreference = $progress
    }

    $unpacked = Join-Path $tmp "unpacked"
    Expand-Archive $zip -DestinationPath $unpacked -Force
    # Found in the unpacked tree rather than at a guessed path: the zip's
    # layout is the release workflow's to choose.
    $exe = Get-ChildItem $unpacked -Recurse -Filter "nitid.exe" | Select-Object -First 1
    if (-not $exe) {
        throw "$name has no nitid.exe in it"
    }

    & $exe.FullName install
    if ($LASTEXITCODE -ne 0) {
        throw "nitid install stopped with code $LASTEXITCODE"
    }

    # The file, not the exit code, says what is installed: an install can
    # finish cleanly and leave an older copy in place. The same bytes as the
    # zip's, and the version that was fetched.
    if (-not (Test-Path $target)) {
        throw "nitid install finished, but $target is not there"
    }
    $installed = (Get-FileHash $target -Algorithm SHA256).Hash
    $fetched = (Get-FileHash $exe.FullName -Algorithm SHA256).Hash
    if ($installed -ne $fetched) {
        throw "nitid install finished, but $target is not the nitid.exe of $tag"
    }
    $said = & $target --version
    if ($said -ne "nitid $version") {
        throw "nitid install finished, but $target says '$said' rather than 'nitid $version'"
    }
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "Installed nitid $tag to $target"
Write-Host "Open a terminal again for 'nitid' to be a command there."
