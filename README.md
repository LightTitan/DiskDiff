# DiskDiff

DiskDiff scans your disk over time and shows what changed between scans: which folders
grew, what was added, moved or deleted, and where your free space went. Each scan is kept
as a snapshot, so you can compare any two of them, browse a past scan like Finder, and spot
folders that keep growing.

It is a native app written in Rust, with the UI in [GPUI](https://www.gpui.rs) through
gpui-kit. It runs on macOS 12 or later, Windows 10 or later, and Linux. It is developed on
macOS; Windows and Linux builds are new and less tested.

| Crate | What |
|---|---|
| `engine` (`diskdiff-engine`) | Scanner, snapshot store, diffs, insights |
| `app` (`diskdiff`) | The window: graph, comparison summary, column browser, insights, manage scans |
| `mcp` (`diskdiff-mcp`) | MCP server and CLI that let AI agents read scans and request new ones |
| `ipc` (`diskdiff-ipc`) | How other processes ask the running app to scan |

## Installing

### macOS

Download the DMG from [Releases](https://github.com/LightTitan/DiskDiff/releases) and drag
DiskDiff into Applications. One build runs on both Apple silicon and Intel Macs.

The app is not notarized by Apple, so macOS blocks it the first time. Open it once, then go to
System Settings → Privacy & Security and click **Open Anyway**. Or clear the download flag
in Terminal:

```sh
xattr -dr com.apple.quarantine /Applications/DiskDiff.app
```

To scan the whole disk, add DiskDiff under System Settings → Privacy & Security → Full Disk
Access. Each release is signed ad hoc, so macOS may treat an update as a different app: if
scans lose access after updating, remove DiskDiff from that list and add it again.

### Windows

Download `DiskDiff_<version>_windows_x64.zip` from
[Releases](https://github.com/LightTitan/DiskDiff/releases), unzip it anywhere and run
`diskdiff.exe`. There is no installer. The app isn't signed, so SmartScreen may ask you to
confirm the first time (**More info**, then **Run anyway**). Some system folders can only be
read by an administrator: run DiskDiff as administrator to include them. OneDrive files that
are online only are never downloaded and count as the space they take on your PC.

### Linux

Download `DiskDiff_<version>_linux_x86_64.tar.gz`, unpack it and run `./install.sh`. It
installs `diskdiff` and `diskdiff-mcp` to `~/.local/bin` and adds a menu entry, without root
(`./install.sh --uninstall` removes them). You can also run `./diskdiff` straight from the
folder. It needs glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36 and later) and
Wayland or X11. A scan of `/` stays on the root filesystem: on btrfs and ZFS that includes
`/home` and other subvolumes or datasets, but never other disks, network shares, snapshots or
tmpfs.

## Use with AI agents

DiskDiff ships `diskdiff-mcp`, a [Model Context Protocol](https://modelcontextprotocol.io)
server that lets AI agents such as Claude Desktop, Claude Code and Cursor read your scan
history and start new scans. It reads the same store as the app and never deletes or moves
files: an agent can tell you what to remove and why, and you decide.

**Help → Copy AI agent setup** copies the config for your MCP client with the right path.
On macOS the server is `DiskDiff.app/Contents/MacOS/diskdiff-mcp`; on Windows and Linux it
sits next to `diskdiff`. For Claude Code:

```sh
claude mcp add diskdiff -- /Applications/DiskDiff.app/Contents/MacOS/diskdiff-mcp
```

For Claude Desktop (`claude_desktop_config.json`) or Cursor (`~/.cursor/mcp.json`):

```json
{ "mcpServers": { "diskdiff": { "command": "/Applications/DiskDiff.app/Contents/MacOS/diskdiff-mcp" } } }
```

Then ask things like "What grew on my disk since last week?" or "What could I safely clean up
in my home folder?". The `disk_checkup` prompt runs a full checkup.

Tools: `list_scans`, `browse`, `what_changed`, `largest_files`, `largest_folders`, `growth`,
`cleanup_suggestions`, `run_scan` and `scan_status`. A scan an agent asks for runs in the
DiskDiff app, which is started in the background if needed, so it gets the app's Full Disk
Access and shows up in the sidebar. Only when no app can be started does the server scan by
itself, with the agent's own permissions. Set `DISKDIFF_MCP_NO_APP=1` to always scan in the
server.

Agents that work through a shell can call the tools directly:

```sh
diskdiff-mcp call what_changed '{"path": "~/Downloads"}'
diskdiff-mcp tools    # every tool with its arguments
```

## Running

```sh
cargo run -p diskdiff                 # debug
./build.sh                            # release DiskDiff.app in target/ (--debug, --open, --install)
./build.sh --universal --dmg          # what a release ships: Apple silicon + Intel, packaged as a DMG
open target/DiskDiff.app
```

On Windows and Linux, `cargo build --release -p diskdiff -p diskdiff-mcp` builds both
binaries (`build.sh` is macOS-only for packaging). Linux needs `pkg-config clang
libfontconfig-dev libfreetype-dev libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev
libxcb1-dev libx11-xcb-dev libvulkan-dev libssl-dev` first.

`--universal` needs the Intel and Apple silicon standard libraries
(`rustup target add x86_64-apple-darwin aarch64-apple-darwin`).

Use the bundle for real full-disk scans. macOS grants Full Disk Access per app; a bare
binary started from a terminal borrows the terminal's permissions and may trigger privacy
prompts for Desktop, Documents and so on.

Data lives in `~/Library/Application Support/app.diskdiff.native/` (`%APPDATA%\DiskDiff Native` on
Windows, `~/.local/share/diskdiff` on Linux): `snapshots.db` and `settings.json`. While the app
runs it also writes `snapshots.control.json`, the local endpoint agents use to request scans.

## How scans are stored

Every scan is a Merkle tree, like git trees. Each directory becomes one node: a sorted
list of sub-directories (name, hash, recursive totals) and files (name, allocated size).
The node is keyed by the hash of its bytes. Scans keep every file, not just folder totals.

- **A rescan stores only what changed.** An unchanged folder has the same hash as last time,
  so it and everything below it is already stored. Identical folders inside one scan
  (duplicate `node_modules`, app frameworks) are stored once too.
- **Large folders are chunked.** Folders with over 1,024 entries are split at content-defined
  boundaries, so one changed file rewrites one chunk of about 200 entries, not the whole
  listing.
- **Comparing is proportional to the change.** Subtrees with equal hashes are skipped
  without being opened. That is what makes folder churn, "Changes", duplicates and
  "untouched" folders cheap.
- **Deleting a scan only touches what it owned.** Nodes are reference counted. Deleting a
  scan releases its root, and only nodes no other scan uses are removed. Free pages go back
  through incremental vacuum.
- Nodes are varint-encoded and zstd-compressed when larger than 96 bytes.

Measured on an M1 Pro:

| Benchmark | Result |
|---|---|
| Scan throughput | ~300k entries/s (`getattrlistbulk`, work-stealing walk) |
| `/Applications` rescan, nothing changed | 2.1 s, 0 bytes written |
| A history of 46 scans | 74 MB file (51 MB of node data) |
| Delete a scan | ~170 ms |

Cloud sizes are not stored. Inside cloud drives (`~/Library/CloudStorage`, iCloud Drive) the
browser and sidebar read each file's real length live from the disk and show it as a ☁ badge
next to the size on disk. Online-only files report their length without being downloaded, and
placeholder folders without a local listing are never opened (the process also tells macOS never
to fetch cloud files), so such totals are marked as a lower bound ("+").

## Developing

```sh
cargo test                                            # engine + app unit tests
cargo run --release -p diskdiff-engine --example bench -- ~/some/dir /tmp/b.db
cargo run -p diskdiff-engine --example demo -- /tmp/demo     # fake 9-scan history
DISKDIFF_DB=/tmp/demo/demo.db cargo run -p diskdiff
```

Development hooks (environment variables, see `app/src/dev.rs`):

- `DISKDIFF_DB`: use another store. Settings then live next to it.
- `DISKDIFF_SCREENSHOT=out.png`: save the window after 2.5 s and quit. Debug builds only.
- `DISKDIFF_APPEARANCE=dark|light`: force the appearance.
- `DISKDIFF_SCREEN=insights|largest|changes|manage`: open a screen.
- `DISKDIFF_REVEAL=/abs/path`: reveal a path in the browser.
- `DISKDIFF_KEYS="down right l"`: replay keystrokes.
- `DISKDIFF_ACTION=diskdiff::About`: dispatch an action by name, e.g. a menu command.
- `DISKDIFF_PRINT_MENUS=1`: print the menu bar.
- `DISKDIFF_MOUSE="d:40,520 m:40,600 u:40,600"`: replay mouse steps in window points (`d` press, `m` move held, `u` release, `h` hover, `c` click, `dc` double-click, `w:ms` wait).
- `DISKDIFF_PRINT_CLOUD=/abs/path`: print live cloud sizes of a folder's entries and quit. Launch the bundle with `open -n --env … --stdout out.txt target/DiskDiff.app` so it runs with the app's own permissions.
- `DISKDIFF_SCROLL="700,800:4,-18*25"`: replay a trackpad gesture at a window point (dx, dy per event, event count). Prefix the step with `L` for mouse-wheel lines and `S` for Shift, e.g. `700,800:LS0,3*4`.

### Releasing

1. Bump `version` in the root `Cargo.toml` and commit.
2. Tag the commit and push the tag, e.g. `git tag v0.2.2 && git push origin v0.2.2`.
3. The Release workflow builds the universal DMG, the Windows zip and the Linux tarball on
   GitHub and attaches them to one draft release. Write the notes there and publish it.

The workflow stops if the tag doesn't match the version in `Cargo.toml`. It can also be
started by hand from the Actions tab, which drafts a release for the current version.

UI conventions are in [CLAUDE.md](CLAUDE.md). The short version: the arrow cursor on every control,
the pointing hand only for links, and no all-caps labels.

## License

DiskDiff is free software, licensed under the [GNU General Public License v3.0 or later](LICENSE).
