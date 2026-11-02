# DiskDiff

DiskDiff scans your disk over time and shows what changed between scans: which folders
grew, what was added, moved or deleted, and where your free space went. Each scan is kept
as a snapshot, so you can compare any two of them, browse a past scan like Finder, and spot
folders that keep growing.

It is a native app written in Rust, with the UI in [GPUI](https://www.gpui.rs) through
gpui-kit. It runs on macOS 12 or later. The engine also builds on Windows, but the app is
developed and tested on macOS.

| Crate | What |
|---|---|
| `engine` (`diskdiff-engine`) | Scanner, snapshot store, diffs, insights |
| `app` (`diskdiff`) | The window: graph, comparison summary, column browser, insights, manage scans |

## Running

```sh
cargo run -p diskdiff                 # debug
./build.sh                            # release DiskDiff.app in target/ (--debug, --open, --install)
open target/DiskDiff.app
```

Use the bundle for real full-disk scans. macOS grants Full Disk Access per app; a bare
binary started from a terminal borrows the terminal's permissions and may trigger privacy
prompts for Desktop, Documents and so on.

Data lives in `~/Library/Application Support/app.diskdiff.native/` (`%APPDATA%\DiskDiff Native` on
Windows): `snapshots.db` and `settings.json`.

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
- `DISKDIFF_MOUSE="d:40,520 m:40,600 u:40,600"`: replay mouse steps in window points (`d` press, `m` move held, `u` release, `h` hover, `c` click, `w:ms` wait).
- `DISKDIFF_PRINT_CLOUD=/abs/path`: print live cloud sizes of a folder's entries and quit. Launch the bundle with `open -n --env … --stdout out.txt target/DiskDiff.app` so it runs with the app's own permissions.
- `DISKDIFF_SCROLL="700,800:4,-18*25"`: replay a trackpad gesture at a window point (dx, dy per event, event count). Prefix the step with `L` for mouse-wheel lines and `S` for Shift, e.g. `700,800:LS0,3*4`.

UI conventions are in [CLAUDE.md](CLAUDE.md). The short version: the arrow cursor on every control,
the pointing hand only for links, and no all-caps labels.

## License

DiskDiff is free software, licensed under the [GNU General Public License v3.0 or later](LICENSE).
