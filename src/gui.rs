use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Instant;
use eframe::egui::{self, Color32, FontId, Key, PointerButton, Rect, RichText, Sense, Vec2};

use crate::core::{ArchiveEntry, ArchiveFormat, ExtractOptions, Progress};
use crate::formats::{get_reader, is_cancelled, password_problem};

// ─── Navigation Location ──────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum NavLocation {
    Disk(PathBuf),
    Archive {
        archive_path: PathBuf,
        internal_prefix: String,
    },
}

// ─── Display Item ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct DisplayItem {
    pub name: String,
    pub is_dir: bool,
    pub is_archive: bool,
    pub size: u64,
    pub compressed_size: Option<u64>,
}

// ─── Context Menu Actions ─────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum ContextAction {
    Open(DisplayItem),
    OpenWith(DisplayItem),
    Extract(DisplayItem),
    Properties(DisplayItem),
}

// ─── Archive operations ───────────────────────────────────────────────────────

/// An operation on an archive that may need a password. It is kept while the
/// password prompt is open so it can be retried with the entered password.
#[derive(Clone, Debug, PartialEq)]
enum ArchiveOp {
    /// List the archive folder currently shown.
    List,
    /// Extract one file to the temp folder and open it.
    Preview { full_internal: String, file_name: String, open_with: bool },
    /// Extract one item into `dest`: a file without its path, a folder with it.
    ExtractItem { full_internal: String, name: String, is_dir: bool, dest: PathBuf },
    /// Extract the whole archive into `dest`.
    ExtractAll { dest: PathBuf },
    /// Decompress every entry and verify its checksum, writing nothing.
    Test,
}

/// What a finished background operation hands back to the UI thread.
#[derive(Debug)]
enum OpOutcome {
    /// `ArchiveOp::List`: every entry of the archive.
    Listed(Vec<ArchiveEntry>),
    /// `ArchiveOp::Preview`: the extracted file, to open on the UI thread.
    Previewed(PathBuf),
    /// Extraction or test finished, with any entries it skipped.
    Done(Vec<String>),
}

/// An archive operation running on a worker thread.
struct RunningOp {
    archive_path: PathBuf,
    op: ArchiveOp,
    /// The password it was started with, to tell a missing from a wrong one.
    password: Option<String>,
    started: Instant,
    /// Bytes done / total, and the cancel switch, shared with the worker.
    progress: Arc<Progress>,
    result: mpsc::Receiver<anyhow::Result<OpOutcome>>,
}

impl RunningOp {
    fn label(&self) -> String {
        op_label(&self.archive_path, &self.op)
    }
}

/// Short description of an operation, e.g. "Extracting archive.7z".
fn op_label(archive_path: &Path, op: &ArchiveOp) -> String {
    let archive = archive_path.file_name().unwrap_or_default().to_string_lossy();
    match op {
        ArchiveOp::List => format!("Opening {archive}"),
        ArchiveOp::Preview { file_name, .. } => format!("Opening {file_name}"),
        ArchiveOp::ExtractItem { name, .. } => format!("Extracting {name}"),
        ArchiveOp::ExtractAll { .. } => format!("Extracting {archive}"),
        ArchiveOp::Test => format!("Testing {archive}"),
    }
}

/// Appends skipped entries to a completion message, e.g.
/// "Extracted to out (2 entries skipped: skipped symbolic link a -> b; …)".
fn with_warnings(message: String, warnings: &[String]) -> String {
    match warnings {
        [] => message,
        [only] => format!("{message} (1 entry skipped: {only})"),
        [first, rest @ ..] => format!("{message} ({} entries skipped: {first}; …)", rest.len() + 1),
    }
}

/// Human-readable size: "512 B", "3.4 KB", "120.0 MB", "1.25 GB".
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let decimals = if unit >= 2 { 2 } else { 1 };
    format!("{value:.decimals$} {}", UNITS[unit])
}

/// State of the "password required" dialog.
#[derive(Debug)]
struct PasswordPrompt {
    archive_path: PathBuf,
    /// The operation to retry once a password is entered.
    op: ArchiveOp,
    input: String,
    /// The previous attempt used a password that did not work.
    incorrect: bool,
}

// ─── Pointer input ────────────────────────────────────────────────────────────

/// How far, in points, the pointer must move after a press before it is a
/// rubber-band drag rather than a click.
const RUBBER_BAND_THRESHOLD: f32 = 4.0;

/// One frame of pointer input, as rubber-band selection needs it.
#[derive(Clone, Copy, Debug, Default)]
struct PointerFrame {
    /// Where the primary button went down this frame, if it did.
    pressed_at: Option<egui::Pos2>,
    /// The primary button is held.
    primary_down: bool,
    /// Current pointer position.
    hover: Option<egui::Pos2>,
    /// Ctrl is held: add to the selection instead of replacing it.
    additive: bool,
}

// ─── App State ────────────────────────────────────────────────────────────────

pub struct OpenArchiveGui {
    current_location: NavLocation,
    history_back: Vec<NavLocation>,
    history_forward: Vec<NavLocation>,

    items: Vec<DisplayItem>,

    /// The set of currently selected row indices.
    selected_indices: HashSet<usize>,
    /// The "anchor" index used as the fixed end of a Shift-range.
    anchor_index: Option<usize>,
    /// The row the keyboard is on: the moving end of a Shift-range. Arrow
    /// keys move from here, Enter opens it.
    focus_index: Option<usize>,

    /// Where the primary button went down inside the list, while it is held.
    drag_select_start: Option<egui::Pos2>,
    /// Current pointer position while the button is held.
    drag_select_current: Option<egui::Pos2>,

    status_message: String,

    // Notification toast
    notification: Option<(String, std::time::Instant)>,

    // Properties modal window
    properties_item: Option<DisplayItem>,

    /// Passwords that opened each archive this session, so the user is asked
    /// once per archive. Kept in memory only.
    passwords: HashMap<PathBuf, String>,
    /// Open "password required" dialog, if any.
    password_prompt: Option<PasswordPrompt>,

    /// Archive operations running on worker threads (OA-5).
    running_ops: Vec<RunningOp>,
    /// The "operation in progress, quit anyway?" dialog is open.
    confirm_quit: bool,
    /// Quit once the cancelled operations have stopped.
    quit_when_idle: bool,
    /// Wakes the UI when a worker finishes; `None` without a window (tests).
    repaint: Option<egui::Context>,
}

// ─── Platform helpers ─────────────────────────────────────────────────────────

/// Opens `path` with its default program.
///
/// Never goes through `cmd.exe`: file names come from archives and may
/// contain `&`, `|` or `^`, which `cmd /c start` would run as commands.
fn open_file_externally(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        shell_open::open(path)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn().map(drop)
    }
}

/// Shows the system "Open with" dialog for `path`.
fn open_with_dialog(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // rundll32 is started directly (no shell), so the name is not parsed
        // for commands.
        std::process::Command::new("rundll32.exe")
            .arg("shell32.dll,OpenAs_RunDLL")
            .arg(path)
            .spawn()
            .map(drop)
    }
    #[cfg(not(target_os = "windows"))]
    {
        open_file_externally(path)
    }
}

/// `ShellExecuteExW`, the API behind `start`, called directly so the path
/// is passed as data rather than parsed as a command line.
#[cfg(target_os = "windows")]
mod shell_open {
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    const SEE_MASK_NOASYNC: u32 = 0x0000_0100;
    /// Report failures as errors instead of showing a message box.
    const SEE_MASK_FLAG_NO_UI: u32 = 0x0000_0400;
    const SW_SHOWNORMAL: i32 = 1;

    /// `SHELLEXECUTEINFOW` from shellapi.h.
    #[repr(C)]
    struct ShellExecuteInfoW {
        cb_size: u32,
        f_mask: u32,
        hwnd: *mut c_void,
        lp_verb: *const u16,
        lp_file: *const u16,
        lp_parameters: *const u16,
        lp_directory: *const u16,
        n_show: i32,
        h_inst_app: *mut c_void,
        lp_id_list: *mut c_void,
        lp_class: *const u16,
        hkey_class: *mut c_void,
        dw_hot_key: u32,
        h_icon_or_monitor: *mut c_void,
        h_process: *mut c_void,
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
    }

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub(super) fn open(path: &Path) -> io::Result<()> {
        let verb = wide("open".as_ref());
        let file = wide(path.as_os_str());
        let mut info = ShellExecuteInfoW {
            cb_size: std::mem::size_of::<ShellExecuteInfoW>() as u32,
            f_mask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            hwnd: std::ptr::null_mut(),
            lp_verb: verb.as_ptr(),
            lp_file: file.as_ptr(),
            lp_parameters: std::ptr::null(),
            lp_directory: std::ptr::null(),
            n_show: SW_SHOWNORMAL,
            h_inst_app: std::ptr::null_mut(),
            lp_id_list: std::ptr::null_mut(),
            lp_class: std::ptr::null(),
            hkey_class: std::ptr::null_mut(),
            dw_hot_key: 0,
            h_icon_or_monitor: std::ptr::null_mut(),
            h_process: std::ptr::null_mut(),
        };
        // SAFETY: `info` is a fully initialised SHELLEXECUTEINFOW whose string
        // pointers reference NUL-terminated buffers that outlive the call.
        if unsafe { ShellExecuteExW(&mut info) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn scratch(tag: &str) -> std::path::PathBuf {
            let dir = std::env::temp_dir().join(format!("openarchive-shell-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn struct_matches_the_windows_layout() {
            // sizeof(SHELLEXECUTEINFOW) on x64 / x86.
            let expected = if cfg!(target_pointer_width = "64") { 112 } else { 60 };
            assert_eq!(std::mem::size_of::<ShellExecuteInfoW>(), expected);
        }

        #[test]
        fn shell_metacharacters_in_names_are_not_executed() {
            // Under `cmd /c start`, everything after `&` ran as a command
            // (OA-25). The file does not exist, so nothing is opened either.
            let dir = scratch("inject");
            let marker = dir.join("PWNED_OA");
            let name = format!("x&md,\"{}\"", marker.display());
            let err = open(&dir.join(name)).unwrap_err();
            assert_eq!(err.raw_os_error(), Some(2), "{err}"); // ERROR_FILE_NOT_FOUND
            assert!(!marker.exists(), "shell metacharacters were executed");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

// ─── App impl ─────────────────────────────────────────────────────────────────

impl OpenArchiveGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let initial = NavLocation::Disk(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Self::with_location(initial, Some(cc.egui_ctx.clone()))
    }

    /// Builds the app state at `initial`. `repaint` wakes the window when a
    /// background operation finishes; tests pass `None`.
    fn with_location(initial: NavLocation, repaint: Option<egui::Context>) -> Self {
        let mut app = Self {
            current_location: initial.clone(),
            history_back: Vec::new(),
            history_forward: Vec::new(),
            items: Vec::new(),
            selected_indices: HashSet::new(),
            anchor_index: None,
            focus_index: None,
            drag_select_start: None,
            drag_select_current: None,
            status_message: "Ready".to_string(),
            notification: None,
            properties_item: None,
            passwords: HashMap::new(),
            password_prompt: None,
            running_ops: Vec::new(),
            confirm_quit: false,
            quit_when_idle: false,
            repaint,
        };
        app.refresh_items();
        app
    }

    pub fn open_archive_from_path(&mut self, path: PathBuf) {
        self.navigate_to(NavLocation::Archive {
            archive_path: path,
            internal_prefix: String::new(),
        });
    }

    fn navigate_to(&mut self, new_loc: NavLocation) {
        if self.current_location != new_loc {
            self.history_back.push(self.current_location.clone());
            self.history_forward.clear();
            self.current_location = new_loc;
            self.clear_selection();
            self.refresh_items();
        }
    }

    pub fn navigate_up_or_back(&mut self) {
        if let Some(prev) = self.history_back.pop() {
            self.history_forward.push(self.current_location.clone());
            self.current_location = prev;
            self.clear_selection();
            self.refresh_items();
            return;
        }
        self.navigate_up();
    }

    pub fn navigate_forward(&mut self) {
        if let Some(next) = self.history_forward.pop() {
            self.history_back.push(self.current_location.clone());
            self.current_location = next;
            self.clear_selection();
            self.refresh_items();
        }
    }

    pub fn navigate_up(&mut self) {
        match &self.current_location {
            NavLocation::Disk(p) => {
                if let Some(parent) = p.parent() {
                    let parent_buf = parent.to_path_buf();
                    self.history_back.push(self.current_location.clone());
                    self.current_location = NavLocation::Disk(parent_buf);
                    self.clear_selection();
                    self.refresh_items();
                    self.show_notification("Up one directory");
                }
            }
            NavLocation::Archive { archive_path, internal_prefix } => {
                let clean = internal_prefix.replace('\\', "/");
                let trimmed = clean.trim_matches('/');
                if let Some(last_slash) = trimmed.rfind('/') {
                    let new_prefix = format!("{}/", &trimmed[..last_slash]);
                    self.history_back.push(self.current_location.clone());
                    self.current_location = NavLocation::Archive {
                        archive_path: archive_path.clone(),
                        internal_prefix: new_prefix,
                    };
                } else if !trimmed.is_empty() {
                    self.history_back.push(self.current_location.clone());
                    self.current_location = NavLocation::Archive {
                        archive_path: archive_path.clone(),
                        internal_prefix: String::new(),
                    };
                } else {
                    let parent = archive_path.parent().unwrap_or(Path::new(".")).to_path_buf();
                    self.history_back.push(self.current_location.clone());
                    self.current_location = NavLocation::Disk(parent);
                }
                self.clear_selection();
                self.refresh_items();
                self.show_notification("Up one level");
            }
        }
    }

    // ─── Selection helpers ───────────────────────────────────────────────────

    fn clear_selection(&mut self) {
        self.selected_indices.clear();
        self.anchor_index = None;
        self.focus_index = None;
        self.drag_select_start = None;
        self.drag_select_current = None;
    }

    fn select_all(&mut self) {
        self.selected_indices = (0..self.items.len()).collect();
        self.anchor_index = Some(0);
        self.focus_index = Some(0);
        self.update_status();
    }

    /// Single-click: set exactly this row as selected (clears others). Moves
    /// anchor and focus to it.
    fn select_single(&mut self, idx: usize) {
        self.selected_indices.clear();
        self.selected_indices.insert(idx);
        self.anchor_index = Some(idx);
        self.focus_index = Some(idx);
        self.update_status();
    }

    /// Ctrl+click: toggle membership of this row, keeping the rest. Moves
    /// anchor and focus to it.
    fn select_toggle(&mut self, idx: usize) {
        if self.selected_indices.contains(&idx) {
            self.selected_indices.remove(&idx);
        } else {
            self.selected_indices.insert(idx);
        }
        self.anchor_index = Some(idx);
        self.focus_index = Some(idx);
        self.update_status();
    }

    /// Shift+click: select the range from the anchor to `idx` (inclusive).
    /// The anchor stays put; the focus moves to `idx`.
    fn select_range_to(&mut self, idx: usize) {
        let anchor = self.anchor_index.unwrap_or(idx);
        self.anchor_index = Some(anchor);
        self.focus_index = Some(idx);
        let lo = anchor.min(idx);
        let hi = anchor.max(idx);
        self.selected_indices.clear();
        for i in lo..=hi {
            self.selected_indices.insert(i);
        }
        self.update_status();
    }

    /// Arrow keys: move the focus one row down or up; with `extend` (Shift),
    /// select the range from the anchor instead of the single row. Does
    /// nothing in an empty list.
    fn move_focus(&mut self, down: bool, extend: bool) {
        let total = self.items.len();
        if total == 0 {
            return;
        }
        let current = self.focus_index.or(self.anchor_index).unwrap_or(0).min(total - 1);
        let target = if down { (current + 1).min(total - 1) } else { current.saturating_sub(1) };
        if self.anchor_index.is_some_and(|anchor| anchor >= total) {
            self.anchor_index = Some(current);
        }
        if extend {
            self.select_range_to(target);
        } else {
            self.select_single(target);
        }
    }

    /// Rubber-band drag: select all rows whose rects intersect the drag
    /// rectangle. With `additive` (Ctrl held), add them to the current
    /// selection instead of replacing it.
    fn apply_rubber_band(&mut self, drag_rect: Rect, row_rects: &[(usize, Rect)], additive: bool) {
        if !additive {
            self.selected_indices.clear();
        }
        for (idx, row_rect) in row_rects {
            if drag_rect.intersects(*row_rect) {
                self.selected_indices.insert(*idx);
            }
        }
        self.update_status();
    }

    /// Feeds one frame of pointer input to rubber-band selection.
    ///
    /// A press inside `list_area` arms the band; it only becomes a drag once
    /// the pointer moves [`RUBBER_BAND_THRESHOLD`] away. A plain click never
    /// becomes a band, so it cannot overwrite what the click (or Ctrl/Shift-
    /// click) handler selected in the same frame.
    fn update_rubber_band(&mut self, frame: PointerFrame, list_area: Rect, row_rects: &[(usize, Rect)]) {
        if let Some(pos) = frame.pressed_at {
            self.drag_select_start = list_area.contains(pos).then_some(pos);
            self.drag_select_current = None;
        }
        if frame.primary_down {
            if self.drag_select_start.is_some() {
                self.drag_select_current = frame.hover;
            }
            return;
        }
        if let Some(band) = self.rubber_band_rect() {
            self.apply_rubber_band(band, row_rects, frame.additive);
        }
        self.drag_select_start = None;
        self.drag_select_current = None;
    }

    /// The rubber band, once the pointer has moved far enough from the press
    /// to be a drag rather than a click.
    fn rubber_band_rect(&self) -> Option<Rect> {
        let (start, end) = (self.drag_select_start?, self.drag_select_current?);
        (start.distance(end) >= RUBBER_BAND_THRESHOLD).then(|| Rect::from_two_pos(start, end))
    }

    fn update_status(&mut self) {
        let n = self.selected_indices.len();
        if n == 0 {
            self.status_message = format!("{} items", self.items.len());
        } else {
            self.status_message = format!("{} of {} items selected", n, self.items.len());
        }
    }

    // ─── Misc helpers ────────────────────────────────────────────────────────

    fn show_notification(&mut self, text: impl Into<String>) {
        self.notification = Some((text.into(), std::time::Instant::now()));
    }

    fn refresh_items(&mut self) {
        self.items.clear();

        match &self.current_location {
            NavLocation::Disk(dir) => {
                let entries = match fs::read_dir(dir) {
                    Ok(entries) => entries,
                    Err(err) => {
                        // An unreadable folder must not look like an empty one.
                        let message = format!("Cannot read {}: {err}", dir.display());
                        self.status_message = message.clone();
                        self.show_notification(message);
                        return;
                    }
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    let is_dir = path.is_dir();
                    let is_archive = !is_dir && ArchiveFormat::from_path(&path).is_some();
                    let size = if is_dir { 0 } else { entry.metadata().map(|m| m.len()).unwrap_or(0) };
                    self.items.push(DisplayItem { name, is_dir, is_archive, size, compressed_size: None });
                }
                self.status_message = format!("{} items", self.items.len());
                self.sort_items();
            }
            NavLocation::Archive { archive_path, .. } => {
                let archive_path = archive_path.clone();
                self.run_archive_op(archive_path, ArchiveOp::List);
            }
        }
    }

    /// Folders first, then alphabetical.
    fn sort_items(&mut self) {
        self.items.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }

    // ─── Archive operations ──────────────────────────────────────────────────

    /// Starts `op` on `archive_path` on a worker thread, with the password
    /// remembered for that archive, so the window stays responsive. The
    /// result is applied by [`Self::poll_running_ops`], called every frame.
    fn run_archive_op(&mut self, archive_path: PathBuf, op: ArchiveOp) {
        let password = self.passwords.get(&archive_path).cloned();
        let (tx, rx) = mpsc::channel();
        let repaint = self.repaint.clone();
        let progress = Arc::new(Progress::new());
        let (job_path, job_op, job_password, job_progress) =
            (archive_path.clone(), op.clone(), password.clone(), progress.clone());
        std::thread::spawn(move || {
            let result = perform_archive_op(&job_path, &job_op, job_password, job_progress);
            // The receiver is gone only if the app closed; nothing to do then.
            let _ = tx.send(result);
            if let Some(ctx) = repaint {
                ctx.request_repaint();
            }
        });

        let running = RunningOp { archive_path, op, password, started: Instant::now(), progress, result: rx };
        if running.op == ArchiveOp::List {
            self.status_message = format!("{}…", running.label());
        }
        self.running_ops.push(running);
    }

    /// Applies the results of background operations that have finished.
    fn poll_running_ops(&mut self) {
        let mut finished = Vec::new();
        self.running_ops.retain(|running| {
            let result = match running.result.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return true,
                // The worker died without answering (it panicked).
                Err(TryRecvError::Disconnected) => Err(anyhow::anyhow!("the operation stopped unexpectedly")),
            };
            finished.push((running.archive_path.clone(), running.op.clone(), running.password.clone(), result));
            false
        });
        for (archive_path, op, password, result) in finished {
            self.finish_archive_op(archive_path, op, password, result);
        }
    }

    /// Test helper: applies background results until none are running.
    #[cfg(test)]
    fn wait_idle(&mut self) {
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        while !self.running_ops.is_empty() {
            assert!(Instant::now() < deadline, "background operation did not finish");
            std::thread::sleep(std::time::Duration::from_millis(2));
            self.poll_running_ops();
        }
    }

    /// Whether an extraction or test is running on `archive_path`.
    fn archive_busy(&self, archive_path: &Path) -> bool {
        self.running_ops
            .iter()
            .any(|running| running.op != ArchiveOp::List && running.archive_path == archive_path)
    }

    /// Status-bar text for the operations in progress, if any, e.g.
    /// "Extracting a.7z… 42% (120.0 MB of 285.4 MB) 12s (+1 more)".
    fn busy_label(&self) -> Option<String> {
        let first = self.running_ops.first()?;
        let progress = &first.progress;
        let amount = match (progress.fraction(), progress.total()) {
            (Some(fraction), Some(total)) => format!(
                " {:.0}% ({} of {})",
                fraction * 100.0,
                format_bytes(progress.done()),
                format_bytes(total)
            ),
            _ if progress.done() > 0 => format!(" {}", format_bytes(progress.done())),
            _ => String::new(),
        };
        let mut label = format!("{}…{amount} {}s", first.label(), first.started.elapsed().as_secs());
        if first.progress.is_cancelled() {
            label.push_str(" (cancelling)");
        }
        if self.running_ops.len() > 1 {
            label.push_str(&format!(" (+{} more)", self.running_ops.len() - 1));
        }
        Some(label)
    }

    /// Completed fraction of the operation shown in the status bar, if known.
    fn busy_fraction(&self) -> Option<f32> {
        self.running_ops.first()?.progress.fraction()
    }

    /// Asks every running operation to stop. Each removes the file it was
    /// writing and reports "Cancelled".
    fn cancel_running_ops(&mut self) {
        for running in &self.running_ops {
            running.progress.cancel();
        }
    }

    /// Work that quitting would interrupt: anything but a listing.
    fn has_work_in_progress(&self) -> bool {
        self.running_ops.iter().any(|running| running.op != ArchiveOp::List)
    }

    /// Applies the result of a finished operation on the UI thread.
    ///
    /// If the archive needs a password, or the remembered one is wrong, the
    /// password dialog is opened and `op` is retried when the user submits it.
    /// Other failures are reported in a notification.
    fn finish_archive_op(
        &mut self,
        archive_path: PathBuf,
        op: ArchiveOp,
        password: Option<String>,
        result: anyhow::Result<OpOutcome>,
    ) {
        let archive_name = archive_path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let err = match result {
            Ok(outcome) => {
                self.apply_outcome(&archive_path, &op, outcome);
                return;
            }
            Err(err) => err,
        };

        // Checked first: sevenz-rust2 reports a cancelled encrypted read as
        // MaybeBadPassword, and that must not open the password dialog.
        if is_cancelled(&err) {
            if op == ArchiveOp::List && self.shows_archive(&archive_path) {
                self.status_message = "Cancelled".to_string();
            }
            self.show_notification(format!("Cancelled: {}", op_label(&archive_path, &op)));
            return;
        }

        if password_problem(&err).is_some() {
            self.passwords.remove(&archive_path);
            if self.password_prompt.is_some() {
                // One dialog at a time: report this one, the user can retry.
                self.show_notification(format!("Password required for {archive_name}; try again"));
                return;
            }
            self.status_message = format!("Password required: {archive_name}");
            self.password_prompt = Some(PasswordPrompt {
                archive_path,
                op,
                input: String::new(),
                incorrect: password.is_some(),
            });
            return;
        }

        let message = match &op {
            ArchiveOp::List => format!("Failed to open archive: {err:#}"),
            ArchiveOp::Preview { file_name, .. } => format!("Failed to open {file_name}: {err:#}"),
            ArchiveOp::ExtractItem { name, .. } => format!("Failed to extract {name}: {err:#}"),
            ArchiveOp::ExtractAll { .. } => format!("Extraction failed: {err:#}"),
            ArchiveOp::Test => format!("Test failed for {archive_name}: {err:#}"),
        };
        if op == ArchiveOp::List && self.shows_archive(&archive_path) {
            self.status_message = message.clone();
        }
        self.show_notification(message);
    }

    /// Whether the archive at `archive_path` is the one currently shown.
    fn shows_archive(&self, archive_path: &Path) -> bool {
        matches!(&self.current_location, NavLocation::Archive { archive_path: current, .. } if current == archive_path)
    }

    fn apply_outcome(&mut self, archive_path: &Path, op: &ArchiveOp, outcome: OpOutcome) {
        let archive_name = archive_path.file_name().unwrap_or_default().to_string_lossy().to_string();
        match (op, outcome) {
            (ArchiveOp::List, OpOutcome::Listed(entries)) => {
                // The user may have navigated away while it was listing.
                let NavLocation::Archive { archive_path: current, internal_prefix } = &self.current_location
                else {
                    return;
                };
                if current != archive_path {
                    return;
                }
                self.items = filter_archive_entries(&entries, internal_prefix);
                self.sort_items();
                self.status_message = format!("Archive: {archive_name} [{} items]", self.items.len());
            }
            (ArchiveOp::Preview { file_name, open_with, .. }, OpOutcome::Previewed(target_file)) => {
                let opened = if *open_with {
                    open_with_dialog(&target_file)
                } else {
                    open_file_externally(&target_file)
                };
                match opened {
                    Ok(()) => self.show_notification(format!("Opened {file_name}")),
                    Err(err) => self.show_notification(format!("Failed to open {file_name}: {err}")),
                }
            }
            (ArchiveOp::ExtractItem { name, dest, .. }, OpOutcome::Done(warnings)) => {
                let message = format!("Extracted {} to {}", name, dest.display());
                self.show_notification(with_warnings(message, &warnings));
            }
            (ArchiveOp::ExtractAll { dest }, OpOutcome::Done(warnings)) => {
                let message = format!("Extracted to {}", dest.display());
                self.show_notification(with_warnings(message, &warnings));
            }
            (ArchiveOp::Test, OpOutcome::Done(warnings)) => {
                let message = format!("{archive_name}: Everything is Ok");
                self.show_notification(with_warnings(message, &warnings));
            }
            (op, outcome) => unreachable!("{op:?} cannot produce {outcome:?}"),
        }
    }

    /// Retries the pending operation with the password typed in the dialog.
    fn submit_password(&mut self) {
        let Some(prompt) = self.password_prompt.take() else {
            return;
        };
        self.passwords.insert(prompt.archive_path.clone(), prompt.input);
        self.run_archive_op(prompt.archive_path, prompt.op);
    }

    fn show_quit_confirmation(&mut self, ctx: &egui::Context) {
        if !self.confirm_quit {
            return;
        }
        let (mut stop_and_quit, mut keep_working) = (false, false);
        let busy = self.busy_label().unwrap_or_default();
        egui::Window::new("Operation in progress")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(340.0);
                ui.label(busy);
                ui.label("Quitting stops it. The file being written is removed; files already extracted are kept.");
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        keep_working = ui.button("Keep working").clicked();
                        stop_and_quit = ui.button("Stop and quit").clicked();
                    });
                });
            });
        if stop_and_quit {
            self.confirm_quit = false;
            self.quit_when_idle = true;
            self.cancel_running_ops();
        } else if keep_working || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.confirm_quit = false;
        }
    }

    fn show_password_prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.password_prompt.as_mut() else {
            return;
        };

        let mut submit = false;
        let mut cancel = ctx.input(|i| i.key_pressed(Key::Escape));
        let mut is_open = true;
        let archive_name = prompt
            .archive_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        egui::Window::new("Password required")
            .open(&mut is_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(320.0);
                ui.label(format!("Enter the password for {archive_name}:"));
                if prompt.incorrect {
                    ui.label(RichText::new("Wrong password, try again.").color(Color32::from_rgb(220, 80, 80)));
                }
                ui.add_space(4.0);
                let field = ui.add(
                    egui::TextEdit::singleline(&mut prompt.input)
                        .password(true)
                        .desired_width(f32::INFINITY),
                );
                if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    submit = true;
                } else if !field.has_focus() {
                    field.request_focus();
                }
                ui.add_space(4.0);
                // A horizontal row keeps the buttons one line tall; a bare
                // right-to-left layout would take all remaining height.
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        if ui.button("  OK  ").clicked() {
                            submit = true;
                        }
                    });
                });
            });

        if submit {
            self.submit_password();
        } else if cancel || !is_open {
            self.cancel_password_prompt();
        }
    }

    fn cancel_password_prompt(&mut self) {
        if let Some(prompt) = self.password_prompt.take()
            && prompt.op == ArchiveOp::List
        {
            self.status_message = "Archive is encrypted; password not entered".to_string();
        }
    }

    fn current_display_path(&self) -> String {
        match &self.current_location {
            NavLocation::Disk(p) => p.to_string_lossy().to_string(),
            NavLocation::Archive { archive_path, internal_prefix } => {
                let arc_str = archive_path.to_string_lossy().to_string();
                let clean_prefix = internal_prefix.replace('/', "\\");
                if clean_prefix.is_empty() {
                    format!("{}\\ (Root)", arc_str)
                } else {
                    format!("{}\\{}", arc_str, clean_prefix)
                }
            }
        }
    }

    fn open_item(&mut self, item: DisplayItem) {
        match &self.current_location {
            NavLocation::Disk(dir) => {
                let target = dir.join(&item.name);
                if item.is_dir {
                    self.navigate_to(NavLocation::Disk(target));
                } else if item.is_archive {
                    self.navigate_to(NavLocation::Archive {
                        archive_path: target,
                        internal_prefix: String::new(),
                    });
                } else {
                    match open_file_externally(&target) {
                        Ok(()) => self.show_notification(format!("Opening {}", item.name)),
                        Err(err) => self.show_notification(format!("Failed to open {}: {err}", item.name)),
                    }
                }
            }
            NavLocation::Archive { archive_path, internal_prefix } => {
                if item.is_dir {
                    let clean_prefix = internal_prefix.replace('\\', "/");
                    let clean_prefix = clean_prefix.trim_matches('/');
                    let item_clean = item.name.trim_matches('/');
                    let next_prefix = if clean_prefix.is_empty() {
                        format!("{}/", item_clean)
                    } else {
                        format!("{}/{}/", clean_prefix, item_clean)
                    };
                    let arc = archive_path.clone();
                    self.navigate_to(NavLocation::Archive {
                        archive_path: arc,
                        internal_prefix: next_prefix,
                    });
                } else {
                    let (arc, prefix) = (archive_path.clone(), internal_prefix.clone());
                    self.preview_archive_item(arc, &prefix, &item, false);
                }
            }
        }
    }

    /// Opens an archive file in an external program after extracting it to
    /// the temp folder.
    fn preview_archive_item(&mut self, archive_path: PathBuf, internal_prefix: &str, item: &DisplayItem, open_with: bool) {
        let op = ArchiveOp::Preview {
            full_internal: archive_item_path(internal_prefix, &item.name),
            file_name: item.name.clone(),
            open_with,
        };
        self.run_archive_op(archive_path, op);
    }

    fn handle_context_action(&mut self, action: ContextAction) {
        match action {
            ContextAction::Open(item) => { self.open_item(item); }
            ContextAction::OpenWith(item) => {
                match &self.current_location {
                    NavLocation::Disk(dir) => {
                        if let Err(err) = open_with_dialog(&dir.join(&item.name)) {
                            self.show_notification(format!("Failed to open {}: {err}", item.name));
                        }
                    }
                    NavLocation::Archive { archive_path, internal_prefix } => {
                        let (arc, prefix) = (archive_path.clone(), internal_prefix.clone());
                        self.preview_archive_item(arc, &prefix, &item, true);
                    }
                }
            }
            ContextAction::Extract(item) => {
                match &self.current_location {
                    NavLocation::Disk(dir) => {
                        let source = dir.join(&item.name);
                        if let Some(dest) = rfd::FileDialog::new().pick_folder() {
                            match copy_to_folder(&source, &dest) {
                                Ok(files) => self.show_notification(format!(
                                    "Copied {} ({files} file{}) to {}",
                                    item.name,
                                    if files == 1 { "" } else { "s" },
                                    dest.display()
                                )),
                                Err(err) => self.show_notification(format!("Failed to copy {}: {err}", item.name)),
                            }
                        }
                    }
                    NavLocation::Archive { archive_path, internal_prefix } => {
                        let (arc, prefix) = (archive_path.clone(), internal_prefix.clone());
                        if let Some(dest) = rfd::FileDialog::new().pick_folder() {
                            let op = ArchiveOp::ExtractItem {
                                full_internal: archive_item_path(&prefix, &item.name),
                                name: item.name.clone(),
                                is_dir: item.is_dir,
                                dest,
                            };
                            self.run_archive_op(arc, op);
                        }
                    }
                }
            }
            ContextAction::Properties(item) => { self.properties_item = Some(item); }
        }
    }

    fn extract_current(&mut self) {
        if let NavLocation::Archive { archive_path, .. } = &self.current_location {
            let arc = archive_path.clone();
            if let Some(dest) = rfd::FileDialog::new().pick_folder() {
                self.run_archive_op(arc, ArchiveOp::ExtractAll { dest });
            }
        }
    }

    /// Tests the integrity of the archive currently shown.
    fn test_current(&mut self) {
        if let NavLocation::Archive { archive_path, .. } = &self.current_location {
            let arc = archive_path.clone();
            self.run_archive_op(arc, ArchiveOp::Test);
        }
    }
}

/// Copies a file or a whole folder from disk into `dest_dir`, keeping its
/// name, and returns how many files were copied. Existing files are
/// overwritten.
///
/// Refuses to copy something onto itself (on Unix `fs::copy` would truncate
/// the file) or a folder into itself (the copy would keep growing the tree
/// being walked).
fn copy_to_folder(source: &Path, dest_dir: &Path) -> std::io::Result<usize> {
    use std::io::{Error, ErrorKind};

    let name = source
        .file_name()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "source has no file name"))?;
    let source = fs::canonicalize(source)?;
    fs::create_dir_all(dest_dir)?;
    let dest_dir = fs::canonicalize(dest_dir)?;
    let target = dest_dir.join(name);

    if target == source {
        return Err(Error::new(ErrorKind::InvalidInput, "source and destination are the same"));
    }

    if !source.is_dir() {
        fs::copy(&source, &target)?;
        return Ok(1);
    }

    if dest_dir.starts_with(&source) {
        return Err(Error::new(ErrorKind::InvalidInput, "cannot copy a folder into itself"));
    }

    let mut files = 0;
    for entry in walkdir::WalkDir::new(&source) {
        let entry = entry.map_err(Error::other)?;
        let relative = entry.path().strip_prefix(&source).map_err(Error::other)?;
        let out = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&out)?;
        } else if entry.path().is_file() {
            // Includes symlinks to files: their content is copied.
            fs::copy(entry.path(), &out)?;
            files += 1;
        }
    }
    Ok(files)
}

/// Does the slow part of `op` on a worker thread. Touches no app state:
/// everything the UI needs comes back in the [`OpOutcome`].
fn perform_archive_op(
    archive_path: &Path,
    op: &ArchiveOp,
    password: Option<String>,
    progress: Arc<Progress>,
) -> anyhow::Result<OpOutcome> {
    let mut reader = get_reader(archive_path, password.clone())?;
    let extract_options = |output_dir: PathBuf, flat: bool| ExtractOptions {
        output_dir,
        flat,
        password: password.clone(),
        overwrite: true,
        threads: None,
        progress: Some(progress.clone()),
    };

    match op {
        ArchiveOp::List => Ok(OpOutcome::Listed(reader.list()?)),
        ArchiveOp::Preview { full_internal, file_name, .. } => {
            let arc_stem = archive_path.file_stem().unwrap_or_default().to_string_lossy();
            let temp_dir = std::env::temp_dir().join("OpenArchive").join(arc_stem.as_ref());
            fs::create_dir_all(&temp_dir)?;
            let filter = [full_internal.clone()];
            reader.extract(&extract_options(temp_dir.clone(), true), Some(&filter))?;

            let target_file = temp_dir.join(file_name);
            if !target_file.exists() {
                anyhow::bail!("{full_internal} was not found in the archive");
            }
            Ok(OpOutcome::Previewed(target_file))
        }
        ArchiveOp::ExtractItem { full_internal, is_dir, dest, .. } => {
            let filter = [full_internal.clone()];
            reader.extract(&extract_options(dest.clone(), !is_dir), Some(&filter))?;
            Ok(OpOutcome::Done(progress.warnings()))
        }
        ArchiveOp::ExtractAll { dest } => {
            reader.extract(&extract_options(dest.clone(), false), None)?;
            Ok(OpOutcome::Done(progress.warnings()))
        }
        ArchiveOp::Test => {
            reader.test(password.as_deref(), Some(&progress))?;
            Ok(OpOutcome::Done(progress.warnings()))
        }
    }
}

/// Full path of `name` inside an archive, given the folder shown in the GUI.
fn archive_item_path(internal_prefix: &str, name: &str) -> String {
    format!("{}{}", internal_prefix.replace('\\', "/"), name)
}

// ─── Archive entry filter ─────────────────────────────────────────────────────

fn filter_archive_entries(all: &[ArchiveEntry], prefix: &str) -> Vec<DisplayItem> {
    use std::collections::HashSet;
    let mut seen_dirs = HashSet::new();
    let mut items = Vec::new();

    let clean_prefix = prefix.replace('\\', "/");
    let clean_prefix = clean_prefix.trim_start_matches('/');

    for entry in all {
        let raw_name = entry.name.replace('\\', "/");
        let name = raw_name.trim_start_matches('/');

        if let Some(rel) = name.strip_prefix(clean_prefix) {
            let rel = rel.trim_start_matches('/');
            if rel.is_empty() { continue; }

            if let Some(slash_idx) = rel.find('/') {
                let dir_name = &rel[..slash_idx];
                if seen_dirs.insert(dir_name.to_string()) {
                    items.push(DisplayItem {
                        name: dir_name.to_string(),
                        is_dir: true,
                        is_archive: false,
                        size: 0,
                        compressed_size: None,
                    });
                }
            } else {
                // 7z stores folders as entries without a trailing slash; the
                // folder may already be listed because of a file inside it.
                if entry.is_dir && !seen_dirs.insert(rel.to_string()) {
                    continue;
                }
                items.push(DisplayItem {
                    name: rel.to_string(),
                    is_dir: entry.is_dir,
                    is_archive: false,
                    size: entry.size,
                    compressed_size: entry.compressed_size,
                });
            }
        }
    }
    items
}

// ─── eframe::App impl ────────────────────────────────────────────────────────

impl eframe::App for OpenArchiveGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut to_open: Option<DisplayItem> = None;
        let mut action_to_take: Option<ContextAction> = None;

        // ── 0. BACKGROUND OPERATIONS ─────────────────────────────────────────
        self.poll_running_ops();
        if !self.running_ops.is_empty() {
            // Keep progress and elapsed time ticking; workers also wake the UI
            // when done.
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        // Quitting mid-extraction would leave a half-written file: ask first,
        // and when the user confirms, cancel and close once workers stopped.
        if ctx.input(|i| i.viewport().close_requested()) && self.has_work_in_progress() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.quit_when_idle {
                self.confirm_quit = true;
            }
        }
        if self.quit_when_idle && self.running_ops.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.show_quit_confirmation(ctx);

        // Grab modifier state for use in item rows below
        let (ctrl_down, shift_down) = ctx.input(|i| (i.modifiers.ctrl, i.modifiers.shift));

        // ── 1. GLOBAL KEYBOARD & MOUSE BUTTON HANDLERS ───────────────────────
        // Disabled while the password dialog has the keyboard: Backspace and
        // Enter there must not navigate or open items.
        let shortcuts_enabled = self.password_prompt.is_none();
        ctx.input(|i| {
            if !shortcuts_enabled { return; }
            if i.pointer.button_clicked(PointerButton::Extra1) { self.navigate_up_or_back(); }
            if i.pointer.button_clicked(PointerButton::Extra2) { self.navigate_forward(); }

            if i.key_pressed(Key::Backspace) || (i.key_pressed(Key::ArrowUp) && i.modifiers.alt) {
                self.navigate_up_or_back();
            }

            // Ctrl+A — select all
            if i.key_pressed(Key::A) && i.modifiers.ctrl {
                self.select_all();
            }

            // F5 — refresh
            if i.key_pressed(Key::F5) {
                self.refresh_items();
            }

            // Arrow-key navigation
            if i.key_pressed(Key::ArrowDown) && !i.modifiers.alt {
                self.move_focus(true, i.modifiers.shift);
            }
            if i.key_pressed(Key::ArrowUp) && !i.modifiers.alt {
                self.move_focus(false, i.modifiers.shift);
            }

            // Enter opens the focused item
            if i.key_pressed(Key::Enter)
                && let Some(item) = self.focus_index.or(self.anchor_index).and_then(|idx| self.items.get(idx))
            {
                to_open = Some(item.clone());
            }

            // Escape clears selection
            if i.key_pressed(Key::Escape) {
                self.clear_selection();
            }
        });

        // ── 2. TOP TOOLBAR & ADDRESS BAR ─────────────────────────────────────
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Open Archive...").clicked() {
                        if let Some(file) = rfd::FileDialog::new()
                            .add_filter("Archives", &["7z", "zip", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz"])
                            .pick_file()
                        {
                            self.open_archive_from_path(file);
                        }
                        ui.close_menu();
                    }
                    if ui.button("Exit").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Edit", |ui| {
                    if ui.button("Select All  Ctrl+A").clicked() {
                        self.select_all();
                        ui.close_menu();
                    }
                    if ui.button("Deselect All  Esc").clicked() {
                        self.clear_selection();
                        ui.close_menu();
                    }
                });
                ui.menu_button("View", |ui| {
                    if ui.button("Refresh  F5").clicked() {
                        self.refresh_items();
                        ui.close_menu();
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About OpenArchive").clicked() {
                        self.show_notification("OpenArchive 0.1.0 — Modern Rust Archive Manager");
                        ui.close_menu();
                    }
                });
            });

            ui.add_space(4.0);

            // Action toolbar
            ui.horizontal(|ui| {
                if ui.button(RichText::new("➕ Add").size(14.0)).clicked() {
                    self.show_notification("Select files to add into an archive");
                }

                // Not while this archive is already being extracted or tested.
                let can_extract = match &self.current_location {
                    NavLocation::Archive { archive_path, .. } => !self.archive_busy(archive_path),
                    NavLocation::Disk(_) => false,
                };
                if ui.add_enabled(can_extract, egui::Button::new(RichText::new("📤 Extract").size(14.0))).clicked() {
                    self.extract_current();
                }
                if ui.add_enabled(can_extract, egui::Button::new(RichText::new("✔ Test").size(14.0))).clicked() {
                    self.test_current();
                }

                ui.separator();

                if ui.button(RichText::new("📂 Open Archive").size(14.0)).clicked() {
                    if let Some(file) = rfd::FileDialog::new()
                        .add_filter("Supported Archives (*.7z, *.zip, *.tar, *.tar.gz, *.tar.bz2, *.tar.xz)", &["7z", "zip", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz"])
                        .pick_file()
                    {
                        self.open_archive_from_path(file);
                    }
                }
            });

            ui.add_space(6.0);

            // Address bar
            ui.horizontal(|ui| {
                let up_btn = ui.button(RichText::new(" ⬆ ").size(16.0).color(Color32::from_rgb(0, 160, 80)));
                if up_btn.on_hover_text("Up One Level (or Mouse Back / Backspace)").clicked() {
                    self.navigate_up_or_back();
                }
                let mut path_display = self.current_display_path();
                ui.add(
                    egui::TextEdit::singleline(&mut path_display)
                        .desired_width(ui.available_width() - 80.0)
                        .interactive(false),
                );
                if ui.button("Browse...").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.navigate_to(NavLocation::Disk(dir));
                    }
                }
            });

            // Notification toast
            if let Some((msg, time)) = &self.notification {
                if time.elapsed().as_secs() < 3 {
                    ui.label(RichText::new(format!("ℹ {}", msg)).color(Color32::from_rgb(40, 180, 240)));
                }
            }

            ui.add_space(4.0);
        });

        // ── 3. BOTTOM STATUS BAR ──────────────────────────────────────────────
        egui::TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(busy) = self.busy_label() {
                    match self.busy_fraction() {
                        Some(fraction) => {
                            ui.add(egui::ProgressBar::new(fraction).desired_width(140.0).desired_height(12.0));
                        }
                        None => {
                            ui.add(egui::Spinner::new().size(12.0));
                        }
                    }
                    ui.label(RichText::new(busy).size(12.0).color(Color32::from_rgb(40, 180, 240)));
                    if ui.small_button("Cancel").on_hover_text("Stop the running operations").clicked() {
                        self.cancel_running_ops();
                    }
                    ui.separator();
                }
                ui.label(RichText::new(&self.status_message).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new("Double-click: Open | Right-click: Menu | Ctrl+A: Select All | Mouse Back: Up")
                            .color(Color32::GRAY)
                            .size(11.0),
                    );
                });
            });
        });

        // ── 4. CENTRAL FILE LIST ──────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            // We need to track row rects for rubber-band hit testing
            let mut row_rects: Vec<(usize, Rect)> = Vec::new();

            egui::ScrollArea::vertical().show(ui, |ui| {
                let available_w = ui.available_width();
                ui.set_min_width(available_w);

                // Header
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Name").strong().size(13.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("Size").strong().size(13.0));
                    });
                });
                ui.separator();

                // ".." Parent folder row
                let (parent_rect, parent_resp) =
                    ui.allocate_exact_size(Vec2::new(available_w, 24.0), Sense::click());
                if parent_resp.hovered() {
                    ui.painter().rect_filled(parent_rect, 2.0, Color32::from_rgba_unmultiplied(100, 150, 255, 30));
                }
                ui.painter().text(
                    parent_rect.left_top() + Vec2::new(6.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    "📁 .. (Parent Folder)",
                    FontId::proportional(13.0),
                    Color32::LIGHT_BLUE,
                );
                if parent_resp.double_clicked() {
                    self.navigate_up_or_back();
                }

                // ── Item rows ─────────────────────────────────────────────────
                // We collect which row (if any) was interacted with before updating
                // state so we have a stable items slice.
                let mut click_info: Option<(usize, bool /* primary */, bool /* double */, bool /* secondary */)> = None;

                for (idx, item) in self.items.iter().enumerate() {
                    let is_selected = self.selected_indices.contains(&idx);
                    let (row_rect, row_resp) =
                        ui.allocate_exact_size(Vec2::new(available_w, 24.0), Sense::click_and_drag());

                    row_rects.push((idx, row_rect));

                    // Visual highlight
                    if is_selected {
                        ui.painter().rect_filled(row_rect, 2.0, Color32::from_rgba_unmultiplied(60, 120, 220, 80));
                    } else if row_resp.hovered() {
                        ui.painter().rect_filled(row_rect, 2.0, Color32::from_rgba_unmultiplied(200, 200, 200, 25));
                    }

                    // Collect click info (we can't mutate self here because items is borrowed)
                    if row_resp.double_clicked() {
                        click_info = Some((idx, true, true, false));
                    } else if row_resp.secondary_clicked() {
                        click_info = Some((idx, false, false, true));
                    } else if row_resp.clicked() {
                        click_info = Some((idx, true, false, false));
                    }

                    // Right-click context menu
                    row_resp.context_menu(|ui| {
                        ui.set_min_width(170.0);
                        let open_label = if item.is_dir {
                            "📂 Open Folder"
                        } else if item.is_archive {
                            "📦 Open Archive"
                        } else {
                            "📄 Open"
                        };
                        if ui.button(open_label).clicked() {
                            action_to_take = Some(ContextAction::Open(item.clone()));
                            ui.close_menu();
                        }
                        if !item.is_dir && !item.is_archive {
                            if ui.button("⚙ Open With...").clicked() {
                                action_to_take = Some(ContextAction::OpenWith(item.clone()));
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        let extract_label = if item.is_dir { "📤 Extract Folder..." } else { "📤 Extract File..." };
                        if ui.button(extract_label).clicked() {
                            action_to_take = Some(ContextAction::Extract(item.clone()));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("ℹ Properties").clicked() {
                            action_to_take = Some(ContextAction::Properties(item.clone()));
                            ui.close_menu();
                        }
                    });

                    let icon = if item.is_dir { "📁" } else if item.is_archive { "📦" } else { "📄" };
                    let size_text = if item.is_dir {
                        String::new()
                    } else if item.size < 1024 {
                        format!("{} B", item.size)
                    } else if item.size < 1024 * 1024 {
                        format!("{:.1} KB", item.size as f64 / 1024.0)
                    } else {
                        format!("{:.1} MB", item.size as f64 / (1024.0 * 1024.0))
                    };
                    let text_color = if is_selected { Color32::WHITE } else { ui.visuals().text_color() };

                    ui.painter().text(
                        row_rect.left_top() + Vec2::new(6.0, 4.0),
                        egui::Align2::LEFT_TOP,
                        format!("{} {}", icon, item.name),
                        FontId::proportional(13.0),
                        text_color,
                    );
                    if !size_text.is_empty() {
                        ui.painter().text(
                            row_rect.right_top() + Vec2::new(-8.0, 4.0),
                            egui::Align2::RIGHT_TOP,
                            size_text,
                            FontId::proportional(12.0),
                            Color32::GRAY,
                        );
                    }
                }

                // ── Process click interactions ─────────────────────────────────
                if let Some((idx, is_primary, is_double, _is_secondary)) = click_info {
                    if is_double {
                        // Always open on double-click (regardless of modifiers)
                        if let Some(item) = self.items.get(idx) {
                            to_open = Some(item.clone());
                        }
                    } else if is_primary {
                        if ctrl_down {
                            self.select_toggle(idx);
                        } else if shift_down {
                            self.select_range_to(idx);
                        } else {
                            self.select_single(idx);
                        }
                    } else {
                        // Right-click: select the row if it wasn't already selected
                        if !self.selected_indices.contains(&idx) {
                            self.select_single(idx);
                        }
                    }
                }

                // ── Rubber-band drag selection ─────────────────────────────────
                // Presses in an open dialog must not start a band behind it.
                let dialog_open = self.password_prompt.is_some() || self.properties_item.is_some();
                let frame = ctx.input(|i| PointerFrame {
                    pressed_at: if i.pointer.primary_pressed() && !dialog_open {
                        i.pointer.press_origin()
                    } else {
                        None
                    },
                    primary_down: i.pointer.primary_down(),
                    hover: i.pointer.hover_pos(),
                    additive: i.modifiers.ctrl,
                });
                // The visible part of the list: presses on the toolbar or
                // address bar do not start a band.
                let list_area = ui.clip_rect();
                self.update_rubber_band(frame, list_area, &row_rects);

                // Draw the band once it is a drag, not a click.
                if let Some(band) = self.rubber_band_rect() {
                    ui.painter().rect(
                        band,
                        0.0,
                        Color32::from_rgba_unmultiplied(60, 120, 220, 25),
                        egui::Stroke::new(1.0_f32, Color32::from_rgb(60, 120, 220)),
                    );
                }

                // Apply deferred open / action
                if let Some(item) = to_open {
                    self.open_item(item);
                }
                if let Some(act) = action_to_take {
                    self.handle_context_action(act);
                }
            });
        });

        // ── 5. PROPERTIES MODAL ───────────────────────────────────────────────
        if let Some(prop_item) = self.properties_item.clone() {
            let mut is_open = true;
            egui::Window::new("Properties")
                .open(&mut is_open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(320.0);
                    ui.horizontal(|ui| {
                        let icon = if prop_item.is_dir { "📁" } else if prop_item.is_archive { "📦" } else { "📄" };
                        ui.label(RichText::new(icon).size(28.0));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&prop_item.name).strong().size(15.0));
                            let kind_str = if prop_item.is_dir {
                                "File Folder"
                            } else if prop_item.is_archive {
                                "Compressed Archive"
                            } else {
                                "File"
                            };
                            ui.label(RichText::new(kind_str).color(Color32::GRAY));
                        });
                    });
                    ui.separator();
                    egui::Grid::new("properties_grid")
                        .num_columns(2)
                        .spacing([16.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(RichText::new("Size:").strong());
                            ui.label(format!("{} bytes", prop_item.size));
                            ui.end_row();
                            if let Some(cs) = prop_item.compressed_size {
                                ui.label(RichText::new("Packed Size:").strong());
                                ui.label(format!("{} bytes", cs));
                                ui.end_row();
                            }
                            ui.label(RichText::new("Location:").strong());
                            ui.label(self.current_display_path());
                            ui.end_row();
                        });
                    ui.separator();
                    ui.add_space(4.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("  OK  ").clicked() {
                            self.properties_item = None;
                        }
                    });
                });
            if !is_open {
                self.properties_item = None;
            }
        }

        // ── 6. PASSWORD DIALOG ────────────────────────────────────────────────
        self.show_password_prompt(ctx);
    }
}

// ─── App icon loading ─────────────────────────────────────────────────────────

fn load_app_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../assets/OpenArchive_favicon.ico");
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(egui::IconData { rgba: rgba.into_raw(), width, height })
}

pub fn run_gui(initial_archive: Option<PathBuf>) -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(Vec2::new(850.0, 550.0))
        .with_min_inner_size(Vec2::new(500.0, 350.0))
        .with_title("OpenArchive File Manager");

    if let Some(icon) = load_app_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        "OpenArchive",
        options,
        Box::new(|cc| {
            let mut app = OpenArchiveGui::new(cc);
            if let Some(p) = initial_archive {
                app.open_archive_from_path(p);
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::CompressionOptions;
    use crate::formats::create_archive;

    const PAYLOAD: &[u8] = b"top secret";

    /// Creates `<tmp>/<tag>/archive.7z` holding `src/a.txt`, encrypted when
    /// `password` is set.
    fn make_archive(tag: &str, password: Option<&str>) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("openarchive-gui-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.txt"), PAYLOAD).unwrap();
        let archive = dir.join("archive.7z");
        let options = CompressionOptions { password: password.map(String::from), ..Default::default() };
        create_archive(&archive, &[dir.join("src")], &options).unwrap();
        (dir, archive)
    }

    /// Opens the GUI on `archive` and waits for the listing to finish.
    fn open_at(archive: &Path) -> OpenArchiveGui {
        let mut app = OpenArchiveGui::with_location(
            NavLocation::Archive { archive_path: archive.to_path_buf(), internal_prefix: String::new() },
            None,
        );
        app.wait_idle();
        app
    }

    fn item_names(app: &OpenArchiveGui) -> Vec<&str> {
        app.items.iter().map(|item| item.name.as_str()).collect()
    }

    fn enter_password(app: &mut OpenArchiveGui, password: &str) {
        app.password_prompt.as_mut().expect("password prompt should be open").input = password.into();
        app.submit_password();
        app.wait_idle();
    }

    #[test]
    fn encrypted_archive_prompts_and_retries_listing() {
        let (dir, archive) = make_archive("list", Some("right"));
        let mut app = open_at(&archive);

        let prompt = app.password_prompt.as_ref().expect("opening should ask for a password");
        assert_eq!(prompt.op, ArchiveOp::List);
        assert!(!prompt.incorrect);
        assert!(app.items.is_empty());

        enter_password(&mut app, "wrong");
        assert!(app.password_prompt.as_ref().unwrap().incorrect);
        assert!(!app.passwords.contains_key(&archive));
        assert!(app.items.is_empty());

        enter_password(&mut app, "right");
        assert!(app.password_prompt.is_none());
        assert_eq!(item_names(&app), ["src"]);

        // The password is remembered while browsing the same archive.
        let src = app.items[0].clone();
        app.open_item(src);
        app.wait_idle();
        assert!(app.password_prompt.is_none());
        assert_eq!(item_names(&app), ["a.txt"]);

        let dest = dir.join("all");
        app.run_archive_op(archive.clone(), ArchiveOp::ExtractAll { dest: dest.clone() });
        app.wait_idle();
        assert!(app.password_prompt.is_none());
        assert_eq!(fs::read(dest.join("src/a.txt")).unwrap(), PAYLOAD);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_item_prompts_and_retries() {
        let (dir, archive) = make_archive("item", Some("right"));
        let mut app = OpenArchiveGui::with_location(NavLocation::Disk(dir.clone()), None);
        let dest = dir.join("one");
        let op = ArchiveOp::ExtractItem {
            full_internal: "src/a.txt".into(),
            name: "a.txt".into(),
            is_dir: false,
            dest: dest.clone(),
        };

        app.run_archive_op(archive.clone(), op.clone());

        app.wait_idle();
        assert_eq!(app.password_prompt.as_ref().unwrap().op, op);

        enter_password(&mut app, "right");
        assert!(app.password_prompt.is_none());
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), PAYLOAD);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelling_the_prompt_leaves_archive_closed() {
        let (dir, archive) = make_archive("cancel", Some("right"));
        let mut app = open_at(&archive);

        app.cancel_password_prompt();
        assert!(app.password_prompt.is_none());
        assert!(app.items.is_empty());
        assert!(app.status_message.contains("encrypted"), "{}", app.status_message);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unencrypted_archive_opens_without_prompt() {
        let (dir, archive) = make_archive("plain", None);
        let app = open_at(&archive);
        assert!(app.password_prompt.is_none());
        assert_eq!(item_names(&app), ["src"]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_failures_are_reported_not_prompted() {
        let (dir, _) = make_archive("corrupt", None);
        let corrupt = dir.join("corrupt.7z");
        fs::write(&corrupt, b"not an archive").unwrap();

        let app = open_at(&corrupt);
        assert!(app.password_prompt.is_none());
        assert!(app.status_message.starts_with("Failed to open archive"), "{}", app.status_message);
        assert!(app.notification.is_some());

        let _ = fs::remove_dir_all(&dir);
    }

    fn notification_text(app: &OpenArchiveGui) -> &str {
        app.notification.as_ref().map(|(text, _)| text.as_str()).unwrap_or("")
    }

    /// Stores `src/a.txt` uncompressed in a `.{ext}` archive, then flips one
    /// byte of its data so the headers still parse but the checksum fails.
    fn make_corrupt_archive(tag: &str, ext: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("openarchive-gui-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.txt"), PAYLOAD).unwrap();
        let archive = dir.join(format!("corrupt.{ext}"));
        let options = CompressionOptions { level: 0, ..Default::default() };
        create_archive(&archive, &[dir.join("src")], &options).unwrap();

        let mut bytes = fs::read(&archive).unwrap();
        let at = bytes
            .windows(PAYLOAD.len())
            .position(|window| window == PAYLOAD)
            .expect("stored payload should appear verbatim");
        bytes[at] ^= 0xFF;
        fs::write(&archive, bytes).unwrap();
        (dir, archive)
    }

    #[test]
    fn test_button_reports_a_healthy_archive() {
        let (dir, archive) = make_archive("test-ok", None);
        let mut app = open_at(&archive);

        app.test_current();

        app.wait_idle();
        assert!(app.password_prompt.is_none());
        assert!(notification_text(&app).ends_with("Everything is Ok"), "{}", notification_text(&app));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_button_reports_corruption() {
        for ext in ["zip", "7z"] {
            let (dir, archive) = make_corrupt_archive(&format!("test-bad-{ext}"), ext);
            let mut app = open_at(&archive);
            // Headers are intact, so the archive still opens and lists.
            assert_eq!(item_names(&app), ["src"], "{ext}");

            app.test_current();

            app.wait_idle();
            assert!(app.password_prompt.is_none(), "{ext}");
            assert!(
                notification_text(&app).starts_with("Test failed for corrupt."),
                "{ext}: {}",
                notification_text(&app)
            );

            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn test_button_prompts_for_encrypted_archive() {
        let (dir, archive) = make_archive("test-enc", Some("right"));
        // Start on disk so no password is cached yet.
        let mut app = OpenArchiveGui::with_location(NavLocation::Disk(dir.clone()), None);

        app.run_archive_op(archive.clone(), ArchiveOp::Test);
        app.wait_idle();
        assert_eq!(app.password_prompt.as_ref().unwrap().op, ArchiveOp::Test);

        enter_password(&mut app, "right");
        assert!(app.password_prompt.is_none());
        assert!(notification_text(&app).ends_with("Everything is Ok"), "{}", notification_text(&app));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── Background operations (OA-5) ─────────────────────────────────────────

    #[test]
    fn operations_return_immediately_and_finish_in_the_background() {
        let (dir, archive) = make_archive("bg", None);
        let mut app = open_at(&archive);
        let dest = dir.join("bg-out");

        app.run_archive_op(archive.clone(), ArchiveOp::ExtractAll { dest: dest.clone() });
        // The call returned without applying anything: the result is only
        // picked up by the per-frame poll.
        assert_eq!(app.running_ops.len(), 1);
        assert!(app.archive_busy(&archive), "Extract/Test must be disabled meanwhile");
        let busy = app.busy_label().expect("status bar shows the operation");
        assert!(busy.starts_with("Extracting archive.7z…"), "{busy}");

        app.wait_idle();
        assert!(app.busy_label().is_none());
        assert!(!app.archive_busy(&archive));
        assert_eq!(fs::read(dest.join("src/a.txt")).unwrap(), PAYLOAD);
        assert!(notification_text(&app).starts_with("Extracted to"), "{}", notification_text(&app));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn listing_finished_after_leaving_the_archive_is_discarded() {
        let (dir, archive) = make_archive("stale", None);
        // Start listing the archive, then go to the folder before it finishes.
        let mut app = OpenArchiveGui::with_location(
            NavLocation::Archive { archive_path: archive.clone(), internal_prefix: String::new() },
            None,
        );
        app.navigate_to(NavLocation::Disk(dir.clone()));
        app.wait_idle();

        // The folder's contents, not the archive's.
        let mut names = item_names(&app);
        names.sort_unstable();
        assert_eq!(names, ["archive.7z", "src"]);
        assert!(app.status_message.ends_with("items"), "{}", app.status_message);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn busy_label_counts_concurrent_operations() {
        let (dir, archive) = make_archive("many", None);
        let mut app = open_at(&archive);

        app.run_archive_op(archive.clone(), ArchiveOp::Test);
        app.run_archive_op(archive.clone(), ArchiveOp::ExtractAll { dest: dir.join("many-out") });
        let busy = app.busy_label().unwrap();
        assert!(busy.starts_with("Testing archive.7z…") && busy.ends_with("(+1 more)"), "{busy}");

        app.wait_idle();
        assert!(app.busy_label().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    // ── Progress and cancellation (OA-26) ────────────────────────────────────

    /// Registers a running extraction whose result channel the test holds,
    /// so its progress can be driven without a worker racing it.
    fn fake_running_extraction(app: &mut OpenArchiveGui, archive: &Path) -> (Arc<Progress>, mpsc::Sender<anyhow::Result<OpOutcome>>) {
        let (tx, rx) = mpsc::channel();
        let progress = Arc::new(Progress::new());
        app.running_ops.push(RunningOp {
            archive_path: archive.to_path_buf(),
            op: ArchiveOp::ExtractAll { dest: PathBuf::from("dest") },
            password: None,
            started: Instant::now(),
            progress: progress.clone(),
            result: rx,
        });
        (progress, tx)
    }

    #[test]
    fn status_bar_shows_percentage_and_cancel_state() {
        let mut app = OpenArchiveGui::with_location(NavLocation::Disk(std::env::temp_dir()), None);
        let (progress, tx) = fake_running_extraction(&mut app, Path::new("big.7z"));

        // Unknown total: bytes only.
        progress.add(3 * 1024 * 1024);
        let busy = app.busy_label().unwrap();
        assert!(busy.starts_with("Extracting big.7z… 3.0 MB "), "{busy}");
        assert_eq!(app.busy_fraction(), None);

        progress.set_total(200 * 1024 * 1024);
        progress.add(47 * 1024 * 1024);
        let busy = app.busy_label().unwrap();
        assert!(busy.starts_with("Extracting big.7z… 25% (50.0 MB of 200.0 MB) "), "{busy}");
        assert_eq!(app.busy_fraction(), Some(0.25));
        assert!(app.has_work_in_progress());

        app.cancel_running_ops();
        assert!(progress.is_cancelled());
        assert!(app.busy_label().unwrap().ends_with("(cancelling)"));

        // The worker reports the cancellation.
        tx.send(Err(anyhow::Error::new(std::io::Error::new(std::io::ErrorKind::Interrupted, crate::core::Cancelled))))
            .unwrap();
        app.poll_running_ops();
        assert!(app.running_ops.is_empty());
        assert_eq!(notification_text(&app), "Cancelled: Extracting big.7z");
    }

    #[test]
    fn cancelled_encrypted_extraction_does_not_ask_for_a_password() {
        let mut app = OpenArchiveGui::with_location(NavLocation::Disk(std::env::temp_dir()), None);
        // How sevenz-rust2 reports a cancel during an encrypted read.
        let cancelled = sevenz_rust2::Error::MaybeBadPassword(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            crate::core::Cancelled,
        ));
        app.finish_archive_op(
            PathBuf::from("secret.7z"),
            ArchiveOp::ExtractAll { dest: PathBuf::from("dest") },
            Some("right".into()),
            Err(anyhow::Error::new(cancelled)),
        );
        assert!(app.password_prompt.is_none());
        assert_eq!(notification_text(&app), "Cancelled: Extracting secret.7z");
    }

    #[test]
    fn a_worker_that_dies_is_reported() {
        let mut app = OpenArchiveGui::with_location(NavLocation::Disk(std::env::temp_dir()), None);
        let (_progress, tx) = fake_running_extraction(&mut app, Path::new("crash.7z"));
        drop(tx); // as if the worker panicked before answering
        app.poll_running_ops();
        assert!(app.running_ops.is_empty());
        assert!(notification_text(&app).contains("stopped unexpectedly"), "{}", notification_text(&app));
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(120 * 1024 * 1024), "120.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024 / 2), "1.50 GB");
    }
}

#[cfg(test)]
mod listing_tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> ArchiveEntry {
        ArchiveEntry {
            name: name.into(),
            size: 0,
            compressed_size: None,
            is_dir,
            modified: None,
            crc32: None,
            encrypted: false,
        }
    }

    fn names(entries: &[ArchiveEntry], prefix: &str) -> Vec<String> {
        filter_archive_entries(entries, prefix).into_iter().map(|item| item.name).collect()
    }

    #[test]
    fn folder_entries_are_listed_once() {
        // 7z style: folder entry without trailing slash, before or after its files.
        let before = [entry("src", true), entry("src/a.txt", false)];
        let after = [entry("src/a.txt", false), entry("src", true)];
        // zip style: folder entry with trailing slash.
        let zip = [entry("src/", true), entry("src/a.txt", false)];
        for entries in [&before[..], &after[..], &zip[..]] {
            assert_eq!(names(entries, ""), ["src"]);
            assert_eq!(names(entries, "src/"), ["a.txt"]);
        }
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;

    fn app_in_dir(tag: &str, files: usize) -> (PathBuf, OpenArchiveGui) {
        let dir = std::env::temp_dir().join(format!("openarchive-keys-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for i in 0..files {
            fs::write(dir.join(format!("f{i}.txt")), b"x").unwrap();
        }
        let app = OpenArchiveGui::with_location(NavLocation::Disk(dir.clone()), None);
        (dir, app)
    }

    fn selected(app: &OpenArchiveGui) -> Vec<usize> {
        let mut indices: Vec<usize> = app.selected_indices.iter().copied().collect();
        indices.sort_unstable();
        indices
    }

    #[test]
    fn arrows_in_an_empty_list_do_nothing() {
        let (dir, mut app) = app_in_dir("empty", 0);
        assert!(app.items.is_empty());
        for (down, extend) in [(true, false), (false, false), (true, true), (false, true)] {
            app.move_focus(down, extend);
        }
        assert!(app.selected_indices.is_empty());
        assert_eq!(app.anchor_index, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn arrows_stop_at_both_ends() {
        let (dir, mut app) = app_in_dir("ends", 3);
        for _ in 0..5 {
            app.move_focus(true, false);
        }
        assert_eq!(selected(&app), [2]);
        for _ in 0..5 {
            app.move_focus(false, false);
        }
        assert_eq!(selected(&app), [0]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_anchor_is_clamped() {
        let (dir, mut app) = app_in_dir("stale", 3);
        app.anchor_index = Some(10); // e.g. the list shrank on refresh
        app.move_focus(true, false);
        assert_eq!(selected(&app), [2]);
        let _ = fs::remove_dir_all(&dir);
    }

    // ── Mouse selection (OA-24) ──────────────────────────────────────────────
    //
    // These replay the per-frame order of `update()`: on the release frame the
    // row click handler runs first, then `update_rubber_band`.

    const ROW_H: f32 = 24.0;
    const LIST_TOP: f32 = 100.0;

    fn row_rects(rows: usize) -> Vec<(usize, Rect)> {
        (0..rows)
            .map(|i| {
                let top = LIST_TOP + ROW_H * i as f32;
                (i, Rect::from_min_size(egui::pos2(0.0, top), Vec2::new(300.0, ROW_H)))
            })
            .collect()
    }

    fn list_area() -> Rect {
        Rect::from_min_max(egui::pos2(0.0, LIST_TOP - 30.0), egui::pos2(300.0, 600.0))
    }

    fn row_center(i: usize) -> egui::Pos2 {
        egui::pos2(150.0, LIST_TOP + ROW_H * i as f32 + ROW_H / 2.0)
    }

    /// A click on row `idx`: press, then release with a 1 px jitter, with the
    /// click handler (`on_click`) running on the release frame first.
    fn click(app: &mut OpenArchiveGui, rows: &[(usize, Rect)], idx: usize, ctrl: bool, on_click: fn(&mut OpenArchiveGui, usize)) {
        let at = row_center(idx);
        let jitter = at + Vec2::new(1.0, 0.0);
        app.update_rubber_band(
            PointerFrame { pressed_at: Some(at), primary_down: true, hover: Some(at), additive: ctrl },
            list_area(),
            rows,
        );
        app.update_rubber_band(
            PointerFrame { pressed_at: None, primary_down: true, hover: Some(jitter), additive: ctrl },
            list_area(),
            rows,
        );
        on_click(app, idx);
        app.update_rubber_band(
            PointerFrame { pressed_at: None, primary_down: false, hover: Some(jitter), additive: ctrl },
            list_area(),
            rows,
        );
    }

    /// A drag from `from` to `to`, released at `to`.
    fn drag(app: &mut OpenArchiveGui, rows: &[(usize, Rect)], from: egui::Pos2, to: egui::Pos2, ctrl: bool) {
        app.update_rubber_band(
            PointerFrame { pressed_at: Some(from), primary_down: true, hover: Some(from), additive: ctrl },
            list_area(),
            rows,
        );
        app.update_rubber_band(
            PointerFrame { pressed_at: None, primary_down: true, hover: Some(to), additive: ctrl },
            list_area(),
            rows,
        );
        app.update_rubber_band(
            PointerFrame { pressed_at: None, primary_down: false, hover: Some(to), additive: ctrl },
            list_area(),
            rows,
        );
    }

    #[test]
    fn ctrl_click_adds_to_and_removes_from_selection() {
        let (dir, mut app) = app_in_dir("ctrl", 5);
        let rows = row_rects(5);
        click(&mut app, &rows, 0, false, OpenArchiveGui::select_single);
        click(&mut app, &rows, 2, true, OpenArchiveGui::select_toggle);
        click(&mut app, &rows, 4, true, OpenArchiveGui::select_toggle);
        assert_eq!(selected(&app), [0, 2, 4]);
        click(&mut app, &rows, 2, true, OpenArchiveGui::select_toggle);
        assert_eq!(selected(&app), [0, 4]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn shift_click_selects_from_anchor_to_clicked_row() {
        let (dir, mut app) = app_in_dir("shift", 5);
        let rows = row_rects(5);
        click(&mut app, &rows, 1, false, OpenArchiveGui::select_single);
        click(&mut app, &rows, 3, false, OpenArchiveGui::select_range_to);
        assert_eq!(selected(&app), [1, 2, 3]);
        // The anchor stays on row 1, so Shift-clicking above it flips the range.
        click(&mut app, &rows, 0, false, OpenArchiveGui::select_range_to);
        assert_eq!(selected(&app), [0, 1]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn drag_selects_rows_and_ctrl_drag_adds() {
        let (dir, mut app) = app_in_dir("drag", 6);
        let rows = row_rects(6);
        drag(&mut app, &rows, row_center(0), row_center(2), false);
        assert_eq!(selected(&app), [0, 1, 2]);

        // A plain drag replaces the selection...
        drag(&mut app, &rows, row_center(4), row_center(5), false);
        assert_eq!(selected(&app), [4, 5]);
        // ...a Ctrl-drag adds to it.
        drag(&mut app, &rows, row_center(0), row_center(1), true);
        assert_eq!(selected(&app), [0, 1, 4, 5]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn press_outside_the_list_does_not_start_a_band() {
        let (dir, mut app) = app_in_dir("outside", 3);
        let rows = row_rects(3);
        click(&mut app, &rows, 1, false, OpenArchiveGui::select_single);
        // Press on the toolbar, drag down across the rows.
        drag(&mut app, &rows, egui::pos2(150.0, 10.0), row_center(2), false);
        assert_eq!(selected(&app), [1]);
        assert!(app.rubber_band_rect().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn shift_arrows_extend_from_the_anchor() {
        let (dir, mut app) = app_in_dir("shiftarrows", 6);
        app.select_single(1);
        app.move_focus(true, true);
        app.move_focus(true, true);
        assert_eq!(selected(&app), [1, 2, 3]);
        app.move_focus(false, true);
        assert_eq!(selected(&app), [1, 2]);
        // Plain arrow collapses to the focused row and moves on from it.
        app.move_focus(true, false);
        assert_eq!(selected(&app), [3]);
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod disk_copy_tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openarchive-copy-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copies_a_single_file() {
        let dir = scratch("file");
        fs::write(dir.join("a.txt"), b"hello").unwrap();
        let dest = dir.join("dest");

        assert_eq!(copy_to_folder(&dir.join("a.txt"), &dest).unwrap(), 1);
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copies_a_folder_recursively() {
        let dir = scratch("tree");
        let src = dir.join("src");
        fs::create_dir_all(src.join("sub/deeper")).unwrap();
        fs::create_dir_all(src.join("empty")).unwrap();
        fs::write(src.join("top.txt"), b"1").unwrap();
        fs::write(src.join("sub/mid.txt"), b"2").unwrap();
        fs::write(src.join("sub/deeper/low.txt"), b"3").unwrap();
        let dest = dir.join("dest");

        assert_eq!(copy_to_folder(&src, &dest).unwrap(), 3);
        assert_eq!(fs::read(dest.join("src/top.txt")).unwrap(), b"1");
        assert_eq!(fs::read(dest.join("src/sub/mid.txt")).unwrap(), b"2");
        assert_eq!(fs::read(dest.join("src/sub/deeper/low.txt")).unwrap(), b"3");
        assert!(dest.join("src/empty").is_dir());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_copy_a_file_onto_itself() {
        let dir = scratch("same");
        fs::write(dir.join("a.txt"), b"keep me").unwrap();

        let err = copy_to_folder(&dir.join("a.txt"), &dir).unwrap_err();
        assert!(err.to_string().contains("same"), "{err}");
        assert_eq!(fs::read(dir.join("a.txt")).unwrap(), b"keep me");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_copy_a_folder_into_itself() {
        let dir = scratch("into");
        let src = dir.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), b"x").unwrap();

        for dest in [src.clone(), src.join("sub")] {
            let err = copy_to_folder(&src, &dest).unwrap_err();
            assert!(err.to_string().contains("itself") || err.to_string().contains("same"), "{err}");
        }
        assert!(!src.join("src").exists() && !src.join("sub/src").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_folder_reports_an_error() {
        let missing = scratch("missing").join("does-not-exist");
        let app = OpenArchiveGui::with_location(NavLocation::Disk(missing), None);
        assert!(app.items.is_empty());
        assert!(app.status_message.starts_with("Cannot read"), "{}", app.status_message);
        assert!(app.notification.is_some());
    }
}
