# Changelog

All notable changes to OpenArchive are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- Wildcards `*` and `?` in file names for every command, also on Windows where the shell does not expand them: `a` picks the files to add (stored without the folder part, like 7-Zip), `x`, `e`, `l` and `t` pick archive entries. Case-insensitive on Windows only.
- `-r` now makes wildcards also match in subfolders (`-r-`: top level only); it was parsed but ignored before.
- Overwrite handling: the CLI asks for each existing file like 7-Zip (Yes / No / Always / Skip all / Auto rename all / Quit; exit code 255 on Quit) and never blocks without a terminal; `-y` and `-aoa` overwrite, `-aos` skips, `-aou` renames the new file. The GUI asks once before extracting: Overwrite all / Skip existing / Keep both / Cancel.
- `.tar.xz` / `.txz` archives (single- and multi-threaded xz).
- GUI: progress percentage and a Cancel button for long operations, which now run in the background; quitting during one asks for confirmation.
- GUI: password prompt for encrypted archives (remembered per archive for the session).
- GUI: Shift-click selects a range and Ctrl-click toggles an item.
- Multi-threaded TAR.GZ and TAR.BZ2 compression (block-parallel gzip / bzip2 streams), and decoding of compressed TAR on a read-ahead thread.
- Memory-mapped reads for very large input files.
- Ignored allocation profiling harness (`tests/alloc_profile.rs`) that reports allocation counts and peak live bytes for archive creation and extraction.

### Changed
- 7z compression now encodes solid blocks concurrently across `-mmt` worker threads using `sevenz-rust2`'s `prepare_block` / `push_prepared_block`; blocks are appended in archive order from the writer thread.
- A single oversized 7z block (for example one huge file) uses multi-threaded LZMA2 inside its own stream, with memory bounded by the chunk size.
- ZIP compression now batches files by uncompressed bytes (and entry count), uses a sliding window to bound the reorder buffer, and spills oversized batches to temporary sub-archives so large files compress in parallel without unbounded RAM.
- Plain TAR extraction now indexes entry offsets and extracts small-file archives in parallel; large-entry archives stay sequential to avoid concurrent-write regressions under Windows AV. Flat, compressed, linked/special, and duplicate-name archives use the ordered path.
- TAR creation now writes explicit GNU headers from opened-file metadata and uses the shared sequential/buffered I/O path instead of `tar::Builder::append_path_with_name` / `append_dir`.
- Windows I/O: traversal uses `DirEntry` file types instead of extra metadata calls; file reads use size-aware 8 KB/256 KB buffers with `FILE_FLAG_SEQUENTIAL_SCAN`; archive writers use 1 MB output buffers; small extracted entries are written directly.
- 7z compression now caps simultaneous LZMA2 encoders against a 1.5 GiB estimated encoder-memory budget, reducing effective worker count at high levels or large `-md` dictionaries.
- 7z extraction now decodes independent blocks concurrently on multi-block archives, buffering entries through a 64 MB budget and serializing disk writes on a single writer thread; single-block and flat extraction keep the ordered pipeline.
- Memory: file copies reuse one 256 KB thread-local buffer instead of allocating per entry; 7z prepared blocks and the 7z extraction queue have explicit 256 MB / 64 MB budgets.
- Migrated the 7z backend from `sevenz-rust` 0.6 to `sevenz-rust2` 0.25.
- Archives are opened by their content first and their extension second, so a misnamed archive still opens; detection checks full signatures and validates TAR headers.
- `l` and `t` honour the file names they are given.
- `-ao` modes other than `a`, `s` and `u` are rejected instead of ignored.

### Security
- Extraction refuses archives with entry names that would escape the output directory (`..`, absolute paths, drive and stream prefixes on Windows), before writing anything.
- `a … -p` for `.zip` encrypts entries with AES-256; before, the password was ignored and the zip was written unencrypted. AES-encrypted zips can now be read.
- The GUI opens files through the Windows shell API instead of `cmd /c start`, so a file name containing `&` can no longer run commands.

### Fixed
- Extracting an encrypted 7z where a file cannot be written (full disk, permissions) reports the write error instead of asking for the password again.
- Existing files are no longer replaced silently on extraction, and `-y` now has an effect.
- File-name filters on extraction match exact paths (and folder contents) instead of any name containing the text.
- TAR hard links are extracted as copies, and symbolic links and special entries are skipped with a warning instead of being written as empty files.
- Multi-member gzip and multi-stream bzip2 TAR files are read completely.
- GUI: the Test button actually tests the archive; 7z folders are listed once; arrow keys in an empty folder no longer panic; copying folders from disk works and disk errors are reported.
- `-mmt` now changes 7z compression wall-clock time instead of only changing solid-block boundaries.
- ZIP parallel compression can no longer accumulate an unbounded reorder buffer; in-flight batch data is capped at 256 MB.
- `-mx=0` now stores 7z entries with the COPY method, matching 7-Zip.
- `-md` now accepts the 4 GB maximum (`-md=32`, `-md=4g`) instead of silently failing to parse.
- 7z extraction peak memory is now bounded by a 64 MB queue budget instead of an item count that could scale with thread count and file size.
- Removed redundant metadata syscalls and per-file 64 KB buffer allocations on small-file archive creation and extraction paths.

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

[Unreleased]: https://github.com/LaurentBerder/OpenArchive/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/LaurentBerder/OpenArchive/releases/tag/v0.1.0
