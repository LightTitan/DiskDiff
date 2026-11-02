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
