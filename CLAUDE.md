# DiskDiff — project conventions

## UI conventions

### Cursor: hand for links, arrow for buttons
Follow the [macOS Human Interface cursor guidance](https://support.apple.com/en-mz/guide/mac-help/mh35695/mac):
the pointing-hand cursor signals a **link** (something that navigates/opens), not a
generic clickable control. Native macOS controls keep the normal arrow on hover, and
DiskDiff should feel like a native Mac app.

- **Buttons, toggles, close/×, segmented controls** → `.cursor_default()`, or a specific
  cursor such as `CursorStyle::ResizeLeftRight` for resize handles. Never `.cursor_pointer()`.
- **Links and link-like elements** (text that navigates, e.g. text that reveals a folder
  in the browser) → `.cursor_pointer()`, usually through `widgets::link`, which also
  underlines on hover.

Default new interactive elements to `.cursor_default()` and only opt into the pointing
hand when the element genuinely behaves like a link.

### Labels
Sentence case everywhere. No all-caps labels.

## Code conventions

### Test data and examples
The repository is public. Test fixtures, examples and demo data use generic names
(`Project`, `app`, `Photos`, `Downloads/film.mp4`), never real folders, projects, files or
paths from anyone's machine, even when a bug was found on a real disk.

### Paths (DiskDiff runs on macOS, Windows and Linux)
Code is written on a Mac, so Unix habits slip in; every one of these broke Windows once.

- Keep paths as `Path`/`PathBuf` and compare them as paths, never as strings. `Path`
  equality compares components, so `/` vs `\` doesn't matter; string equality does.
- Every path from outside (user input, a scan root, an agent's argument) goes through
  `platform::canonical_root` (or `select::normalize` in the CLI) before it's compared
  with stored roots. Don't call `canonicalize()` yourself: on Windows it adds `\\?\`.
- Paths stored inside a scan are `/`-separated text. Turn them into real paths part by
  part (`LargeFile::abs`, `Path::extend(rel.split('/'))`), never with `root.join(rel)`.
- Tests: build expected paths with `Path::join`, and when comparing text output (JSON,
  reports) convert `/` to `std::path::MAIN_SEPARATOR`. Don't use Unix absolute paths
  like `"/x"` as "some absolute path": on Windows they aren't absolute.
- Don't pass paths containing `/` to Windows commands (`cmd`, `mklink`): `/x` is a flag
  there. Join part by part.
- Loopback network behaviour differs too: on Windows a connection to a closed port times
  out instead of being refused.

### Paths (DiskDiff runs on macOS, Windows and Linux)
Code is written on a Mac, so Unix habits slip in; every one of these broke Windows once.

- Keep paths as `Path`/`PathBuf` and compare them as paths, never as strings. `Path`
  equality compares components, so `/` vs `\` doesn't matter; string equality does.
- Every path from outside (user input, a scan root, an agent's argument) goes through
  `platform::canonical_root` (or `select::normalize` in the CLI) before it's compared
  with stored roots. Don't call `canonicalize()` yourself: on Windows it adds `\\?\`.
- Paths stored inside a scan are `/`-separated text. Turn them into real paths part by
  part (`LargeFile::abs`, `Path::extend(rel.split('/'))`), never with `root.join(rel)`.
- Tests: build expected paths with `Path::join`, and when comparing text output (JSON,
  reports) convert `/` to `std::path::MAIN_SEPARATOR`. Don't use Unix absolute paths
  like `"/x"` as "some absolute path": on Windows they aren't absolute.
- Don't pass paths containing `/` to Windows commands (`cmd`, `mklink`): `/x` is a flag
  there. Join part by part.
- Loopback network behaviour differs too: on Windows a connection to a closed port times
  out instead of being refused.
