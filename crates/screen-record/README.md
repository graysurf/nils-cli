# screen-record

## Overview

screen-record is a macOS 12+ and Linux CLI that records a single window (or a full display)
to a video file. On macOS it uses ScreenCaptureKit (`SCShareableContent` for discovery,
`SCStream` for capture) and AVFoundation (`AVAssetWriter` for mux/encoding). On Linux it relies on
X11 (`x11rb`) for window/display discovery and `ffmpeg` for capture/encoding. On Wayland-only
sessions, it can use an interactive portal picker (`--portal`) via xdg-desktop-portal
(`org.freedesktop.portal.ScreenCast` over DBus) + PipeWire. It also exposes parseable
window/app/display lists (X11) to make selection deterministic in scripts.

## Linux (X11 + Wayland portal)

Linux support targets X11/Xorg sessions (including XWayland when `DISPLAY` is set). Ubuntu 24.04 is
the CI/validation baseline, but other distros with X11 should work. For Wayland-only sessions
(no `DISPLAY`), `--portal` provides an interactive capture path.

Prerequisites:

- `ffmpeg` on `PATH` (example: `sudo apt-get install ffmpeg`).
- For X11 selectors and list modes: an X11 session with `DISPLAY` set.
- For `--portal` on Wayland-only sessions:
  - xdg-desktop-portal + a desktop backend (e.g. `xdg-desktop-portal-gnome` or
    `xdg-desktop-portal-kde`)
  - a PipeWire session (Ubuntu default)
- For `--audio system|mic|both`: `pactl` (PulseAudio compatibility) on `PATH`.

See the workspace [`BINARY_DEPENDENCIES.md`](../../BINARY_DEPENDENCIES.md) for the canonical install
matrix (rows for `ffmpeg`, `pactl`, `xdg-desktop-portal` + PipeWire, and the macOS `cwebp` WebP
fallback).

Selection parity:

- Recording selectors `--window-id`, `--active-window`, `--app`, `--display`, and `--display-id`
  are supported (X11).
- `--portal` is supported for recording and screenshots on Wayland-only sessions, but is
  interactive/user-driven (not deterministic for scripts).
- Screenshot mode remains window-only; `--display` and `--display-id` are invalid with
  `--screenshot`.
- Linux `display_id` values are X11/XRandR output ids. `--display` selects the XRandR primary output
  when available; otherwise it selects the first display in the deterministic list.

Linux examples:

```bash
screen-record --list-windows
screen-record --display --duration 3 --audio off --path "./recordings/display.mp4"
```

### Troubleshooting (Linux)

- **Wayland-only session + X11 selectors / list modes**: X11-only selectors and list modes require
  an X11 session. Use `--portal` for recording/screenshot or log into an Xorg session (Ubuntu
  example: **"Ubuntu on Xorg"**). You may see:

  ```text
  error: X11 selectors require X11 (DISPLAY is unset). Use --portal on Wayland-only sessions, or log into "Ubuntu on Xorg".
  ```

- **Wayland + XWayland (`DISPLAY` is set, but some apps are missing)**: only X11 client windows are
  discoverable/capturable. Wayland-native apps won’t appear in `--list-windows`; switch to Xorg.
- **Missing portal packages** (Wayland-only + `--portal`): install xdg-desktop-portal + a backend.
  Error example:

  ```text
  error: Wayland-only session detected but xdg-desktop-portal is missing.
  ```

- **ffmpeg missing portal FD support** (Wayland-only + `--portal`): install an ffmpeg build with
  PipeWire portal FD support. Error example:

  ```text
  error: ffmpeg PipeWire input does not appear to support portal FDs (missing -pipewire_fd in `ffmpeg -hide_banner -h demuxer=pipewire`).
  ```

- **Missing `ffmpeg`**: install it (Ubuntu):

  ```text
  sudo apt-get install ffmpeg
  ```

  Error example:

  ```text
  error: ffmpeg not found on PATH. Install it with: sudo apt-get install ffmpeg
  ```

- **Audio capture prerequisites (`--audio system|mic|both`)**: Linux audio capture uses PulseAudio
  compatibility via `pactl`. `--audio mic` resolves the PulseAudio default source; `--audio system`
  resolves the default sink and records its `.monitor` source; `--audio both` combines both inputs
  and requires a `.mov` container. On Ubuntu, install:

  ```text
  sudo apt-get install pulseaudio-utils pipewire-pulse
  ```

  Error example:

  ```text
  error: pactl not found on PATH (install pipewire-pulse or pulseaudio-utils)
  ```

- **Blank/occluded capture**: X11 region/window capture can include occlusion and typically cannot
  capture minimized windows. Keep the target visible and un-minimized while recording.

## Usage

```text
screen-record [options]
```

## Flags

| Flag | Value | Default | Description |
| --- | --- | --- | --- |
| `--screenshot` | (none) | (none) | Capture a single window screenshot and exit. |
| `--portal` | (none) | (none) | Use the system portal picker (Linux Wayland) instead of X11 selectors. |
| `--list-windows` | (none) | (none) | Print selectable windows as TSV and exit. |
| `--list-displays` | (none) | (none) | Print selectable displays as TSV and exit. |
| `--list-apps` | (none) | (none) | Print selectable apps as TSV and exit. |
| `--window-id` | `<id>` | (none) | Record a specific window id. |
| `--app` | `<name>` | (none) | Select a window by app/owner name (case-insensitive substring). |
| `--window-name` | `<name>` | (none) | Narrow `--app` selection by window title substring. |
| `--active-window` | (none) | (none) | Record the frontmost window on the current Space. |
| `--display` | (none) | (none) | Record the primary display. |
| `--display-id` | `<id>` | (none) | Record a specific display id. |
| `--duration` | `<seconds>` | (required for recording) | Record for N seconds. |
| `--audio` | `off\|system\|mic\|both` | `off` | Control audio capture. `both` requires `.mov`. |
| `--path` | `<path>` | (required for recording) | Output file path. Required for recording; optional for `--screenshot`. |
| `--metadata-out` | `<path>` | (none) | Write recording metadata JSON (success and failure paths). |
| `--diagnostics-out` | `<path>` | (none) | Write diagnostics manifest JSON and sidecar artifacts for recording mode. |
| `--format` | `mov\|mp4` | (auto) | Explicit container selection. Overrides extension. |
| `--image-format` | `png\|jpg\|webp` | (auto) | Screenshot output format. Overrides extension. |
| `--dir` | `<path>` | `./screenshots` | Output directory for `--screenshot` when `--path` is omitted. |
| `--if-changed` | (none) | (none) | In screenshot mode, skip publish when hash-distance is within threshold. |
| `--if-changed-baseline` | `<path>` | (none) | Baseline screenshot path used by `--if-changed`; defaults to current output file when present. |
| `--if-changed-threshold` | `<bits>` | `0` | Max hash-distance bits considered unchanged with `--if-changed`. |
| `--preflight` | (none) | (none) | Check macOS Screen Recording permission or Linux prerequisites, then exit. |
| `--request-permission` | (none) | (none) | Best-effort permission request + status check on macOS; on Linux runs `--preflight`. |
| `-h, --help` | (none) | (none) | Show help. |
| `-V, --version` | (none) | (none) | Show version. |

## Mode rules

- Exactly one mode must be selected: `--list-windows`, `--list-displays`, `--list-apps`,
  `--preflight`, `--request-permission`, `--screenshot`, or recording.
- Recording mode requires exactly one selector: `--portal`, `--window-id`, `--active-window`,
  `--app`, `--display`, or `--display-id`.
- Screenshot mode requires exactly one selector: `--portal`, `--window-id`, `--active-window`,
  or `--app`.
- Display selectors (`--display`, `--display-id`) are invalid with `--screenshot`.
- `--window-name` is only valid together with `--app`.
- `--duration` is required for recording mode.
- Press `Ctrl-C` to stop a recording early; `--duration` is still required and acts as an upper bound.
- `--dir` and `--image-format` are only valid with `--screenshot`.
- `--metadata-out` is only used in recording mode.
- `--diagnostics-out` is only used in recording mode.
- `--if-changed`, `--if-changed-baseline`, and `--if-changed-threshold` are only valid with `--screenshot`.
- Recording-only flags (`--duration`, `--audio`, `--format`) are not valid with `--screenshot`.
  `--portal` currently supports `--audio off` only.

## Output contract

- Success (recording/screenshot): stdout prints only the resolved output file path followed by `\n`.
- Success (list): stdout prints only TSV rows followed by `\n`.
- Success (preflight/request): stdout is empty; any user messaging goes to stderr.
- Recording writes to a staging file and publishes the target path only on success.
- When `--metadata-out <path>` is provided, recording mode writes JSON with fixed keys:
  `target`, `duration_ms`, `audio_mode`, `format`, `output_path`, `output_bytes`,
  `started_at`, `ended_at`, `error`.
- When `--diagnostics-out <path>` is provided, recording mode writes a versioned diagnostics
  manifest with keys:
  `schema_version`, `contract_version`, `source_output_path`, `source_output_bytes`,
  `generated_at`, `artifacts.contact_sheet`, `artifacts.motion_intervals`, `error`.
- Diagnostics sidecar naming convention:
  - Artifacts directory: `<diagnostics-out>.diagnostics/`
  - Contact sheet: `<recording-stem>-contact-sheet.svg`
  - Motion intervals: `<recording-stem>-motion-intervals.json`
- Errors: stdout is empty; stderr contains user-facing errors (no stack traces).

## Permission schema (library adapter)

- For automation integrations embedding `screen-record` as a Rust dependency, macOS permission state
  can be mapped to shared schema fields:
  `screen_recording`, `accessibility`, `automation`, `ready`, `hints`.
- Shared types live in `screen_record::types::{PermissionState, PermissionStatusSchema}` and
  macOS adapter helper is `screen_record::macos::permissions::permission_status()`.

## List output (TSV)

All list output is UTF-8 TSV with no header and one record per line. Tabs or newlines in string
fields are normalized to a single space. Sorting is deterministic.

### `--list-windows` column order

1. `window_id` (decimal)
2. `owner_name`
3. `window_title` (empty when missing)
4. `x` (decimal)
5. `y` (decimal)
6. `width` (decimal)
7. `height` (decimal)
8. `on_screen` (`true` or `false`)

Sorting: by `owner_name`, then `window_title`, then `window_id`.

### `--list-apps` column order

1. `app_name`
2. `pid` (decimal)
3. `bundle_id` (empty when missing)

Sorting: by `app_name`, then `pid`.

### `--list-displays` column order

1. `display_id` (decimal)
2. `width` (pixels; macOS reports points)
3. `height` (pixels; macOS reports points)

Sorting: by `display_id`.

## Selection rules

- `--window-id <id>` selects exactly that window id.
- `--active-window` selects the single frontmost window on the current Space.
- `--app <name>` matches windows by owner/app name substring (case-insensitive).
- `--window-name <name>` further filters by title substring (case-insensitive).
- `--display` selects the main display (macOS primary display; Linux XRandR primary output when
  available, otherwise the first deterministic display).
- `--display-id <id>` selects exactly that display id (macOS display id; Linux X11/XRandR output id).
- If multiple windows remain after filtering, and no single frontmost window can be chosen,
  selection is ambiguous and the CLI exits 2 with candidate output.

## Ambiguous selection errors

Ambiguous selection is a usage error (exit 2). The error format is fixed:

```text
error: multiple windows match --app "<app>"
error: refine with --window-name or use --window-id
<window_id>\t<owner_name>\t<window_title>\t<x>\t<y>\t<width>\t<height>\t<on_screen>
<window_id>\t<owner_name>\t<window_title>\t<x>\t<y>\t<width>\t<height>\t<on_screen>
```

Candidate rows are identical to `--list-windows` TSV output and are printed to stderr.

## Container selection (.mov vs .mp4)

- If `--format` is provided, its value selects the container.
- Otherwise, `.mov` or `.mp4` is selected from the `--path` extension.
- If no supported extension is present, the container defaults to `.mov`.
- If `--format` conflicts with the `--path` extension, exit 2 with a usage error.
- `--audio both` requires `.mov`; using `.mp4` exits 2 with a clear error.

## Screenshot format selection (.png vs .jpg vs .webp)

- If `--image-format` is provided, its value selects the output format.
- Otherwise, `.png`, `.jpg`/`.jpeg`, or `.webp` is selected from the `--path` extension.
- If `--path` has no extension (or `--path` is omitted), the format defaults to `.png`.
- If `--image-format` conflicts with the `--path` extension, exit 2 with a usage error.
- Note: WebP encoding on macOS is best-effort with a two-stage fallback chain:
  1. macOS ImageIO (`CGImageDestination` with the `org.webmproject.webp` UTI). Available when the
     installed macOS bundles a WebP encoder for ImageIO.
  2. `cwebp` (`brew install webp`). The capture is encoded as a temporary PNG via ImageIO, then
     piped through `cwebp -lossless` to produce the final `.webp`.
  If neither stage succeeds (no ImageIO WebP encoder and `cwebp` is missing or fails),
  `--image-format webp` exits with code 1. Use `--image-format png|jpg` as a workaround.

## Diff-aware screenshot capture (`--if-changed`)

- `--if-changed` is opt-in and affects screenshot mode only.
- Baseline selection:
  - If `--if-changed-baseline <path>` is set, that file is hashed as baseline.
  - Otherwise, if target output path exists, the existing output file is baseline.
  - If no baseline exists, capture is treated as changed and is published.
- Threshold:
  - `--if-changed-threshold <bits>` compares hash-distance bits (`0..=64`).
  - `0` means exact hash match is required to skip publish.
- Publish behavior:
  - Changed: staged capture replaces target output path.
  - Unchanged: staged capture is deleted and target output is preserved.

## Screenshot default naming

When `--screenshot` is used without `--path`, output is written under `./screenshots/` and the
filename is generated from:

- Local timestamp (`YYYYMMDD-HHMMSS`)
- Window identity: `win<id>` + owner name + window title (sanitized)

Shape:

```text
./screenshots/screenshot-20260101-000000-win100-Terminal-Inbox.png
```

## Exit codes

- `0`: Success (recording completed, list output printed, or preflight success).
- `1`: Runtime failure (permission denied, capture/encode failure).
- `2`: Usage error (invalid flags, ambiguous selection, invalid format).

## Environment

- `AGENTS_SCREEN_RECORD_TEST_MODE=1`: Use deterministic fixtures instead of macOS APIs. In test
  mode, recording copies a fixture file matching the selected container.

## Examples

List windows:

```bash
screen-record --list-windows
```

List apps:

```bash
screen-record --list-apps
```

Record by app name:

```bash
screen-record --app Terminal --duration 3 --audio off --path "./recordings/terminal.mov"
```

Record by window id:

```bash
screen-record --window-id 4811 --duration 5 --audio off --path "./recordings/window-4811.mov"
```

Record for a short duration:

```bash
screen-record --active-window --duration 2 --audio off --path "./recordings/active.mov"
```

Record the main display:

```bash
screen-record --display --duration 2 --audio off --path "./recordings/display.mov"
```

List displays:

```bash
screen-record --list-displays
```

Record a specific display id:

```bash
screen-record --display-id 1 --duration 2 --audio off --path "./recordings/display-1.mov"
```

Record with system audio:

```bash
screen-record --app Terminal --duration 5 --audio system --path "./recordings/terminal-audio.mov"
```

Capture a screenshot (default png + default naming):

```bash
screen-record --screenshot --active-window
```

Capture a screenshot as WebP:

```bash
screen-record --screenshot --app Terminal --image-format webp
```

Capture a screenshot to an explicit path:

```bash
screen-record --screenshot --window-id 4811 --path "./screenshots/window-4811.jpg"
```

Skip screenshot publish when unchanged:

```bash
screen-record --screenshot --app Terminal --path "./screenshots/inbox.png" --if-changed --if-changed-threshold 2
```

Record with metadata + diagnostics manifests:

```bash
screen-record --app Terminal --duration 3 --audio off --path "./recordings/terminal.mov" \
  --metadata-out "./recordings/terminal.metadata.json" \
  --diagnostics-out "./recordings/terminal.diagnostics.json"
```

## Docs

- [Docs index](docs/README.md)
