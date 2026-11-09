DiskDiff for Windows (x64)

Unzip this folder anywhere (for example to "C:\Program Files\DiskDiff" or
your user folder) and run diskdiff.exe. There's no installer; to remove
DiskDiff, delete the folder, and %APPDATA%\DiskDiff Native if you also want
to delete your scans and settings.

Windows may show a SmartScreen warning the first time, since the app isn't
signed: choose "More info", then "Run anyway".

Requirements: Windows 10 (1809) or later, 64-bit.

Scanning C:\ never follows junctions, symbolic links or mounted volumes, and
never downloads OneDrive (or other cloud) files that are online only: they
count as the space they take on this PC. Hard links in the Windows folder
(WinSxS) are counted once. Some system folders can only be read by an
administrator and show as unreadable; run DiskDiff as administrator if you
need them included.

diskdiff-mcp.exe is DiskDiff's server for AI agents (MCP). See the README.
