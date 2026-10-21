//! Explicit file renaming. Image pixels and GPU resources keep their document identity.
use super::*;
use std::{ffi::OsString, path::Path, thread::JoinHandle};

struct RenameDialog {
    source: PathBuf,
    stem: String,
    extension: String,
    error: Option<String>,
    focus: bool,
}

struct RenameJob {
    ticket: u64,
    generation: u64,
    source: PathBuf,
    cancel: Cancellation,
    receiver: mpsc::Receiver<light_image_tool::Result<PathBuf>>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for RenameJob {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("rename worker panicked during shutdown");
        }
    }
}

#[derive(Default)]
pub(super) struct RenameState {
    dialog: Option<RenameDialog>,
    job: Option<RenameJob>,
    next_ticket: u64,
}
impl RenameState {
    pub(super) fn is_open(&self) -> bool {
        self.dialog.is_some()
    }
    pub(super) fn is_busy(&self) -> bool {
        self.job.is_some()
    }
    pub(super) fn blocks_document(&self) -> bool {
        self.is_open() || self.is_busy()
    }
    pub(super) fn cancel_dialog(&mut self) -> bool {
        if self.is_busy() {
            return false;
        }
        self.dialog.take().is_some()
    }
    pub(super) fn shutdown(&mut self) {
        self.dialog = None;
        // Drop cancels before publication, or joins the real result if publication won.
        self.job = None;
    }
}

fn valid_stem(stem: &str) -> light_image_tool::Result<()> {
    if stem.trim().is_empty() || stem == "." || stem == ".." {
        return Err(ToolError::Invalid("文件主名不能为空".into()));
    }
    if stem.ends_with([' ', '.']) {
        return Err(ToolError::Invalid("文件名不能以空格或句点结尾".into()));
    }
    if stem
        .chars()
        .any(|c| c <= '\u{1f}' || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
    {
        return Err(ToolError::Invalid(
            "文件名不能包含路径分隔符、控制字符或 <>:\"|?*".into(),
        ));
    }
    let base = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let numbered_device = ["COM", "LPT"].iter().any(|prefix| {
        base.strip_prefix(*prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    if numbered_device
        || matches!(
            base.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
        )
    {
        return Err(ToolError::Invalid("此文件名是 Windows 保留名称".into()));
    }
    Ok(())
}

fn rename_target(source: &Path, stem: &str) -> light_image_tool::Result<PathBuf> {
    valid_stem(stem)?;
    if source.file_name().is_none() {
        return Err(ToolError::Invalid("当前路径没有可重命名的文件".into()));
    }
    let mut name = OsString::from(stem);
    if let Some(extension) = source.extension() {
        name.push(".");
        name.push(extension);
    }
    #[cfg(windows)]
    let units = {
        use std::os::windows::ffi::OsStrExt;
        name.encode_wide().count()
    };
    #[cfg(not(windows))]
    let units = name.to_string_lossy().encode_utf16().count();
    if units > 255 {
        return Err(ToolError::Invalid(
            "文件名与扩展名合计不能超过 255 个 UTF-16 单元".into(),
        ));
    }
    Ok(source.with_file_name(name))
}

#[cfg(windows)]
fn rename_file(
    source: &Path,
    target: &Path,
    expected_sha256: &str,
    cancel: &Cancellation,
) -> light_image_tool::Result<PathBuf> {
    cancel.check()?;
    if source.parent() != target.parent() || source.extension() != target.extension() {
        return Err(ToolError::Invalid("重命名必须保留原目录和扩展名".into()));
    }
    let file = verified_source(source, expected_sha256, cancel)?;
    rename_open_file(&file, target, cancel)
}

#[cfg(windows)]
fn rename_error(error: std::io::Error) -> ToolError {
    match error.raw_os_error() {
        Some(80 | 183) => {
            ToolError::Invalid("同目录已存在同名文件，请换一个名称；未覆盖任何文件".into())
        }
        Some(32 | 33) => {
            ToolError::Invalid("图片正被其他程序占用，请关闭占用后重试；原文件名未改变".into())
        }
        Some(5) => {
            ToolError::Invalid("无法重命名：文件被占用或没有写入权限；原文件名未改变".into())
        }
        Some(2 | 3) => ToolError::Invalid("原文件已移动或删除，请刷新目录后重试".into()),
        _ => ToolError::Io(error),
    }
}

#[cfg(windows)]
fn verified_source(
    source: &Path,
    expected_sha256: &str,
    cancel: &Cancellation,
) -> light_image_tool::Result<std::fs::File> {
    use sha2::{Digest, Sha256};
    use std::{
        io::Read,
        os::windows::fs::{MetadataExt, OpenOptionsExt},
    };
    use windows_sys::Win32::{
        Foundation::GENERIC_READ,
        Storage::FileSystem::{
            DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        },
    };
    cancel.check()?;
    // Keep this handle through commit. Denying write/delete sharing blocks both
    // in-place writers and namespace replacement after the snapshot check.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .access_mode(GENERIC_READ | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(source)
        .map_err(rename_error)?;
    let before = file.metadata()?;
    if !before.is_file() || before.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ToolError::Invalid(
            "当前路径不是普通图片文件，暂不支持重命名".into(),
        ));
    }
    if before.len() > light_image_tool::pipeline::MAX_INPUT_BYTES {
        return Err(ToolError::Invalid(
            "原文件内容已变化或超过输入上限，请重新载入后再重命名".into(),
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut bytes = 0u64;
    loop {
        cancel.check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > light_image_tool::pipeline::MAX_INPUT_BYTES {
            return Err(ToolError::Invalid(
                "原文件内容已变化，请重新载入后再重命名".into(),
            ));
        }
        hash.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    if before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(expected_sha256)
    {
        return Err(ToolError::Invalid(
            "原文件内容已变化，请重新载入后再重命名；未修改文件名".into(),
        ));
    }
    cancel.check()?;
    Ok(file)
}

#[cfg(windows)]
fn rename_open_file(
    file: &std::fs::File,
    target: &Path,
    cancel: &Cancellation,
) -> light_image_tool::Result<PathBuf> {
    use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_RENAME_INFO, FileRenameInfo, SetFileInformationByHandle,
    };
    let absolute = std::path::absolute(target)?;
    let mut name: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if name.len() > 32767 || name.contains(&0) {
        return Err(ToolError::Invalid("文件路径包含无效字符".into()));
    }
    let name_bytes = name
        .len()
        .checked_mul(2)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| ToolError::Invalid("文件路径过长".into()))?;
    name.push(0);
    let bytes = std::mem::offset_of!(FILE_RENAME_INFO, FileName) + name.len() * 2;
    // u64 storage supplies FILE_RENAME_INFO's native alignment on 32/64-bit Windows.
    let mut storage = vec![0u64; bytes.div_ceil(std::mem::size_of::<u64>())];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
    }
    cancel.publish(|| {
        // The source is the verified handle; the kernel also rejects a destination
        // created after confirmation. No path-based reopen and no replacement flag.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle().cast(),
                FileRenameInfo,
                info.cast(),
                bytes as u32,
            )
        } == 0
        {
            return Err(rename_error(std::io::Error::last_os_error()));
        }
        Ok(target.to_path_buf())
    })
}

#[cfg(not(windows))]
fn rename_file(
    _source: &Path,
    _target: &Path,
    _expected_sha256: &str,
    cancel: &Cancellation,
) -> light_image_tool::Result<PathBuf> {
    cancel.check()?;
    Err(ToolError::Invalid(
        "当前平台暂不支持安全重命名，请使用文件管理器".into(),
    ))
}

impl ImageApp {
    pub(super) fn begin_rename(&mut self) {
        if self.rename.blocks_document()
            || self.loading
            || self.work.is_some()
            || self.shutting_down
            || self.pending_intent.is_some()
            || self.path_dialog
            || self.export_dialog
            || self.extract_dialog
            || self.ai_view.confirm.is_some()
            || self.error.is_some()
        {
            return;
        }
        if self.ai.is_busy() || self.evidence_task.is_some() || self.preview_task.is_some() {
            self.status = "AI、来源或预览仍在处理中，请等待完成后再重命名".into();
            self.notice_until = Some(Instant::now() + std::time::Duration::from_secs(5));
            return;
        }
        let Some(source) = self.current_file_path() else {
            self.status = "临时图片没有原文件，不能重命名；可导出新副本".into();
            return;
        };
        if self.unsaved() {
            self.error = Some("当前有未导出的修改，请先导出副本或放弃修改，再重命名原文件".into());
            return;
        }
        let Some(stem) = source.file_stem().and_then(|name| name.to_str()) else {
            self.error =
                Some("当前文件名无法在输入框中无损表示，请使用文件资源管理器重命名".into());
            return;
        };
        self.rename.dialog = Some(RenameDialog {
            stem: stem.to_owned(),
            extension: source
                .extension()
                .map_or_else(String::new, |ext| format!(".{}", ext.to_string_lossy())),
            source,
            error: None,
            focus: true,
        });
        self.more_open = false;
    }

    pub(super) fn rename_dialog(&mut self, ctx: &egui::Context) {
        let busy = self.rename.is_busy();
        let Some(dialog) = self.rename.dialog.as_mut() else {
            return;
        };
        if dialog.focus {
            egui::Popup::close_all(ctx);
        }
        let mut confirm = !busy
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        let mut cancel = false;
        let response = egui::Modal::new(egui::Id::new("rename-current-image")).show(ctx, |ui| {
            ui.set_width(440.0_f32.min((ctx.viewport_rect().width() - 48.0).max(1.0)));
            ui.heading("重命名图片文件");
            ui.label("直接修改磁盘上的文件名；目录和扩展名保持不变。");
            ui.label("文件主名");
            let input = ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut dialog.stem)
                    .id(egui::Id::new("rename-file-stem"))
                    .desired_width(ui.available_width()),
            );
            if dialog.focus && !busy {
                input.request_focus();
                dialog.focus = false;
            }
            ui.small(if dialog.extension.is_empty() {
                "原文件没有扩展名".to_owned()
            } else {
                format!("保留扩展名：{}", dialog.extension)
            });
            if let Some(error) = &dialog.error {
                ui.colored_label(crate::app_theme::palette(ui.ctx()).error, error);
            }
            if busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在重命名，请稍候…");
                });
            }
            ui.horizontal(|ui| {
                confirm |= ui
                    .add_enabled(!busy, egui::Button::new("确认 · Enter"))
                    .clicked();
                cancel = ui
                    .add_enabled(!busy, egui::Button::new("取消 · Esc"))
                    .clicked();
            });
        });
        if busy {
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        } else if cancel || response.should_close() {
            self.rename.dialog = None;
        } else if confirm {
            self.start_rename(ctx);
        }
    }

    fn start_rename(&mut self, ctx: &egui::Context) {
        if self.rename.is_busy() {
            return;
        }
        let Some(dialog) = &self.rename.dialog else {
            return;
        };
        if self.unsaved()
            || self.loading
            || self.work.is_some()
            || self.ai.is_busy()
            || self.evidence_task.is_some()
            || self.preview_task.is_some()
            || self.current_file_path().as_ref() != Some(&dialog.source)
        {
            if let Some(dialog) = &mut self.rename.dialog {
                dialog.error = Some("当前文档状态已改变，请取消后重新重命名".into());
            }
            return;
        }
        let target = match rename_target(&dialog.source, &dialog.stem) {
            Ok(target) => target,
            Err(error) => {
                self.rename.dialog.as_mut().unwrap().error = Some(error.to_string());
                return;
            }
        };
        if target == dialog.source {
            self.rename.dialog = None;
            self.status = "文件名未改变".into();
            return;
        }
        let Some(ticket) = self.rename.next_ticket.checked_add(1) else {
            self.rename.dialog.as_mut().unwrap().error =
                Some("重命名操作标识已耗尽，请重启应用".into());
            return;
        };
        self.rename.next_ticket = ticket;
        let source = dialog.source.clone();
        let operation_source = source.clone();
        let expected_sha256 = self.loaded.as_ref().unwrap().source_sha256.clone();
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let (tx, receiver) = mpsc::sync_channel(1);
        let wake = ctx.clone();
        let thread = std::thread::spawn(move || {
            let result = protected(
                || rename_file(&operation_source, &target, &expected_sha256, &token),
                "重命名后台异常，请检查文件后重试",
            );
            let _ = tx.try_send(result);
            wake.request_repaint();
        });
        self.rename.dialog.as_mut().unwrap().error = None;
        self.rename.job = Some(RenameJob {
            ticket,
            generation: self.generation,
            source,
            cancel,
            receiver,
            thread: Some(thread),
        });
        ctx.request_repaint();
    }

    pub(super) fn poll_rename(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.rename.job else {
            return;
        };
        let mut result = match job.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => Err(ToolError::Invalid(
                "重命名后台意外结束，请检查文件后重试".into(),
            )),
        };
        let mut job = self.rename.job.take().unwrap();
        if let Some(thread) = job.thread.take()
            && thread.join().is_err()
        {
            result = Err(ToolError::Invalid(
                "重命名后台异常，请检查文件后重试".into(),
            ));
        }
        if job.ticket != self.rename.next_ticket {
            return;
        }
        match result {
            Ok(target) => {
                self.rename.dialog = None;
                if job.generation == self.generation
                    && matches!(self.source, DocumentSource::File)
                    && self
                        .loaded
                        .as_ref()
                        .is_some_and(|image| image.path == job.source)
                {
                    self.apply_renamed_path(&job.source, &target, ctx);
                } else {
                    self.refresh_browser();
                    self.status = format!(
                        "文件已重命名为 {}；当前文档已改变，目录已请求刷新",
                        target.display()
                    );
                }
            }
            Err(error) => {
                if let Some(dialog) = &mut self.rename.dialog {
                    dialog.error = Some(error.to_string());
                    dialog.focus = true;
                }
            }
        }
        ctx.request_repaint();
    }

    fn apply_renamed_path(&mut self, source: &Path, target: &Path, ctx: &egui::Context) {
        if let Some(image) = &mut self.loaded {
            image.path = target.to_path_buf();
        }
        for path in &mut self.siblings {
            if path.as_path() == source {
                *path = target.to_path_buf();
            }
        }
        if !self.siblings.iter().any(|path| path == target) {
            if self.siblings.len() >= browsing::MAX_DIRECTORY_IMAGES {
                self.siblings.pop();
            }
            self.siblings.push(target.to_path_buf());
        }
        if let Some(metadata) = self.ai_view.metadata.remove(source) {
            self.ai_view.metadata.insert(target.to_path_buf(), metadata);
        }
        self.requested_path = Some(target.to_path_buf());
        self.directory_path = Some(browsing::directory(target));
        self.position = self
            .siblings
            .iter()
            .position(|path| path == target)
            .unwrap_or(0);
        self.locate_current = true;
        self.thumbnail_worker.cancel();
        self.thumbnail_busy = None;
        for entry in &mut self.thumbnail_cache {
            if entry.path == source {
                entry.path = target.to_path_buf();
            }
        }
        self.loader.clear_cache();
        self.ai_sync_scope();
        self.directory_generation += 1;
        self.directory_scanning = true;
        self.directory_warning = None;
        self.directory_worker.request(BrowserRequest {
            generation: self.directory_generation,
            path: target.to_path_buf(),
            cancel: Cancellation::default(),
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "gloamere-image · {}",
            target.file_name().unwrap_or_default().to_string_lossy()
        )));
        self.status = format!(
            "已重命名：{}",
            target.file_name().unwrap_or_default().to_string_lossy()
        );
        self.notice_until = Some(Instant::now() + std::time::Duration::from_secs(5));
        ctx.request_repaint_after(std::time::Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_reserved_and_invalid_stems_are_rejected() {
        for stem in [
            "",
            " ",
            ".",
            "..",
            "name.",
            "name ",
            "a/b",
            "a\\b",
            "a:b",
            "a*b",
            "a?b",
            "a\"b",
            "a<b",
            "a>b",
            "a|b",
            "a\0b",
            "a\nb",
            "CON",
            "con.txt",
            "PRN",
            "AUX",
            "NUL",
            "COM1",
            "com9.more",
            "LPT1",
            "LPT9",
            "COM¹",
            "LPT²",
            "LPT³",
            "CON .part",
            "CONIN$",
            "CONOUT$",
        ] {
            assert!(
                rename_target(Path::new("original.PNG"), stem).is_err(),
                "{stem:?}"
            );
        }
        assert!(rename_target(Path::new("original.PNG"), &"长".repeat(252)).is_err());
    }

    #[test]
    fn unicode_spaces_and_dots_preserve_directory_and_original_extension() {
        let source = Path::new("pictures").join("original.part.PNG");
        assert_eq!(
            rename_target(&source, "新的 名称.v2").unwrap(),
            Path::new("pictures").join("新的 名称.v2.PNG")
        );
        assert_eq!(
            rename_target(Path::new("without-extension"), "新名称").unwrap(),
            PathBuf::from("新名称")
        );
    }

    #[test]
    fn cancelled_rename_does_not_touch_either_path() {
        let token = Cancellation::default();
        token.cancel();
        assert!(matches!(
            rename_file(
                Path::new("missing.png"),
                Path::new("target.png"),
                "",
                &token
            ),
            Err(ToolError::Cancelled)
        ));
    }

    fn file_app() -> (tempfile::TempDir, ImageApp, egui::Context) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("original.png");
        image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "rename-test".into(), None, None);
        let loaded = light_image_tool::pipeline::load(&path, &Cancellation::default()).unwrap();
        app.pixels = Some(loaded.pixels.clone());
        app.loaded = Some(loaded);
        app.prepare_browser(&path);
        app.refresh_texture(&ctx);
        (directory, app, ctx)
    }

    #[test]
    fn dirty_and_temporary_documents_cannot_begin_rename() {
        let (_directory, mut app, _) = file_app();
        app.edited = true;
        app.begin_rename();
        assert!(!app.rename.is_open() && !app.rename.is_busy());
        assert!(app.error.as_ref().unwrap().contains("未导出"));
        app.error = None;
        app.edited = false;
        app.source = DocumentSource::Clipboard;
        app.begin_rename();
        assert!(!app.rename.blocks_document());
        assert!(app.status.contains("没有原文件"));
    }

    #[test]
    fn identity_sync_preserves_pixels_textures_history_and_load_generation() {
        let (_directory, mut app, ctx) = file_app();
        let source = app.loaded.as_ref().unwrap().path.clone();
        let target = rename_target(&source, "新的 名称").unwrap();
        let pixels = app.pixels.as_ref().unwrap().clone();
        let original = app.loaded.as_ref().unwrap().pixels.clone();
        let ids: Vec<_> = app.tiles.iter().map(|tile| tile.texture.id()).collect();
        let generation = app.generation;
        let misses = app.loader.cache_stats().misses;
        app.undo.push(pixels.clone());
        app.exported_pixels = Some(Arc::downgrade(&pixels));
        app.ai_view.metadata.insert(
            source.clone(),
            browsing::FileMetadata {
                bytes: Some(42),
                regular_file: true,
                ..Default::default()
            },
        );
        app.thumbnail_cache.push(ThumbnailEntry {
            path: source.clone(),
            texture: None,
            used: 0,
            retry_attempts: 0,
            retry_after: None,
        });
        app.apply_renamed_path(&source, &target, &ctx);
        assert_eq!(app.loaded.as_ref().unwrap().path, target);
        assert_eq!(app.current_file_path().as_ref(), Some(&target));
        assert_eq!(app.requested_path.as_ref(), Some(&target));
        assert!(!app.siblings.contains(&source));
        assert_eq!(app.siblings[app.position], target);
        assert_eq!(app.ai_view.metadata.get(&target).unwrap().bytes, Some(42));
        assert!(!app.ai_view.metadata.contains_key(&source));
        assert_eq!(app.thumbnail_cache[0].path, target);
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &pixels));
        assert!(Arc::ptr_eq(&app.loaded.as_ref().unwrap().pixels, &original));
        assert_eq!(
            app.tiles
                .iter()
                .map(|tile| tile.texture.id())
                .collect::<Vec<_>>(),
            ids
        );
        assert_eq!(app.generation, generation);
        assert_eq!(app.loader.cache_stats().misses, misses);
        assert!(!app.loading && !app.loader.is_busy());
        assert_eq!(app.undo.len(), 1);
        assert!(
            app.exported_pixels
                .as_ref()
                .unwrap()
                .ptr_eq(&Arc::downgrade(&pixels))
        );
    }

    #[cfg(windows)]
    fn sha256(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(bytes))
    }

    #[cfg(windows)]
    #[test]
    fn native_rename_refuses_existing_destination_without_overwriting() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.png");
        let target = directory.path().join("target.png");
        std::fs::write(&source, b"source bytes").unwrap();
        std::fs::write(&target, b"target bytes").unwrap();
        assert!(
            rename_file(
                &source,
                &target,
                &sha256(b"source bytes"),
                &Cancellation::default()
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"source bytes");
        assert_eq!(std::fs::read(&target).unwrap(), b"target bytes");
    }

    #[cfg(windows)]
    #[test]
    fn native_unicode_and_case_only_rename_keep_file_contents() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("Original.PNG");
        std::fs::write(&source, b"unchanged image bytes").unwrap();
        let case_target = rename_target(&source, "original").unwrap();
        rename_file(
            &source,
            &case_target,
            &sha256(b"unchanged image bytes"),
            &Cancellation::default(),
        )
        .unwrap();
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .any(|entry| entry.unwrap().file_name() == "original.PNG")
        );
        let unicode_target = rename_target(&case_target, "新的 名称.v2").unwrap();
        rename_file(
            &case_target,
            &unicode_target,
            &sha256(b"unchanged image bytes"),
            &Cancellation::default(),
        )
        .unwrap();
        assert!(!case_target.exists());
        assert_eq!(
            std::fs::read(unicode_target).unwrap(),
            b"unchanged image bytes"
        );
    }

    #[cfg(windows)]
    #[test]
    fn occupied_file_returns_error_and_keeps_original_name() {
        use std::os::windows::fs::OpenOptionsExt;
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("locked.png");
        let target = directory.path().join("new.png");
        std::fs::write(&source, b"locked bytes").unwrap();
        let _held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&source)
            .unwrap();
        let error = rename_file(
            &source,
            &target,
            &sha256(b"locked bytes"),
            &Cancellation::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("占用"));
        assert!(source.exists() && !target.exists());
    }

    #[cfg(windows)]
    #[test]
    fn external_same_name_replacement_is_rejected_before_rename() {
        let (_directory, app, _) = file_app();
        let loaded = app.loaded.as_ref().unwrap();
        let source = loaded.path.clone();
        let target = rename_target(&source, "new-name").unwrap();
        std::fs::write(&source, b"replacement file with different bytes").unwrap();
        let error = rename_file(
            &source,
            &target,
            &loaded.source_sha256,
            &Cancellation::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("内容已变化"));
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"replacement file with different bytes"
        );
        assert!(!target.exists());
    }

    #[cfg(windows)]
    #[test]
    fn verified_handle_blocks_source_writes_and_replacement_until_commit() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.png");
        let target = directory.path().join("renamed.png");
        let displaced = directory.path().join("displaced.png");
        std::fs::write(&source, b"displayed bytes").unwrap();
        let file = verified_source(
            &source,
            &sha256(b"displayed bytes"),
            &Cancellation::default(),
        )
        .unwrap();
        assert!(std::fs::write(&source, b"different bytes").is_err());
        assert!(std::fs::rename(&source, &displaced).is_err());
        rename_open_file(&file, &target, &Cancellation::default()).unwrap();
        drop(file);
        assert!(!source.exists() && !displaced.exists());
        assert_eq!(std::fs::read(&target).unwrap(), b"displayed bytes");
    }

    #[cfg(windows)]
    #[test]
    fn background_rename_publishes_identity_without_redecoding() {
        let (_directory, mut app, ctx) = file_app();
        let source = app.loaded.as_ref().unwrap().path.clone();
        let pixels = app.pixels.as_ref().unwrap().clone();
        let generation = app.generation;
        app.begin_rename();
        app.rename.dialog.as_mut().unwrap().stem = "新图片 name".into();
        app.start_rename(&ctx);
        assert!(app.rename.blocks_document() && app.rename.is_busy());
        let started = Instant::now();
        while app.rename.is_busy() {
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            app.poll_rename(&ctx);
            std::thread::yield_now();
        }
        assert!(!app.rename.is_open());
        let target = rename_target(&source, "新图片 name").unwrap();
        assert_eq!(app.loaded.as_ref().unwrap().path, target);
        assert!(!source.exists() && target.exists());
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &pixels));
        assert_eq!(app.generation, generation);
        assert!(!app.loading && !app.loader.is_busy());
    }
}
