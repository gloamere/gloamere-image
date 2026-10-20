//! Small document actions shared by the menus and keyboard shortcuts.
use super::*;
use light_image_tool::memory::{self, Pixels};
use std::path::Path;

const HISTORY_ENTRIES: usize = 3;
const HISTORY_BYTES: u64 = 256 * 1024 * 1024;

fn history_pixel_bytes(pixels: &Pixels) -> u64 {
    // Display preparation is lazy, but may remain owned by every history image.
    // Include its full RGBA-sized reservation without preparing it on the UI thread.
    (pixels.as_raw().capacity() as u64).saturating_add(pixels.as_raw().len() as u64)
}

fn history_bytes(undo: &[Arc<Pixels>], redo: &[Arc<Pixels>]) -> u64 {
    let mut unique: Vec<&Arc<Pixels>> = Vec::new();
    for pixels in undo.iter().chain(redo) {
        if !unique.iter().any(|other| Arc::ptr_eq(other, pixels)) {
            unique.push(pixels);
        }
    }
    unique.into_iter().fold(0, |total, pixels| {
        total.saturating_add(history_pixel_bytes(pixels))
    })
}

fn evict_farthest(undo: &mut Vec<Arc<Pixels>>, redo: &mut Vec<Arc<Pixels>>) -> bool {
    // Index zero is the most distant state on either side of the current image.
    if !undo.is_empty() && undo.len() >= redo.len() {
        undo.remove(0);
        true
    } else if !redo.is_empty() {
        redo.remove(0);
        true
    } else {
        false
    }
}

fn trim_history(
    undo: &mut Vec<Arc<Pixels>>,
    redo: &mut Vec<Arc<Pixels>>,
    max_entries: usize,
    max_bytes: u64,
) {
    while undo.len().saturating_add(redo.len()) > max_entries
        || history_bytes(undo, redo) > max_bytes
    {
        if !evict_farthest(undo, redo) {
            break;
        }
    }
}

impl ImageApp {
    pub(super) fn set_export_format(&mut self, format: ExportFormat) {
        if self.format == format {
            return;
        }
        self.format = format;
        self.export_name = if self.export_name.is_empty() {
            self.suggested_export()
        } else {
            PathBuf::from(&self.export_name)
                .with_extension(if format == ExportFormat::Png {
                    "png"
                } else {
                    "jpg"
                })
                .to_string_lossy()
                .into_owned()
        };
        self.preview_matte = None;
        self.preview_texture = None;
        self.preview_reservation = None;
        if let Some(task) = &self.preview_task {
            task.cancel.cancel();
        }
    }

    pub(super) fn clear_edit_history(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub(super) fn edit_history_bytes(&self) -> u64 {
        history_bytes(&self.undo, &self.redo)
    }

    pub(super) fn trim_edit_history(&mut self) {
        trim_history(
            &mut self.undo,
            &mut self.redo,
            HISTORY_ENTRIES,
            HISTORY_BYTES,
        );
    }

    pub(super) fn record_edit_history(&mut self, previous: Arc<Pixels>) {
        // Committing another edit creates a new branch of the document history.
        self.redo.clear();
        self.undo.push(previous);
        self.trim_edit_history();
    }

    pub(super) fn evict_edit_history(&mut self, extra: u64) {
        while memory::in_use().saturating_add(extra) > memory::OPERATION_BUDGET {
            if !evict_farthest(&mut self.undo, &mut self.redo) {
                break;
            }
        }
    }

    pub(super) fn undo_image_edit(&mut self, ctx: &egui::Context) {
        self.move_edit_history(ctx, false);
    }

    pub(super) fn redo_edit(&mut self, ctx: &egui::Context) {
        self.move_edit_history(ctx, true);
    }

    fn move_edit_history(&mut self, ctx: &egui::Context, redo: bool) {
        if self.rename.blocks_document()
            || self.work.is_some()
            || self.loading
            || self.shutting_down
            || self.pending_intent.is_some()
        {
            return;
        }
        let Some(current) = self.pixels.clone() else {
            return;
        };
        let Some(target) = (if redo { &self.redo } else { &self.undo }).last().cloned() else {
            return;
        };
        self.close_export();
        // Upload admission can fail. Keep both document identity and history intact
        // until the target can actually replace the visible image.
        if let Err(error) = self.upload_pixels(&target, ctx, &Cancellation::default()) {
            self.error = Some(error.to_string());
            return;
        }
        if redo {
            self.redo.pop();
            self.undo.push(current);
        } else {
            self.undo.pop();
            self.redo.push(current);
        }
        self.pixels = Some(target);
        self.trim_edit_history();
        self.edited = !matches!(self.source, DocumentSource::File)
            || self
                .original_pixels()
                .as_ref()
                .zip(self.pixels.as_ref())
                .is_some_and(|(original, current)| !Arc::ptr_eq(original, current));
        self.crop_selecting = false;
        self.crop_anchor = None;
        let [w, h] = self.dimensions();
        self.resize = [w, h];
        self.crop = [0, 0, w, h];
        self.fit = true;
        self.pan = Vec2::ZERO;
        self.error = None;
        self.status = if redo {
            "已重做编辑"
        } else {
            "已撤销编辑"
        }
        .into();
        ctx.request_repaint();
    }

    pub(super) fn current_file_path(&self) -> Option<PathBuf> {
        if self.loading || !matches!(self.source, DocumentSource::File) {
            return None;
        }
        self.loaded.as_ref().map(|image| image.path.clone())
    }

    pub(super) fn reload_current(&mut self) {
        if self.rename.blocks_document()
            || self.work.is_some()
            || self.shutting_down
            || self.pending_intent.is_some()
        {
            return;
        }
        let Some(path) = self.current_file_path() else {
            return;
        };
        self.loader.clear_cache();
        // Reuse the ordinary open intent so edited pixels still receive the save prompt.
        self.open(path);
    }

    pub(super) fn show_current_in_folder(&mut self, ctx: &egui::Context) {
        if self.rename.blocks_document()
            || self.work.is_some()
            || self.shutting_down
            || self.pending_intent.is_some()
        {
            return;
        }
        let Some(path) = self.current_file_path() else {
            return;
        };
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let (tx, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result = protected(|| reveal_file(&path, &token), "文件定位未完成，请重试");
            let _ = tx.try_send(WorkResult::FileLocated(result));
        });
        self.work = Some(Work {
            generation: self.generation,
            cancel,
            receiver,
            export_thread: Some(thread),
        });
        self.status = "正在文件夹中定位当前图片…".into();
        ctx.request_repaint();
    }
}

#[cfg(windows)]
fn shell_path_wide(path: &Path) -> light_image_tool::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    if path.as_os_str().encode_wide().any(|unit| unit == 0) {
        return Err(ToolError::Invalid("文件路径包含无效字符".into()));
    }
    let absolute = std::path::absolute(path)?;
    Ok(absolute
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect())
}

#[cfg(windows)]
fn reveal_file(path: &Path, cancel: &Cancellation) -> light_image_tool::Result<String> {
    use windows_sys::Win32::{
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
        UI::Shell::{Common::ITEMIDLIST, SHOpenFolderAndSelectItems, SHParseDisplayName},
    };
    struct Apartment(bool);
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }
    struct ShellItem(*mut ITEMIDLIST);
    impl Drop for ShellItem {
        fn drop(&mut self) {
            unsafe { CoTaskMemFree(self.0.cast()) };
        }
    }
    cancel.check()?;
    let wide = shell_path_wide(path)?;
    if !std::fs::metadata(path)?.is_file() {
        return Err(ToolError::Invalid("当前图片已不是可定位的文件".into()));
    }
    cancel.check()?;
    let initialized = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    // A caller-owned apartment may already use another COM model.
    if initialized < 0 && initialized != 0x80010106u32 as i32 {
        return Err(ToolError::Invalid(format!(
            "无法初始化文件定位组件 (0x{:08X})",
            initialized as u32
        )));
    }
    let _apartment = Apartment(initialized >= 0);
    let mut raw = std::ptr::null_mut();
    let parsed = unsafe {
        SHParseDisplayName(
            wide.as_ptr(),
            std::ptr::null_mut(),
            &mut raw,
            0,
            std::ptr::null_mut(),
        )
    };
    let item = ShellItem(raw);
    if parsed < 0 || item.0.is_null() {
        return Err(ToolError::Invalid(format!(
            "无法定位当前图片 (0x{:08X})",
            parsed as u32
        )));
    }
    cancel.check()?;
    cancel.publish(|| {
        // With cidl=0 the fully qualified item opens its parent and selects itself.
        let result = unsafe { SHOpenFolderAndSelectItems(item.0, 0, std::ptr::null(), 0) };
        if result < 0 {
            return Err(ToolError::Invalid(format!(
                "无法在文件夹中显示当前图片 (0x{:08X})",
                result as u32
            )));
        }
        Ok("已在文件夹中显示当前图片".into())
    })
}

#[cfg(not(windows))]
fn reveal_file(_path: &Path, cancel: &Cancellation) -> light_image_tool::Result<String> {
    cancel.check()?;
    Err(ToolError::Invalid("当前平台暂不支持在文件夹中显示".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixels(value: u8) -> Arc<Pixels> {
        Arc::new(
            Pixels::from_image(image::RgbaImage::from_pixel(
                4,
                3,
                image::Rgba([value, 0, 0, 255]),
            ))
            .unwrap(),
        )
    }

    fn app() -> (ImageApp, egui::Context) {
        let ctx = egui::Context::default();
        let app = ImageApp::from_context(ctx.clone(), "actions-test".into(), None, None);
        (app, ctx)
    }

    fn file_app() -> (tempfile::TempDir, ImageApp, egui::Context) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("original.png");
        image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();
        let (mut app, ctx) = app();
        let loaded = light_image_tool::pipeline::load(&path, &Cancellation::default()).unwrap();
        app.pixels = Some(loaded.pixels.clone());
        app.loaded = Some(loaded);
        (directory, app, ctx)
    }

    fn commit(app: &mut ImageApp, next: Arc<Pixels>) {
        let previous = app.pixels.replace(next).unwrap();
        app.record_edit_history(previous);
        app.edited = true;
    }

    #[test]
    fn export_format_preserves_custom_directory_and_stem() {
        let (mut app, _) = app();
        let destination = PathBuf::from("custom folder").join("名字, (v2) & final.part.PNG");
        app.export_name = destination.to_string_lossy().into_owned();
        app.export_dialog = true;
        app.export_generation = Some(app.generation);
        app.set_export_format(ExportFormat::Jpeg);
        assert_eq!(
            PathBuf::from(&app.export_name),
            destination.with_extension("jpg")
        );
        assert!(app.export_dialog);
        assert_eq!(app.export_generation, Some(app.generation));
        app.set_export_format(ExportFormat::Png);
        assert_eq!(
            PathBuf::from(&app.export_name),
            destination.with_extension("png")
        );
    }

    #[test]
    fn format_change_cancels_preview_without_closing_export() {
        let (mut app, _) = app();
        app.export_dialog = true;
        app.format = ExportFormat::Jpeg;
        app.preview_matte = Some(app.matte);
        let cancel = Cancellation::default();
        let (_tx, receiver) = mpsc::sync_channel(1);
        app.preview_task = Some(PreviewTask {
            generation: app.generation,
            matte: app.matte,
            cancel: cancel.clone(),
            receiver,
            thread: None,
        });
        app.set_export_format(ExportFormat::Png);
        assert!(matches!(cancel.check(), Err(ToolError::Cancelled)));
        assert!(app.preview_matte.is_none());
        assert!(app.export_dialog);
    }

    #[test]
    fn undo_redo_retains_export_identity_and_original_clean_state() {
        let (_directory, mut app, ctx) = file_app();
        let original = app.pixels.clone().unwrap();
        let saved_edit = pixels(40);
        commit(&mut app, saved_edit.clone());
        app.exported_pixels = Some(Arc::downgrade(&saved_edit));
        assert!(!app.unsaved());
        commit(&mut app, pixels(80));
        assert!(app.unsaved());
        app.undo_image_edit(&ctx);
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &saved_edit));
        assert!(!app.unsaved());
        app.undo_image_edit(&ctx);
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &original));
        assert!(!app.edited);
        assert!(!app.unsaved());
        app.redo_edit(&ctx);
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &saved_edit));
        assert!(app.edited);
        assert!(!app.unsaved());
        app.redo_edit(&ctx);
        assert!(app.unsaved());
        assert_eq!(app.undo.len() + app.redo.len(), 2);
    }

    #[test]
    fn new_edit_discards_redo_and_history_has_one_shared_limit() {
        let (mut app, ctx) = app();
        app.source = DocumentSource::Clipboard;
        app.pixels = Some(pixels(0));
        for value in 1..=4 {
            commit(&mut app, pixels(value));
        }
        assert_eq!(app.undo.len(), HISTORY_ENTRIES);
        app.undo_image_edit(&ctx);
        app.undo_image_edit(&ctx);
        assert_eq!(app.undo.len() + app.redo.len(), HISTORY_ENTRIES);
        assert_eq!(app.redo.len(), 2);
        assert!(app.unsaved());
        commit(&mut app, pixels(90));
        assert!(app.redo.is_empty());
        app.clear_edit_history();
        assert!(app.undo.is_empty() && app.redo.is_empty());
    }

    #[test]
    fn history_budget_includes_prepared_display_and_deduplicates_arcs() {
        let first = pixels(1);
        first.prepare_display(&Cancellation::default()).unwrap();
        let second = pixels(2);
        let cost = first.as_raw().capacity() as u64 + first.as_raw().len() as u64;
        assert_eq!(history_pixel_bytes(&first), cost);
        assert_eq!(
            history_bytes(std::slice::from_ref(&first), std::slice::from_ref(&first)),
            cost
        );
        let mut undo = vec![first.clone()];
        let mut redo = vec![second.clone()];
        // Two raw images fit, but their retained CPU display buffers do not.
        let raw_bytes = first.as_raw().capacity() as u64 + second.as_raw().capacity() as u64;
        trim_history(&mut undo, &mut redo, HISTORY_ENTRIES, raw_bytes);
        assert_eq!(undo.len() + redo.len(), 1);
        assert!(history_bytes(&undo, &redo) <= raw_bytes);
    }

    #[test]
    fn memory_pressure_evicts_both_history_stacks() {
        let (mut app, _) = app();
        app.undo.push(pixels(1));
        app.redo.push(pixels(2));
        app.evict_edit_history(memory::OPERATION_BUDGET);
        assert!(app.undo.is_empty() && app.redo.is_empty());
    }

    #[test]
    fn undo_upload_failure_preserves_document_and_history() {
        let (mut app, ctx) = app();
        let current = pixels(2);
        let image = image::RgbaImage::from_pixel(4, 3, image::Rgba([1, 0, 0, 255]));
        // The target's isolated budget fits RGBA but cannot admit its display buffer.
        let budget = memory::Budget::new(image.as_raw().capacity() as u64);
        let mut reservation = budget.reserve(image.as_raw().capacity() as u64).unwrap();
        let previous = Arc::new(Pixels::from_reserved(image, &mut reservation).unwrap());
        app.pixels = Some(current.clone());
        app.undo.push(previous.clone());
        app.edited = true;
        app.undo_image_edit(&ctx);
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &current));
        assert!(Arc::ptr_eq(app.undo.last().unwrap(), &previous));
        assert!(app.redo.is_empty());
        assert!(app.error.is_some());
    }

    #[test]
    fn reload_preserves_unsaved_guard_and_temporary_sessions_have_no_file_actions() {
        let (_directory, mut app, _) = file_app();
        let path = app.current_file_path().unwrap();
        commit(&mut app, pixels(40));
        let edited = app.pixels.clone().unwrap();
        let generation = app.generation;
        app.reload_current();
        assert_eq!(app.pending_intent, Some(PendingIntent::Open(path)));
        assert_eq!(app.generation, generation);
        assert!(!app.loading);
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &edited));
        app.pending_intent = None;
        app.source = DocumentSource::Clipboard;
        assert!(app.current_file_path().is_none());
        app.reload_current();
        assert!(app.pending_intent.is_none());
    }

    #[test]
    fn cancelled_folder_reveal_never_opens_explorer() {
        let token = Cancellation::default();
        token.cancel();
        assert!(matches!(
            reveal_file(Path::new("does-not-need-to-exist.png"), &token),
            Err(ToolError::Cancelled)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn shell_item_path_preserves_unicode_and_shell_metacharacters() {
        use std::os::windows::ffi::OsStringExt;
        let path = PathBuf::from("名字 folder").join("a, (v2) & # final.png");
        let wide = shell_path_wide(&path).unwrap();
        assert_eq!(wide.last(), Some(&0));
        assert!(!wide[..wide.len() - 1].contains(&0));
        assert_eq!(
            PathBuf::from(std::ffi::OsString::from_wide(&wide[..wide.len() - 1])),
            std::path::absolute(path).unwrap()
        );
        assert!(shell_path_wide(Path::new("bad\0name.png")).is_err());
    }
}
