# OpenArchive — Issue Tracker

Issues found during the project review of 2026-11-22. Ordered by priority.

| ID | Title | Type | Priority | Status |
|---|---|---|---|---|
| OA-1 | ~~Path traversal ("zip slip") on extraction~~ | Security bug | Critical | **DONE** |
| OA-2 | ~~Entry filter matches by substring~~ | Bug | High | **DONE** |
| OA-3 | ~~GUI cannot open password-protected archives~~ | Bug | High | **DONE** |
| OA-4 | GUI swallows extraction errors | Bug | High | IN PROGRESS |
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
| OA-18 | `a x.zip -p…` silently creates an unencrypted zip | Security bug | Critical | TODO |
| OA-19 | AES-encrypted zips cannot be read | Bug | Medium | TODO |
| OA-20 | Arrow keys in an empty folder overflow / panic | Bug | Medium | TODO |
| OA-21 | ~~GUI lists 7z folders twice~~ | Bug | Medium | **DONE** |
| OA-22 | GUI "Test" button reports success without testing | Bug | High | TODO |
| OA-23 | 7z listing always reports entries as unencrypted | Bug | Low | TODO |

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
- Replaces the skip-and-warn fix from `07facff` (PR #1, `core::io::entry_target`): unsafe
  entries now refuse the archive instead of being skipped, rooted names are kept inside the
  output directory, and `.. ` / `:` names are also caught. Its traversal test was adapted.
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

## ~~OA-2 — Entry filter matches by substring~~ — DONE

**Type:** Bug · **Priority:** High · **Component:** `formats`, `gui` · **Status:** DONE

### Description

~~Filters use `name.contains(filter)`. Extracting `a.txt` also extracts `data.txt`,
`a.txt.bak`, `x/a.txt/...`. With flat extraction (GUI single-file extract, preview) the extra
matches overwrite one another or the intended file.~~

### Resolution

- New `core::paths::matches_filter`: a filter selects the entry with exactly that path and,
  when it names a directory, everything below it (`docs` → `docs`, `docs/`, `docs/a.txt`; not
  `docs2/…` or `old/docs/…`). `/` and `\` are equivalent; leading `./` or `/` and trailing
  separators are ignored. Case-sensitive. No filter, or `.` / `/`, selects everything.
  Allocation-free per entry.
- All 7 filter sites (zip ×1, tar ×2, 7z ×4) use it, so CLI `x`/`e` and every GUI extract and
  preview action get the fix.
- Tests: 4 unit tests in `core::paths`; CLI regression test
  `test_filter_selects_exact_entries_only` (zip, 7z, tar, tar.gz × 1 and 4 threads × `x` and
  flat `e`), verified to fail on the old substring matching.
- Behaviour change: CLI partial names no longer match (`x a.zip readme` used to extract
  `docs/readme.md`). Wildcards (`*.txt`) are still not supported; consider adding them with
  `-r` (OA-8).

---

## ~~OA-3 — GUI cannot open password-protected archives~~ — DONE

**Type:** Bug · **Priority:** High · **Component:** `gui` · **Status:** DONE

### Description

~~Every GUI call to `get_reader` / `ExtractOptions` passes `password: None`
(`src/gui.rs` `extract_and_open_temp`, `handle_context_action`, `extract_current`).
Encrypted 7z/zip archives cannot be extracted or previewed from the GUI.~~

### Resolution

- `formats::password_problem(&anyhow::Error) -> Option<PasswordProblem>` classifies errors as
  `Required` / `Incorrect` by walking the error chain, including the payload of `io::Error`s
  (7z `PasswordRequired` / `MaybeBadPassword`; zip `InvalidPassword` /
  `UnsupportedArchive(PASSWORD_REQUIRED)`).
- GUI archive actions (list, open/preview, open with, extract item, extract all) are now an
  `ArchiveOp` run through `run_archive_op`. On a password problem it opens a
  "Password required" dialog and retries the same operation with the entered password; a wrong
  password re-opens it with "Wrong password, try again." Cancel / Escape closes it.
- Passwords are remembered in memory per archive for the session (dropped when they fail), so
  browsing folders of an unlocked archive does not re-prompt.
- Global keyboard shortcuts (Backspace = up, Enter = open, arrows) are disabled while the
  dialog is open.
- Tests: 3 classifier tests in `formats` (real encrypted 7z; zip errors through wrappers;
  non-password errors), 5 GUI state tests (prompt → wrong → right → browse → extract all;
  extract item retry; cancel; unencrypted archive; corrupt archive reports instead of
  prompting). Checked in the running GUI: dialog layout, status bar, unlocking with the right
  password.
- Note: our 7z writer encrypts headers, so even listing needs the password. Encrypted zips
  only work for ZipCrypto until OA-19.

---

## OA-4 — GUI swallows extraction errors

**Type:** Bug · **Priority:** High · **Component:** `gui` · **Status:** IN PROGRESS

### Description

Extraction results are checked with `if let Ok(()) = ...`; on failure nothing is shown.
Same for `fs::copy` / `create_dir_all` results discarded with `let _ =`.

### Suggested fix

Match on the result and show the error via `show_notification` (or a modal for serious
failures).

**Progress:** with OA-3, all archive operations (list, preview, extract item, extract all)
report failures in a notification and the status bar. Remaining: disk-mode "Extract" copy
(`fs::copy` / `create_dir_all` with `let _ =`), the temp dir for previews, and opening files
externally (`spawn()` results ignored).

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

**Progress:** path computation (OA-1) and filter matching (OA-2) now live in
`core::paths`. Remaining duplication: the per-format thread-count resolution and the
directory pre-creation loops.

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

24 clippy warnings (collapsible `if`, manual prefix strip, needless refs, manual clamp);
was 31 before OA-2 replaced some of the flagged filter blocks.

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

---

## OA-18 — `a x.zip -p…` silently creates an unencrypted zip

**Type:** Security bug · **Priority:** Critical · **Component:** `formats::zip` · **Status:** TODO

### Description

The zip writer never reads `CompressionOptions::password`. `openarchive a pw.zip src -psecret`
reports success and writes a plain zip whose contents anyone can read (verified with Python's
`zipfile`: no entry has the encryption flag, contents readable without a password). Users
believe their data is protected.

### Suggested fix

Enable the `zip` crate's `aes-crypto` feature and use
`SimpleFileOptions::with_aes_encryption(AesMode::Aes256, password)` for every file entry,
including the parallel batch and temp-spill paths. Until then, refuse `-p` for zip with a
clear error rather than ignoring it. Add a test that a `-p` zip cannot be read without the
password.

---

## OA-19 — AES-encrypted zips cannot be read

**Type:** Bug · **Priority:** Medium · **Component:** `formats::zip` · **Status:** TODO

### Description

`zip` is built without `aes-crypto`, so only legacy ZipCrypto archives can be decrypted.
AES zips (the default for 7-Zip / WinZip "encrypt") fail with
`UnsupportedArchive("AES encrypted files cannot be decrypted without the aes-crypto feature.")`.

### Suggested fix

Enable `aes-crypto` (shared with OA-18) and add a roundtrip test; `password_problem` already
classifies the resulting password errors for the GUI.

---

## OA-20 — Arrow keys in an empty folder overflow / panic

**Type:** Bug · **Priority:** Medium · **Component:** `gui` · **Status:** TODO

### Description

In `update()`, the guard `if !total == 0 { return; }` applies bitwise NOT to `total`, so it
never returns. With no items, ArrowDown computes `total - 1`, which panics in debug builds
(overflow) and selects a nonsense index in release.

### Suggested fix

`if total == 0 { return; }`, plus a test of the selection helpers on an empty list.

---

## ~~OA-21 — GUI lists 7z folders twice~~ — DONE

**Type:** Bug · **Priority:** Medium · **Component:** `gui` · **Status:** DONE

### Description

~~7z stores folder entries without a trailing slash (`src`). `filter_archive_entries` added
such an entry as a row without recording it in `seen_dirs`, and the folder was added again
for the files inside it, so every 7z folder appeared twice.~~

### Resolution

Folder entries are now recorded in `seen_dirs` and skipped when already listed.
Test: `gui::listing_tests::folder_entries_are_listed_once` (7z-style before/after its files,
zip-style with trailing slash). Found while testing OA-3.

---

## OA-22 — GUI "Test" button reports success without testing

**Type:** Bug · **Priority:** High · **Component:** `gui` · **Status:** TODO

### Description

The toolbar "✔ Test" button only calls
`show_notification("Archive testing: Everything is Ok")`; it never runs `ArchiveReader::test`.
A corrupt archive is reported as OK.

### Suggested fix

Add `ArchiveOp::Test` and run `reader.test(password)` through `run_archive_op` (gets the
password prompt for free); report the real result. Ideally on a worker thread (OA-5).

---

## OA-23 — 7z listing always reports entries as unencrypted

**Type:** Bug · **Priority:** Low · **Component:** `formats::sevenz` · **Status:** TODO

### Description

`SevenZHandler::list` hardcodes `encrypted: false`, so `ArchiveEntry::encrypted` is wrong for
7z archives with encrypted content but plain headers.

### Suggested fix

Set it from the entry's block coders (AES method id present), as `zip` does with
`file.encrypted()`.
