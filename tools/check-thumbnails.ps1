# Ask Windows for thumbnails, the way an Explorer window does.
#
#   powershell -File tools/check-thumbnails.ps1
#   powershell -File tools/check-thumbnails.ps1 -Files a.jxl,b.heic
#
# With nothing named it writes a picture of its own in each format nitid draws
# for Windows and asks about those, and stops with an error unless every one of
# them came back as the picture it is. For a machine nitid is installed on: the
# release workflow runs it on a clean one before a release is published.
#
# What answers is the shell, with whatever is registered: this script starts no
# nitid and loads none of it.
param([string[]]$Files)
$ErrorActionPreference = "Stop"

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;

public static class ShellThumbnail {
    [ComImport, Guid("bcc18b79-ba16-442f-80c4-8a59c30c463b"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IShellItemImageFactory { [PreserveSig] int GetImage(Size size, int flags, out IntPtr bitmap); }

    [StructLayout(LayoutKind.Sequential)]
    struct Bitmap32 { public int Type, Width, Height, WidthBytes; public short Planes, Bits; public IntPtr Data; }

    [DllImport("gdi32.dll")] static extern int GetObject(IntPtr handle, int size, out Bitmap32 bitmap);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(IntPtr handle);
    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    static extern int SHCreateItemFromParsingName(string path, IntPtr context, ref Guid interfaceId,
        [MarshalAs(UnmanagedType.Interface)] out IShellItemImageFactory factory);

    // A thumbnail and nothing else: not the type's icon in its place.
    const int ThumbnailOnly = 0x8;

    // "WIDTHxHEIGHT RRGGBB", the colour being the middle's; or "none 0x...".
    public static string Of(string path, int side) {
        Guid id = new Guid("bcc18b79-ba16-442f-80c4-8a59c30c463b");
        IShellItemImageFactory factory;
        int answer = SHCreateItemFromParsingName(path, IntPtr.Zero, ref id, out factory);
        if (answer != 0) return "none 0x" + answer.ToString("x8");
        IntPtr handle;
        answer = factory.GetImage(new Size(side, side), ThumbnailOnly, out handle);
        if (answer != 0) return "none 0x" + answer.ToString("x8");
        try {
            Bitmap32 bitmap;
            GetObject(handle, Marshal.SizeOf(typeof(Bitmap32)), out bitmap);
            if (bitmap.Bits != 32 || bitmap.Data == IntPtr.Zero) return bitmap.Width + "x" + bitmap.Height + " at " + bitmap.Bits + " bits";
            int at = (bitmap.Height / 2) * bitmap.WidthBytes + (bitmap.Width / 2) * 4;
            byte blue = Marshal.ReadByte(bitmap.Data, at), green = Marshal.ReadByte(bitmap.Data, at + 1), red = Marshal.ReadByte(bitmap.Data, at + 2);
            return bitmap.Width + "x" + bitmap.Height + " " + red.ToString("x2") + green.ToString("x2") + blue.ToString("x2");
        } finally {
            DeleteObject(handle);
        }
    }
}
'@

if ($Files) {
    foreach ($file in $Files) {
        "$([IO.Path]::GetFileName($file)): $([ShellThumbnail]::Of((Resolve-Path $file).Path, 256))"
    }
    exit 0
}

# The samples of src/thumbs/samples.rs: sixty-four pixels square, of one
# colour, written by other programs' encoders. Read from there rather than
# copied here, and SVG written out, being text.
$source = Get-Content -Raw (Join-Path $PSScriptRoot "..\src\thumbs\samples.rs")
function Get-Sample($name) {
    $found = [regex]::Match($source, "const ${name}: &str = concat!\(([^;]*)\);")
    if (-not $found.Success) { throw "src/thumbs/samples.rs has no sample called $name" }
    $text = -join ([regex]::Matches($found.Groups[1].Value, '"([0-9a-f]+)"') | ForEach-Object { $_.Groups[1].Value })
    $bytes = New-Object byte[] ($text.Length / 2)
    for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [Convert]::ToByte($text.Substring($i * 2, 2), 16) }
    , $bytes
}

$dir = Join-Path ([IO.Path]::GetTempPath()) "nitid-thumbnails-$([guid]::NewGuid())"
New-Item -ItemType Directory -Force $dir | Out-Null
$wrong = @()
try {
    # A name of its own each time: Explorer remembers what it drew for a file.
    [IO.File]::WriteAllBytes("$dir\sample.jxl", (Get-Sample "JXL"))
    [IO.File]::WriteAllBytes("$dir\sample.avif", (Get-Sample "AVIF"))
    [IO.File]::WriteAllBytes("$dir\sample.heic", (Get-Sample "HEIC"))
    [IO.File]::WriteAllText("$dir\sample.svg", '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><rect width="64" height="64" fill="#2878c8"/></svg>')

    foreach ($file in Get-ChildItem $dir | Sort-Object Name) {
        $said = [ShellThumbnail]::Of($file.FullName, 256)
        Write-Host "$($file.Name): $said"
        # 28 78 c8, as far as a lossy encoder keeps it.
        $colour = [regex]::Match($said, '^\d+x\d+ ([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$')
        $near = $colour.Success
        if ($near) {
            $meant = 0x28, 0x78, 0xc8
            for ($i = 0; $i -lt 3; $i++) {
                $near = $near -and ([Math]::Abs([Convert]::ToInt32($colour.Groups[$i + 1].Value, 16) - $meant[$i]) -le 8)
            }
        }
        if (-not $near) { $wrong += "$($file.Name) ($said)" }
    }
} finally {
    Remove-Item $dir -Recurse -Force -ErrorAction SilentlyContinue
}
if ($wrong) { throw "Windows shows no thumbnail, or not the right one, for $($wrong -join ', ')" }
Write-Host "Every one has its thumbnail."
