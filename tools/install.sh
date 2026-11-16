#!/bin/sh
# nitid is a Windows program: it draws through DirectX 12 for HDR and lives in
# Explorer's menus, and no release carries a build for anything else. This
# script is here so that whoever pipes it into a shell on Linux or macOS is
# told so, rather than being handed a 404 or a program that cannot start.
#
# On Windows, in PowerShell:
#   irm https://raw.githubusercontent.com/lacodda/nitid/main/tools/install.ps1 | iex
set -eu

echo "nitid runs on Windows only; there is no release for $(uname -s)." >&2
echo "On Windows, in PowerShell:" >&2
echo "  irm https://raw.githubusercontent.com/lacodda/nitid/main/tools/install.ps1 | iex" >&2
exit 1
