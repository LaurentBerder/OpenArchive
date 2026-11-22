use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use eframe::egui::{self, Color32, FontId, Key, PointerButton, Rect, RichText, Sense, Vec2};

use crate::core::{ArchiveEntry, ArchiveFormat, ExtractOptions};
use crate::formats::{get_reader, password_problem};

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

    /// True while the user is drag-selecting; tracks the initial pointer position.
    drag_select_start: Option<egui::Pos2>,
    /// Current pointer position during a drag (updated each frame).
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
}

// ─── Platform helpers ─────────────────────────────────────────────────────────

fn open_file_externally(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let path_str = path.to_string_lossy().to_string();
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", &path_str])
            .spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(path)
            .spawn();
    }
}

fn open_with_dialog(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let path_str = path.to_string_lossy().to_string();
        let _ = std::process::Command::new("rundll32.exe")
            .args(["shell32.dll,OpenAs_RunDLL", &path_str])
            .spawn();
    }
}

// ─── App impl ─────────────────────────────────────────────────────────────────

impl OpenArchiveGui {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let initial = NavLocation::Disk(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Self::with_location(initial)
    }

    /// Builds the app state at `initial` without any UI context.
    fn with_location(initial: NavLocation) -> Self {
        let mut app = Self {
            current_location: initial.clone(),
            history_back: Vec::new(),
            history_forward: Vec::new(),
            items: Vec::new(),
            selected_indices: HashSet::new(),
            anchor_index: None,
            drag_select_start: None,
            drag_select_current: None,
            status_message: "Ready".to_string(),
            notification: None,
            properties_item: None,
            passwords: HashMap::new(),
            password_prompt: None,
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
        self.drag_select_start = None;
        self.drag_select_current = None;
    }

    fn select_all(&mut self) {
        self.selected_indices = (0..self.items.len()).collect();
        self.anchor_index = Some(0);
        self.update_status();
    }

    /// Single-click: set exactly this row as selected (clears others). Updates anchor.
    fn select_single(&mut self, idx: usize) {
        self.selected_indices.clear();
        self.selected_indices.insert(idx);
        self.anchor_index = Some(idx);
        self.update_status();
    }

    /// Ctrl+click: toggle membership of this row. Updates anchor to clicked row.
    fn select_toggle(&mut self, idx: usize) {
        if self.selected_indices.contains(&idx) {
            self.selected_indices.remove(&idx);
        } else {
            self.selected_indices.insert(idx);
        }
        self.anchor_index = Some(idx);
        self.update_status();
    }

    /// Shift+click: range from anchor to idx (inclusive). Does not change anchor.
    fn select_range_to(&mut self, idx: usize) {
        let anchor = self.anchor_index.unwrap_or(idx);
        let lo = anchor.min(idx);
        let hi = anchor.max(idx);
        self.selected_indices.clear();
        for i in lo..=hi {
            self.selected_indices.insert(i);
        }
        self.update_status();
    }

    /// Rubber-band drag: select all rows whose rects intersect the drag rectangle.
    fn apply_rubber_band(&mut self, drag_rect: Rect, row_rects: &[(usize, Rect)]) {
        self.selected_indices.clear();
        for (idx, row_rect) in row_rects {
            if drag_rect.intersects(*row_rect) {
                self.selected_indices.insert(*idx);
            }
        }
        self.update_status();
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
                if let Ok(entries) = fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        let name = entry.file_name().to_string_lossy().to_string();
                        let is_dir = path.is_dir();
                        let is_archive = !is_dir && ArchiveFormat::from_path(&path).is_some();
                        let size = if is_dir { 0 } else { entry.metadata().map(|m| m.len()).unwrap_or(0) };
                        self.items.push(DisplayItem { name, is_dir, is_archive, size, compressed_size: None });
                    }
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

    /// Runs `op` on `archive_path` with the password remembered for it.
    ///
    /// If the archive needs a password, or the remembered one is wrong, the
    /// password dialog is opened and `op` is retried when the user submits it.
    /// Other failures are reported in a notification.
    fn run_archive_op(&mut self, archive_path: PathBuf, op: ArchiveOp) {
        let password = self.passwords.get(&archive_path).cloned();
        let Err(err) = self.execute_archive_op(&archive_path, &op, password.clone()) else {
            return;
        };

        if password_problem(&err).is_some() {
            self.passwords.remove(&archive_path);
            self.status_message = format!(
                "Password required: {}",
                archive_path.file_name().unwrap_or_default().to_string_lossy()
            );
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
            ArchiveOp::Test => format!(
                "Test failed for {}: {err:#}",
                archive_path.file_name().unwrap_or_default().to_string_lossy()
            ),
        };
        if op == ArchiveOp::List {
            self.status_message = message.clone();
        }
        self.show_notification(message);
    }

    fn execute_archive_op(
        &mut self,
        archive_path: &Path,
        op: &ArchiveOp,
        password: Option<String>,
    ) -> anyhow::Result<()> {
        let mut reader = get_reader(archive_path, password.clone())?;
        let extract_options = |output_dir: PathBuf, flat: bool| ExtractOptions {
            output_dir,
            flat,
            password: password.clone(),
            overwrite: true,
            threads: None,
        };

        match op {
            ArchiveOp::List => {
                // The user may have navigated away while the dialog was open.
                let NavLocation::Archive { archive_path: current, internal_prefix } = &self.current_location
                else {
                    return Ok(());
                };
                if current != archive_path {
                    return Ok(());
                }
                let entries = reader.list()?;
                self.items = filter_archive_entries(&entries, internal_prefix);
                self.sort_items();
                self.status_message = format!(
                    "Archive: {} [{} items]",
                    archive_path.file_name().unwrap_or_default().to_string_lossy(),
                    self.items.len()
                );
            }
            ArchiveOp::Preview { full_internal, file_name, open_with } => {
                let arc_stem = archive_path.file_stem().unwrap_or_default().to_string_lossy();
                let temp_dir = std::env::temp_dir().join("OpenArchive").join(arc_stem.as_ref());
                fs::create_dir_all(&temp_dir)?;
                let filter = [full_internal.clone()];
                reader.extract(&extract_options(temp_dir.clone(), true), Some(&filter))?;

                let target_file = temp_dir.join(file_name);
                if !target_file.exists() {
                    anyhow::bail!("{full_internal} was not found in the archive");
                }
                if *open_with { open_with_dialog(&target_file); } else { open_file_externally(&target_file); }
                self.show_notification(format!("Opened {file_name}"));
            }
            ArchiveOp::ExtractItem { full_internal, name, is_dir, dest } => {
                let filter = [full_internal.clone()];
                reader.extract(&extract_options(dest.clone(), !is_dir), Some(&filter))?;
                self.show_notification(format!("Extracted {} to {}", name, dest.display()));
            }
            ArchiveOp::ExtractAll { dest } => {
                reader.extract(&extract_options(dest.clone(), false), None)?;
                self.show_notification(format!("Extracted to {}", dest.display()));
            }
            ArchiveOp::Test => {
                reader.test(password.as_deref())?;
                self.show_notification(format!(
                    "{}: Everything is Ok",
                    archive_path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
        Ok(())
    }

    /// Retries the pending operation with the password typed in the dialog.
    fn submit_password(&mut self) {
        let Some(prompt) = self.password_prompt.take() else {
            return;
        };
        self.passwords.insert(prompt.archive_path.clone(), prompt.input);
        self.run_archive_op(prompt.archive_path, prompt.op);
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
                    open_file_externally(&target);
                    self.show_notification(format!("Opening {}", item.name));
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
                    NavLocation::Disk(dir) => { open_with_dialog(&dir.join(&item.name)); }
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
                            let target = dest.join(&item.name);
                            if item.is_dir { let _ = fs::create_dir_all(&target); }
                            else { let _ = fs::copy(&source, &target); }
                            self.show_notification(format!("Copied to {}", dest.display()));
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
            let total = self.items.len();
            if !total == 0 { return; }

            if i.key_pressed(Key::ArrowDown) && !i.modifiers.alt {
                let anchor = self.anchor_index.unwrap_or(0);
                let next = (anchor + 1).min(total - 1);
                if i.modifiers.shift {
                    self.select_range_to(next);
                } else {
                    self.select_single(next);
                }
            }
            if i.key_pressed(Key::ArrowUp) && !i.modifiers.alt {
                let anchor = self.anchor_index.unwrap_or(0);
                let prev = anchor.saturating_sub(1);
                if i.modifiers.shift {
                    self.select_range_to(prev);
                } else {
                    self.select_single(prev);
                }
            }

            // Enter opens the focused (anchor) item
            if i.key_pressed(Key::Enter) {
                if let Some(idx) = self.anchor_index {
                    if let Some(item) = self.items.get(idx) {
                        to_open = Some(item.clone());
                    }
                }
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
                            .add_filter("Archives", &["7z", "zip", "tar", "gz", "bz2"])
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

                let can_extract = matches!(self.current_location, NavLocation::Archive { .. });
                if ui.add_enabled(can_extract, egui::Button::new(RichText::new("📤 Extract").size(14.0))).clicked() {
                    self.extract_current();
                }
                if ui.add_enabled(can_extract, egui::Button::new(RichText::new("✔ Test").size(14.0))).clicked() {
                    self.test_current();
                }

                ui.separator();

                if ui.button(RichText::new("📂 Open Archive").size(14.0)).clicked() {
                    if let Some(file) = rfd::FileDialog::new()
                        .add_filter("Supported Archives (*.7z, *.zip, *.tar.gz)", &["7z", "zip", "tar", "gz", "bz2"])
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
                // Detect drag starting on empty space (not on a row)
                let panel_resp = ui.allocate_response(
                    Vec2::new(available_w, 0.0), // zero-height, just for input
                    Sense::drag(),
                );

                ctx.input(|i| {
                    // Start drag only when primary button goes down and no item was clicked
                    if i.pointer.primary_pressed() && click_info.is_none() {
                        if let Some(pos) = i.pointer.press_origin() {
                            self.drag_select_start = Some(pos);
                        }
                    }
                    if i.pointer.primary_down() {
                        if self.drag_select_start.is_some() {
                            self.drag_select_current = i.pointer.hover_pos();
                        }
                    }
                    if !i.pointer.primary_down() {
                        // Drag released: apply and clear
                        if let (Some(start), Some(end)) = (self.drag_select_start, self.drag_select_current) {
                            let drag_rect = Rect::from_two_pos(start, end);
                            self.apply_rubber_band(drag_rect, &row_rects);
                        }
                        self.drag_select_start = None;
                        self.drag_select_current = None;
                    }
                });

                // Draw rubber-band rectangle
                if let (Some(start), Some(end)) = (self.drag_select_start, self.drag_select_current) {
                    let drag_rect = Rect::from_two_pos(start, end);
                    ui.painter().rect(
                        drag_rect,
                        0.0,
                        Color32::from_rgba_unmultiplied(60, 120, 220, 25),
                        egui::Stroke::new(1.0_f32, Color32::from_rgb(60, 120, 220)),
                    );
                }

                // Drop panel_resp to suppress unused warning
                drop(panel_resp);

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

    fn open_at(archive: &Path) -> OpenArchiveGui {
        OpenArchiveGui::with_location(NavLocation::Archive {
            archive_path: archive.to_path_buf(),
            internal_prefix: String::new(),
        })
    }

    fn item_names(app: &OpenArchiveGui) -> Vec<&str> {
        app.items.iter().map(|item| item.name.as_str()).collect()
    }

    fn enter_password(app: &mut OpenArchiveGui, password: &str) {
        app.password_prompt.as_mut().expect("password prompt should be open").input = password.into();
        app.submit_password();
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
        assert!(app.password_prompt.is_none());
        assert_eq!(item_names(&app), ["a.txt"]);

        let dest = dir.join("all");
        app.run_archive_op(archive.clone(), ArchiveOp::ExtractAll { dest: dest.clone() });
        assert!(app.password_prompt.is_none());
        assert_eq!(fs::read(dest.join("src/a.txt")).unwrap(), PAYLOAD);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_item_prompts_and_retries() {
        let (dir, archive) = make_archive("item", Some("right"));
        let mut app = OpenArchiveGui::with_location(NavLocation::Disk(dir.clone()));
        let dest = dir.join("one");
        let op = ArchiveOp::ExtractItem {
            full_internal: "src/a.txt".into(),
            name: "a.txt".into(),
            is_dir: false,
            dest: dest.clone(),
        };

        app.run_archive_op(archive.clone(), op.clone());
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
        let mut app = OpenArchiveGui::with_location(NavLocation::Disk(dir.clone()));

        app.run_archive_op(archive.clone(), ArchiveOp::Test);
        assert_eq!(app.password_prompt.as_ref().unwrap().op, ArchiveOp::Test);

        enter_password(&mut app, "right");
        assert!(app.password_prompt.is_none());
        assert!(notification_text(&app).ends_with("Everything is Ok"), "{}", notification_text(&app));

        let _ = fs::remove_dir_all(&dir);
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
