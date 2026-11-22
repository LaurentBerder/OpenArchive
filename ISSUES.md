# OpenArchive — Issue Tracker

Issues found during the project review of 2026-11-22. Ordered by priority.

| ID | Title | Type | Priority | Status |
|---|---|---|---|---|
| OA-1 | ~~Path traversal ("zip slip") on extraction~~ | Security bug | Critical | **DONE** |
| OA-2 | Entry filter matches by substring | Bug | High | TODO |
| OA-3 | GUI cannot open password-protected archives | Bug | High | TODO |
| OA-4 | GUI swallows extraction errors | Bug | High | TODO |
| OA-5 | GUI freezes during extraction | Bug | Medium | TODO |
| OA-6 | GUI "Extract" of an on-disk folder copies nothing | Bug | Medium | TODO |
| OA-7 | Tar links are extracted as empty regular files | Bug | Medium | TODO |
| OA-8 | `-r` and `-y` switches are parsed but ignored | Bug | Medium | TODO |
| OA-9 | No overwrite prompt; existing files silently replaced | Improvement | Medium | TODO |
| OA-10 | `l` command prints blank dates | Bug | Low | TODO |
| OA-11 | Magic-byte format detection is unreliable | Bug | Low | TODO |
| OA-12 | mmap read path: robustness follow-ups | Tech debt | Low | TODO |
| OA-13 | Duplicated extraction path/filter logic | Tech debt | Low | TODO |
| OA-14 | Hardcoded version string in CLI banner | Tech debt | Low | TODO |
| OA-15 | Clippy warnings | Tech debt | Low | TODO |
| OA-16 | Add `.tar.xz` / `.txz` support | Feature | Low | TODO |
| OA-17 | Clean up untracked mmap benchmark artifacts | Chore | Low | TODO |

---

## ~~OA-1 — Path traversal ("zip slip") on extraction~~ — DONE

**Type:** Security bug · **Priority:** Critical · **Component:** `formats` · **Status:** DONE

### Description

~~All extractors build the output path with `options.output_dir.join(name)` using the raw,
untrusted entry name from the archive. An entry named `../evil.txt`, `/etc/x`, `C:\x` or
`..\x` is written outside the chosen output directory. Reproduced with crafted `.zip` and
`.tar` archives: `openarchive x evil.zip -o out` wrote `escaped_zip.txt` next to `out/` and
reported success. The 7z extractor uses the identical pattern. The GUI shares the same code.~~

~~Affected sites: `src/formats/zip.rs` (extract planning), `src/formats/tar.rs` (parallel
planning and sequential path), `src/formats/sevenz.rs` (pre-pass, sequential, pipelined and
block-parallel paths).~~

### Resolution

- New `src/core/paths.rs`: `sanitize_entry_name` / `entry_target` turn untrusted entry names
  into paths that cannot leave the output directory. `/` and `\` are both separators; leading
  separators and `.` are dropped (`/etc/x` → `out/etc/x`); `..`, dot/space-only segments
  (Windows `.. ` bypass), NUL, and on Windows `:` (drive / alternate data stream) are rejected.
- All 7 extraction sites (zip, tar ×2, 7z ×4) now use `entry_target`, including flat (`e`)
  extraction.
- zip, 7z and multi-threaded plain tar reject the archive before writing any file; streamed
  tar / tar.gz / tar.bz2 stop at the offending entry. Error:
  `refusing to extract unsafe archive entry "../x": contains a parent-directory component`.
- Tests: 6 unit tests in `core::paths`, 4 CLI regression tests in `tests/cli_tests.rs`
  (`test_zip_slip_rejected`, `test_tar_slip_rejected`, `test_7z_slip_rejected`,
  `test_rooted_entry_extracts_inside_output_dir`).
- Not covered here: a pre-existing symlink *inside* the output directory could still redirect
  writes; links in archives are not created today (see OA-7).

### Suggested fix

- Add a single helper that converts an entry name into a safe relative path: split on `/`
  and `\`, drop empty and `.` segments, strip leading separators, reject `..` (and, on
  Windows, segments with `:` or made only of dots/spaces, which Win32 normalizes to `..`).
  Verify every resulting component is `Component::Normal`.
- Use it at every extraction site, for both full and flat (`e`) extraction.
- Reject the archive before writing anything where the format allows a pre-scan (zip, 7z,
  plain tar parallel path); fail at the offending entry for streamed tar.gz/tar.bz2.

### Acceptance criteria

- Extracting an archive containing `../x` fails with a clear error and writes nothing outside
  the output directory, for zip, tar (single and multi-threaded) and 7z.
- Unit tests for the sanitizer; CLI regression tests per format.

---

## OA-2 — Entry filter matches by substring

**Type:** Bug · **Priority:** High · **Component:** `formats`, `gui` · **Status:** TODO

### Description

Filters use `name.contains(filter)`. Extracting `a.txt` also extracts `data.txt`,
`a.txt.bak`, `x/a.txt/...`. With flat extraction (GUI single-file extract, preview) the extra
matches overwrite one another or the intended file.

### Suggested fix

Match exactly, or as a directory prefix: `name == f || name.starts_with(&format!("{f}/"))`,
after normalizing separators and trailing `/`. If wildcard support is wanted (7-Zip style
`*.txt`), use a glob matcher explicitly rather than substring.

---

## OA-3 — GUI cannot open password-protected archives

**Type:** Bug · **Priority:** High · **Component:** `gui` · **Status:** TODO

### Description

Every GUI call to `get_reader` / `ExtractOptions` passes `password: None`
(`src/gui.rs` `extract_and_open_temp`, `handle_context_action`, `extract_current`).
Encrypted 7z/zip archives cannot be extracted or previewed from the GUI.

### Suggested fix

Detect encryption (`ArchiveEntry::encrypted`, or a decrypt error), show a password dialog,
cache the password per opened archive, and pass it through.

---

## OA-4 — GUI swallows extraction errors

**Type:** Bug · **Priority:** High · **Component:** `gui` · **Status:** TODO

### Description

Extraction results are checked with `if let Ok(()) = ...`; on failure nothing is shown.
Same for `fs::copy` / `create_dir_all` results discarded with `let _ =`.

### Suggested fix

Match on the result and show the error via `show_notification` (or a modal for serious
failures).

---

## OA-5 — GUI freezes during extraction

**Type:** Bug · **Priority:** Medium · **Component:** `gui` · **Status:** TODO

### Description

Extraction runs synchronously inside the egui `update` loop, so the window stops responding
for large archives.

### Suggested fix

Run extraction on a worker thread; report progress/results through a channel polled in
`update` (call `ctx.request_repaint()`); show a progress bar and allow cancel.

---

## OA-6 — GUI "Extract" of an on-disk folder copies nothing

**Type:** Bug · **Priority:** Medium · **Component:** `gui` · **Status:** TODO

### Description

In disk mode, `ContextAction::Extract` on a directory only calls `create_dir_all(target)`;
contents are not copied.

### Suggested fix

Copy recursively with `walkdir`, or hide the action for on-disk folders.

---

## OA-7 — Tar links are extracted as empty regular files

**Type:** Bug · **Priority:** Medium · **Component:** `formats::tar` · **Status:** TODO

### Description

The sequential tar path treats every non-directory entry as a file, so symlinks and hard
links become empty files (and char/block/fifo entries too).

### Suggested fix

Handle entry types explicitly: skip special files with a warning; for links either skip with
a warning or recreate them, validating the link target stays inside the output directory
(see OA-1).

---

## OA-8 — `-r` and `-y` switches are parsed but ignored

**Type:** Bug · **Priority:** Medium · **Component:** `cli` · **Status:** TODO

### Description

`recurse` and `assume_yes` are set by the parser and advertised in help, but nothing reads
them. Directories are always recursed.

### Suggested fix

Either implement them (`-y` pairs with OA-9) or remove them from help until implemented.

---

## OA-9 — No overwrite prompt; existing files silently replaced

**Type:** Improvement · **Priority:** Medium · **Component:** `cli`, `formats` · **Status:** TODO

### Description

`overwrite: true` is hardcoded in CLI and GUI; `ExtractOptions::overwrite` is never honoured.

### Suggested fix

Honour `overwrite`; add 7-Zip style `-ao{a|s|u}` switches; prompt in CLI unless `-y`; ask in
the GUI.

---

## OA-10 — `l` command prints blank dates

**Type:** Bug · **Priority:** Low · **Component:** `main` · **Status:** TODO

### Description

`src/main.rs` prints a hardcoded blank `date_str` although `ArchiveEntry::modified` is
populated.

### Suggested fix

Format `entry.modified` as `%Y-%m-%d %H:%M:%S` (local time), blank when `None`.

---

## OA-11 — Magic-byte format detection is unreliable

**Type:** Bug · **Priority:** Low · **Component:** `core` · **Status:** TODO

### Description

Any file starting with `BZ` is treated as tar.bz2; plain tar without extension is not
detected (signature `ustar` at offset 257); gzip/bzip2 are assumed to contain a tar.

### Suggested fix

Check `BZh` + level digit; read 262 bytes and look for `ustar` at 257; consider peeking
inside gz/bz2 streams.

---

## OA-12 — mmap read path: robustness follow-ups

**Type:** Tech debt · **Priority:** Low · **Component:** `core::io` · **Status:** TODO

### Description

Uncommitted mmap change in `copy_file_to`: if the source file is truncated while mapped,
the process can crash (SIGBUS on Unix). A mapping failure after a partial write returns an
error instead of falling back.

### Suggested fix

Resume with buffered reads from `offset` on mapping failure; document the
concurrent-modification limitation, or enable mmap on Windows only.

---

## OA-13 — Duplicated extraction path/filter logic

**Type:** Tech debt · **Priority:** Low · **Component:** `formats` · **Status:** TODO

### Description

Filter matching and target-path computation are copy-pasted ~6 times across 3 formats,
which is how OA-1 and OA-2 spread everywhere.

### Suggested fix

Centralize in `core` (`entry_target`, `matches_filter`) and use from all formats.

---

## OA-14 — Hardcoded version string in CLI banner

**Type:** Tech debt · **Priority:** Low · **Component:** `main` · **Status:** TODO

### Description

`print_banner` hardcodes `0.1.0`.

### Suggested fix

Use `env!("CARGO_PKG_VERSION")`.

---

## OA-15 — Clippy warnings

**Type:** Tech debt · **Priority:** Low · **Status:** TODO

### Description

31 clippy warnings (collapsible `if`, manual prefix strip, needless refs, manual clamp).

### Suggested fix

`cargo clippy --fix --all-targets`, then add `cargo clippy -- -D warnings` to CI.

---

## OA-16 — Add `.tar.xz` / `.txz` support

**Type:** Feature · **Priority:** Low · **Component:** `formats::tar` · **Status:** TODO

### Description

xz-compressed tarballs are common and unsupported, although `lzma-rust2` is already a
dependency.

### Suggested fix

Add `ArchiveFormat::TarXz` with xz reader/writer from `lzma-rust2`, extension and magic
(`FD 37 7A 58 5A 00`) detection.

---

## OA-17 — Clean up untracked mmap benchmark artifacts

**Type:** Chore · **Priority:** Low · **Status:** TODO

### Description

`mmap-*.log` files and `mmap_bench/` are untracked at the repo root.

### Suggested fix

Delete, move under `benches/`, or add to `.gitignore`.
