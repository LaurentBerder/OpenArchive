# Changelog

All notable changes to OpenArchive are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.1.0] — 2026-11-05

### Added

#### Core engine
- Modular format-agnostic archive engine (`src/core/mod.rs`) with `ArchiveReader` and `ArchiveWriter` traits
- **7z** support via `sevenz-rust` — LZMA2 compression, solid blocks, header encoding, full interoperability with 7-Zip
- **ZIP** support via `zip-rs` — Deflate / Store methods
- **TAR / TAR.GZ / TAR.BZ2** support via `tar-rs` with `flate2` and `bzip2` streaming filters
- Format auto-detection from file extension

#### CLI (`openarchive.exe`)
- 7-Zip-compatible command set: `a` (add), `x` (extract with paths), `e` (extract flat), `l` (list), `t` (test)
- Switch support: `-o{dir}`, `-p{password}`, `-mx={0..9}`, `-r` (recursive), `-y` (yes-to-all)
- `gui` subcommand and bare invocation launch the GUI

#### GUI file manager (`openarchive-gui.exe`)
- File manager interface similar to 7-Zip File Manager
- Navigate the filesystem and inside archive files seamlessly
- Double-click to open files with the default application, or enter folders/archives
- Right-click context menu: Open, Open With…, Extract File/Folder…, Properties
- **Multi-item selection**:
  - `Ctrl+Click` — toggle individual item
  - `Shift+Click` — extend range from anchor
  - `Ctrl+A` — select all items
  - `Shift+Arrow` — extend selection with keyboard
  - **Rubber-band drag** — drag on empty space to select a region
  - `Escape` — clear selection
- Mouse Back button (Button 4) → navigate up / back
- Mouse Forward button (Button 5) → navigate forward in history
- `Backspace` / `Alt+Up` → navigate up / back
- `Enter` → open focused item; `Escape` → clear selection; `F5` → refresh
- Address bar with Browse button and Up button
- Notification toasts for user feedback
- Properties window (name, type, size, packed size, location)
- Extract all / extract selected to a chosen directory
- Window / taskbar icon from embedded ICO file

#### Installer
- Windows MSI installer (`openarchive-0.1.0-x86_64.msi`) generated with WiX Toolset v3
- Installs `openarchive.exe` and `openarchive-gui.exe` to `Program Files\OpenArchive\bin\`
- Creates a Start Menu shortcut for the GUI
- Optionally adds install directory to `PATH`
- `build-msi.ps1` helper script for reproducible MSI builds

#### Developer tooling
- Integration test suite: roundtrip create + extract tests for 7z and ZIP formats
- `wix/main.wxs` WiX source for the MSI installer
- `.gitignore` excluding `target/` and `wix314-tools/`

[0.1.0]: https://github.com/LaurentBerder/OpenArchive/releases/tag/v0.1.0
