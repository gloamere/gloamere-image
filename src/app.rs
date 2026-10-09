use crate::viewer_ui::{self, ActionButton, CompactAction, Icon, ToolbarLayout};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use light_image_tool::{
    Cancellation, ToolError,
    browsing::{self, DirectoryListing, Request as BrowserRequest, Worker as BrowserWorker},
    edit::{self, EditOp},
    export::{self, ExportFormat, ExportOptions},
    pipeline::LoadedImage,
    provenance::{self, CheckState, EvidenceCache, EvidenceReport, EvidenceState},
    settings::{self, CanvasBackground, Preferences},
    worker::{LoadRequest, Loader},
};
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
    time::Instant,
};

fn accessible_path_input(ui: &mut egui::Ui, text: &mut String, name: &str, width: f32) {
    let id = egui::Id::new(name);
    let mut too_long = false;
    ui.ctx().input_mut(|input| {
        input.consume_accesskit_action_requests(id, |request| {
            if request.action == egui::accesskit::Action::SetValue
                && let Some(egui::accesskit::ActionData::Value(value)) = &request.data
            {
                if value.encode_utf16().count() <= 32767 {
                    *text = value.to_string();
                } else {
                    too_long = true;
                }
                true
            } else {
                false
            }
        })
    });
    if too_long {
        ui.colored_label(Color32::from_rgb(178, 45, 45), "路径超过 Windows 长度限制");
    }
    ui.add(egui::TextEdit::singleline(text).id(id).desired_width(width));
}

enum WorkResult {
    Edited(light_image_tool::Result<light_image_tool::memory::Pixels>),
    Exported(light_image_tool::Result<PathBuf>),
}
fn protected<T>(
    operation: impl FnOnce() -> light_image_tool::Result<T>,
    message: &str,
) -> light_image_tool::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
        .unwrap_or_else(|_| Err(ToolError::Invalid(message.into())))
}
struct PreviewTask {
    generation: u64,
    matte: [u8; 3],
    cancel: Cancellation,
    receiver: mpsc::Receiver<light_image_tool::Result<light_image_tool::memory::Pixels>>,
}
struct Work {
    generation: u64,
    cancel: Cancellation,
    receiver: mpsc::Receiver<WorkResult>,
    // Only export owns files. Keep its thread until cleanup/publication has completed.
    export_thread: Option<std::thread::JoinHandle<()>>,
}
impl Work {
    fn spawn_export(
        generation: u64,
        operation: impl FnOnce(Cancellation) -> light_image_tool::Result<PathBuf> + Send + 'static,
    ) -> Self {
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let (tx, receiver) = mpsc::sync_channel(1);
        let export_thread = std::thread::spawn(move || {
            let result = protected(|| operation(token), "导出后台异常，原图未修改");
            // Exactly one result, no acknowledgement and no UI/context lock.
            let _ = tx.try_send(WorkResult::Exported(result));
        });
        Self {
            generation,
            cancel,
            receiver,
            export_thread: Some(export_thread),
        }
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.export_thread.take()
            && thread.join().is_err()
        {
            // Never panic during renderer unwinding. The worker has still exited
            // and unwound its owned temporary before join returns.
            log::error!("export worker panicked during shutdown");
        }
    }
}
struct ProvenanceTask {
    generation: u64,
    cancel: Cancellation,
    receiver: mpsc::Receiver<light_image_tool::Result<EvidenceReport>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EvidenceStage {
    NotStarted,
    Running,
    Complete,
    Cancelled,
    Failed,
}
fn evidence_check_label(state: &EvidenceState, check: &CheckState) -> &'static str {
    match check {
        CheckState::Passed => "通过",
        CheckState::Failed => "失败",
        CheckState::NotChecked if matches!(state, EvidenceState::NoCredential) => {
            "不适用（未发现凭证）"
        }
        CheckState::NotChecked => "未核验",
    }
}
fn evidence_stage_label(stage: EvidenceStage) -> &'static str {
    match stage {
        EvidenceStage::NotStarted => "尚未核验",
        EvidenceStage::Running => "正在离线核验…",
        EvidenceStage::Complete => "离线核验已结束",
        EvidenceStage::Cancelled => "核验已取消，可重新核验",
        EvidenceStage::Failed => "核验任务失败，可重新核验",
    }
}
struct Tile {
    _reservation: light_image_tool::memory::Reservation,
    texture: egui::TextureHandle,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}
pub struct Smoke {
    pub directory: PathBuf,
    pub scale: f32,
    pub show_info: bool,
    pub show_more: bool,
    pub show_edit: bool,
    pub show_export: bool,
    pub show_error: bool,
    pub jpeg_preview: bool,
    pub crop_preview: bool,
    pub background: Option<CanvasBackground>,
    pub hide_list: bool,
    pub destroy_device: bool,
    pub destroy_during_export: bool,
    pub cjk_fallback: bool,
    fault_requested: bool,
    pub workflow_image: Option<PathBuf>,
    pub workflow_opens: u16,
    pub workflow_delay_ms: u64,
    workflow_not_before: Instant,
    workflow_stage: u16,
    workflow_frame: u64,
    workflow_original: Option<(PathBuf, String, [u32; 2])>,
    workflow_records: Vec<serde_json::Value>,
    workflow_result: Option<serde_json::Value>,
    state_prepared: bool,
    requested: bool,
    pub idle_seconds: u64,
    idle_start: Option<(Instant, u64)>,
    idle_frames: Option<u64>,
    idle_wake: Arc<std::sync::atomic::AtomicBool>,
    idle_focus_start: Option<bool>,
    idle_lost_focus: bool,
}
impl Smoke {
    pub fn new(directory: PathBuf, scale: f32, show_info: bool) -> Self {
        Self {
            directory,
            scale,
            show_info,
            show_more: false,
            show_edit: false,
            show_export: false,
            show_error: false,
            jpeg_preview: false,
            crop_preview: false,
            background: None,
            hide_list: true,
            destroy_device: false,
            destroy_during_export: false,
            cjk_fallback: false,
            fault_requested: false,
            workflow_image: None,
            workflow_opens: 4,
            workflow_delay_ms: 0,
            workflow_not_before: Instant::now(),
            workflow_stage: 0,
            workflow_frame: 0,
            workflow_original: None,
            workflow_records: Vec::new(),
            workflow_result: None,
            state_prepared: false,
            requested: false,
            idle_seconds: 0,
            idle_start: None,
            idle_frames: None,
            idle_wake: Arc::default(),
            idle_focus_start: None,
            idle_lost_focus: false,
        }
    }
}

struct ThumbnailEntry {
    path: PathBuf,
    texture: Option<egui::TextureHandle>,
    used: u64,
    retry_attempts: u8,
    retry_after: Option<Instant>,
}

pub struct ImageApp {
    brand_texture: egui::TextureHandle,
    loader: Loader,
    generation: u64,
    load_cancel: Cancellation,
    loading: bool,
    loaded: Option<LoadedImage>,
    pixels: Option<Arc<light_image_tool::memory::Pixels>>,
    tiles: Vec<Tile>,
    undo: Vec<Arc<light_image_tool::memory::Pixels>>,
    edited: bool,
    siblings: Vec<PathBuf>,
    directory_worker: BrowserWorker<DirectoryListing>,
    directory_generation: u64,
    directory_path: Option<PathBuf>,
    directory_scanning: bool,
    directory_warning: Option<String>,
    requested_path: Option<PathBuf>,
    thumbnail_worker: BrowserWorker<image::RgbaImage>,
    thumbnail_sequence: u64,
    thumbnail_busy: Option<(u64, PathBuf)>,
    thumbnail_cache: Vec<ThumbnailEntry>,
    locate_current: bool,
    list_viewport_rect: Option<Rect>,
    selected_list_row_rect: Option<Rect>,
    preferences: Preferences,
    settings_path: Option<PathBuf>,
    settings_warning: Option<String>,
    position: usize,
    zoom: f32,
    fit: bool,
    pan: Vec2,
    editing: bool,
    info_open: bool,
    fullscreen: bool,
    more_open: bool,
    restore_more_focus: bool,
    more_trigger_release_blocked: bool,
    more_button_rect: Option<Rect>,
    toolbar_layout: Option<ToolbarLayout>,
    menu_rect: Option<Rect>,
    #[cfg(test)]
    menu_list_rect: Option<Rect>,
    #[cfg(test)]
    menu_edit_rect: Option<Rect>,
    current_has_alpha: bool,
    preview_matte: Option<[u8; 3]>,
    preview_task: Option<PreviewTask>,
    preview_texture: Option<egui::TextureHandle>,
    preview_reservation: Option<light_image_tool::memory::Reservation>,
    crop: [u32; 4],
    crop_selecting: bool,
    crop_anchor: Option<[f64; 2]>,
    canvas_image_rect: Option<Rect>,
    #[cfg(test)]
    crop_apply_rect: Option<Rect>,
    resize: [u32; 2],
    keep_ratio: bool,
    format: ExportFormat,
    quality: u8,
    matte: [u8; 3],
    export_dialog: bool,
    export_generation: Option<u64>,
    export_name: String,
    path_dialog: bool,
    open_path: String,
    status: String,
    notice_until: Option<Instant>,
    error: Option<String>,
    work: Option<Work>,
    evidence: Option<EvidenceReport>,
    evidence_task: Option<ProvenanceTask>,
    evidence_attempted: bool,
    evidence_stage: EvidenceStage,
    evidence_cache: EvidenceCache,
    adapter: String,
    start: Instant,
    frames: u64,
    smoke: Option<Smoke>,
    _browser_reservation: light_image_tool::memory::Reservation,
    _ui_reservation: light_image_tool::memory::Reservation,
}

pub fn configure(ctx: &egui::Context) {
    let candidates = if cfg!(windows) {
        vec![
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\simhei.ttf",
        ]
    } else {
        vec!["/System/Library/Fonts/PingFang.ttc"]
    };
    configure_with_font_candidates(ctx, &candidates);
}
fn configure_with_font_candidates(ctx: &egui::Context, candidates: &[&str]) -> Option<String> {
    let mut fonts = egui::FontDefinitions::default();
    let mut selected = None;
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "cjk-system".into(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("cjk-system".into());
            }
            log::info!("CJK font loaded from {path}");
            selected = Some(path.to_string());
            break;
        }
    }
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::ThemePreference::Light);
    ctx.set_visuals(egui::Visuals::light());
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(10.0, 6.0);
        s.spacing.interact_size.y = 34.0;
        s.text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(13.0));
        s.text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
        s.text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
        s.visuals.weak_text_color = Some(Color32::from_gray(104));
        s.visuals.panel_fill = Color32::WHITE;
        s.visuals.selection.bg_fill = Color32::from_rgb(217, 234, 248);
        s.visuals.selection.stroke.color = Color32::from_rgb(33, 84, 128);
    });
    selected
}

impl ImageApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial: Option<PathBuf>,
        smoke: Option<Smoke>,
    ) -> Self {
        if cfg!(windows) && smoke.as_ref().is_some_and(|s| s.cjk_fallback) {
            let missing = "C:\\Windows\\Fonts\\__light_image_qa_missing__.ttf";
            let selected = configure_with_font_candidates(
                &cc.egui_ctx,
                &[missing, "C:\\Windows\\Fonts\\simhei.ttf"],
            );
            let report = serde_json::json!({"controlled_primary_unavailable":!std::path::Path::new(missing).exists(),"selected_font":selected,"native_input_tested":false,"all_cjk_fonts_absent_tested":false});
            let _ = std::fs::write(
                smoke.as_ref().unwrap().directory.join("font-fallback.json"),
                serde_json::to_vec_pretty(&report).unwrap_or_default(),
            );
        } else {
            configure(&cc.egui_ctx);
        }
        if let Some(smoke) = &smoke {
            cc.egui_ctx.set_zoom_factor(smoke.scale);
        }
        let adapter = cc
            .wgpu_render_state
            .as_ref()
            .map(|r| {
                let info = r.adapter.get_info();
                format!("{} / {:?}", info.name, info.backend)
            })
            .unwrap_or_else(|| "GPU 未初始化".into());
        log::info!("render adapter: {adapter}");
        if let Some(smoke) = &smoke
            && smoke.destroy_device
            && let Some(state) = &cc.wgpu_render_state
        {
            let directory = smoke.directory.clone();
            let wake = cc.egui_ctx.clone();
            state.device.set_device_lost_callback(move |reason,message| {
                let report = serde_json::json!({"callback_observed":true,"reason":format!("{reason:?}"),"message":message,"controlled_logical_device_destroy":true,"real_driver_loss_tested":false});
                if let Err(error) = std::fs::write(directory.join("gpu-loss.json"),serde_json::to_vec_pretty(&report).unwrap_or_default()) {log::error!("GPU diagnostic write failed: {error}");}
                log::error!("controlled GPU device callback: {reason:?}");
                wake.request_repaint();
            });
        }
        let persistent = smoke.is_none();
        let mut app = Self::from_context(cc.egui_ctx.clone(), adapter, initial, smoke);
        if persistent {
            app.settings_path = settings::user_settings_path();
            if let Some(path) = &app.settings_path {
                match settings::load(path) {
                    Ok(preferences) => app.preferences = preferences,
                    Err(ToolError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => app.settings_warning = Some(error.to_string()),
                }
            }
        }
        app
    }
    fn cancel_background(&mut self) {
        self.directory_worker.cancel();
        self.thumbnail_worker.cancel();
        self.load_cancel.cancel();
        if let Some(work) = &self.work {
            work.cancel.cancel();
        }
        if let Some(task) = &self.evidence_task {
            task.cancel.cancel();
        }
        if let Some(task) = &self.preview_task {
            task.cancel.cancel();
        }
    }
    fn shutdown_background(&mut self) {
        self.cancel_background();
        // Finish file-owning work before returning to main's process-exit boundary.
        // Work also owns this join when poll removes a completed/stale/failed task.
        drop(self.work.take());
    }
    fn from_context(
        ctx: egui::Context,
        adapter: String,
        initial: Option<PathBuf>,
        smoke: Option<Smoke>,
    ) -> Self {
        let mut app = Self {
            brand_texture: viewer_ui::load_brand_texture(&ctx),
            loader: Loader::new(ctx.clone()),
            generation: 0,
            load_cancel: Cancellation::default(),
            loading: false,
            loaded: None,
            pixels: None,
            tiles: Vec::new(),
            undo: Vec::new(),
            edited: false,
            siblings: Vec::new(),
            directory_worker: BrowserWorker::new("directory-scanner", ctx.clone(), browsing::scan),
            directory_generation: 0,
            directory_path: None,
            directory_scanning: false,
            directory_warning: None,
            requested_path: None,
            thumbnail_worker: BrowserWorker::new(
                "thumbnail-loader",
                ctx.clone(),
                browsing::thumbnail,
            ),
            thumbnail_sequence: 0,
            thumbnail_busy: None,
            thumbnail_cache: Vec::new(),
            locate_current: true,
            list_viewport_rect: None,
            selected_list_row_rect: None,
            preferences: Preferences::default(),
            settings_path: None,
            settings_warning: None,
            position: 0,
            zoom: 1.0,
            fit: true,
            pan: Vec2::ZERO,
            editing: false,
            info_open: false,
            fullscreen: false,
            more_open: false,
            restore_more_focus: false,
            more_trigger_release_blocked: false,
            more_button_rect: None,
            toolbar_layout: None,
            menu_rect: None,
            #[cfg(test)]
            menu_list_rect: None,
            #[cfg(test)]
            menu_edit_rect: None,
            current_has_alpha: false,
            preview_matte: None,
            preview_task: None,
            preview_texture: None,
            preview_reservation: None,
            crop: [0, 0, 1, 1],
            crop_selecting: false,
            crop_anchor: None,
            canvas_image_rect: None,
            #[cfg(test)]
            crop_apply_rect: None,
            resize: [1, 1],
            keep_ratio: true,
            format: ExportFormat::Png,
            quality: 92,
            matte: [255; 3],
            export_dialog: false,
            export_generation: None,
            export_name: String::new(),
            path_dialog: false,
            open_path: String::new(),
            status: "本地处理 · 不上传图片".into(),
            notice_until: None,
            error: None,
            work: None,
            evidence: None,
            evidence_task: None,
            evidence_attempted: false,
            evidence_stage: EvidenceStage::NotStarted,
            evidence_cache: EvidenceCache::default(),
            adapter,
            start: Instant::now(),
            frames: 0,
            smoke,
            _browser_reservation: light_image_tool::memory::reserve(16 * 1024 * 1024)
                .expect("bounded browser allocation budget"),
            _ui_reservation: light_image_tool::memory::reserve(64 * 1024 * 1024)
                .expect("UI initial allocation budget"),
        };
        if let Some(smoke) = &app.smoke {
            if let Some(background) = smoke.background {
                app.preferences.background = background;
            }
            app.preferences.list_visible = !smoke.hide_list;
            app.more_open = smoke.show_more;
        }
        if app.smoke.as_ref().is_some_and(|s| s.show_info) {
            app.info_open = true;
        }
        if app.smoke.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if let Some(path) = initial {
            app.open(path);
        }
        app
    }

    fn prepare_browser(&mut self, path: &PathBuf) {
        let parent = browsing::directory(path);
        if self.directory_path.as_ref() != Some(&parent) {
            self.directory_generation += 1;
            self.directory_path = Some(parent);
            self.directory_scanning = true;
            self.directory_warning = None;
            self.siblings = vec![path.clone()];
            self.position = 0;
            self.thumbnail_worker.cancel();
            self.thumbnail_busy = None;
            self.thumbnail_cache.clear();
            self.directory_worker.request(BrowserRequest {
                generation: self.directory_generation,
                path: path.clone(),
                cancel: Cancellation::default(),
            });
        }
        if !self.siblings.contains(path) {
            match browsing::ensure_current(std::mem::take(&mut self.siblings), path) {
                Ok(paths) => self.siblings = paths,
                Err(error) => self.directory_warning = Some(error.to_string()),
            }
        }
        self.requested_path = Some(path.clone());
        if let Some(position) = self.siblings.iter().position(|p| p == path) {
            self.position = position;
        }
        self.locate_current = true;
    }
    fn restore_browser_selection(&mut self) {
        if let Some(path) = self.loaded.as_ref().map(|image| image.path.clone()) {
            self.prepare_browser(&path);
        }
    }
    fn open(&mut self, path: PathBuf) {
        self.prepare_browser(&path);
        self.crop_selecting = false;
        self.crop_anchor = None;
        self.close_export();
        self.undo.clear();
        self.load_cancel.cancel();
        self.load_cancel = Cancellation::default();
        if let Some(work) = &self.work {
            work.cancel.cancel();
        }
        if let Some(task) = &self.evidence_task {
            task.cancel.cancel();
        }
        self.generation += 1;
        self.loading = true;
        self.error = None;
        self.status = format!(
            "正在打开 {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        self.loader.request(LoadRequest {
            generation: self.generation,
            path,
            cancel: self.load_cancel.clone(),
        });
    }
    fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("gloamere-image · 打开图片")
            .add_filter("JPEG / PNG", &["jpg", "jpeg", "png"])
            .pick_file()
        {
            self.open(path);
        }
    }
    fn close_export(&mut self) {
        self.export_dialog = false;
        self.export_generation = None;
        self.preview_matte = None;
        self.preview_texture = None;
        self.preview_reservation = None;
        if let Some(task) = &self.preview_task {
            task.cancel.cancel();
        }
    }
    fn resident_estimate(&self) -> u64 {
        let mut unique: Vec<&Arc<light_image_tool::memory::Pixels>> = Vec::new();
        for pixels in self
            .loaded
            .as_ref()
            .map(|l| &l.pixels)
            .into_iter()
            .chain(self.pixels.iter())
            .chain(self.undo.iter())
        {
            if !unique.iter().any(|p| Arc::ptr_eq(p, pixels)) {
                unique.push(pixels);
            }
        }
        let images: u64 = unique.iter().map(|p| p.as_raw().len() as u64).sum();
        let textures: u64 = self
            .tiles
            .iter()
            .map(|t| u64::from(t.w) * u64::from(t.h) * 8)
            .sum();
        images + textures + 64 * 1024 * 1024 + self._browser_reservation.bytes()
    }
    fn evict_history(&mut self, extra: u64) {
        while !self.undo.is_empty()
            && light_image_tool::memory::in_use().saturating_add(extra)
                > light_image_tool::memory::OPERATION_BUDGET
        {
            self.undo.remove(0);
        }
    }
    fn memory_breakdown(&self) -> serde_json::Value {
        let original = self.loaded.as_ref().map_or(0, |l| l.pixels.as_raw().len());
        let current = self.pixels.as_ref().map_or(0, |p| p.as_raw().len());
        let shared = self
            .loaded
            .as_ref()
            .zip(self.pixels.as_ref())
            .is_some_and(|(l, p)| Arc::ptr_eq(&l.pixels, p));
        let texture_bytes: u64 = self
            .tiles
            .iter()
            .map(|t| u64::from(t.w) * u64::from(t.h) * 4)
            .sum();
        serde_json::json!({
            "original_rgba_bytes": original,
            "current_rgba_bytes": current,
            "original_and_current_share_one_allocation": shared,
            "history_entries": self.undo.len(),
            "history_referenced_bytes_not_deduplicated": self.undo.iter().map(|p| p.as_raw().len()).sum::<usize>(),
            "texture_cpu_colorimage_estimate_bytes": texture_bytes,
            "texture_gpu_estimate_bytes": texture_bytes,
            "ui_fixed_reservation_bytes": 64 * 1024 * 1024,
            "browser_fixed_reservation_bytes": self._browser_reservation.bytes(),
            "thumbnail_cpu_gpu_estimate_bytes": self.thumbnail_cache.iter().filter_map(|entry| entry.texture.as_ref()).map(|texture| texture.size()[0] as u64 * texture.size()[1] as u64 * 8).sum::<u64>(),
            "preview_reservation_bytes": self.preview_reservation.as_ref().map_or(0, |r| r.bytes()),
            "total_resident_logical_estimate_bytes": self.resident_estimate(),
            "owned_total_bytes": light_image_tool::memory::in_use(),
            "owned_peak_bytes": light_image_tool::memory::peak(),
            "driver_allocator_and_delayed_resource_release": "not measured separately; logical estimates are not process memory limits"
        })
    }
    fn update_preview(&mut self, ctx: &egui::Context) {
        if !self.export_dialog || self.format != ExportFormat::Jpeg || !self.current_has_alpha {
            self.preview_matte = None;
            self.preview_texture = None;
            self.preview_reservation = None;
            if let Some(task) = &self.preview_task {
                task.cancel.cancel();
            }
            return;
        }
        if self.preview_matte == Some(self.matte) {
            return;
        }
        self.preview_texture = None;
        self.preview_reservation = None;
        if let Some(task) = &self.preview_task {
            if task.matte != self.matte {
                task.cancel.cancel();
            }
            return;
        }
        let Some(pixels) = self.pixels.clone() else {
            return;
        };
        let matte = self.matte;
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let wake = ctx.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || -> light_image_tool::Result<_> {
                    let mut reservation = light_image_tool::memory::reserve(4 * 1024 * 1024)?;
                    let scale = (512.0 / pixels.width().max(pixels.height()) as f64).min(1.0);
                    let width = (pixels.width() as f64 * scale).round().max(1.0) as u32;
                    let height = (pixels.height() as f64 * scale).round().max(1.0) as u32;
                    token.check()?;
                    let small = image::RgbaImage::from_fn(width, height, |x, y| {
                        *pixels.get_pixel(
                            (u64::from(x) * u64::from(pixels.width()) / u64::from(width)) as u32,
                            (u64::from(y) * u64::from(pixels.height()) / u64::from(height)) as u32,
                        )
                    });
                    let preview =
                        image::DynamicImage::ImageRgb8(export::flatten(&small, matte)).into_rgba8();
                    token.check()?;
                    light_image_tool::memory::Pixels::from_reserved(preview, &mut reservation)
                },
            ))
            .unwrap_or_else(|_| Err(ToolError::Invalid("预览后台异常".into())));
            let _ = tx.send(result);
            wake.request_repaint();
        });
        self.preview_task = Some(PreviewTask {
            generation: self.generation,
            matte,
            cancel,
            receiver: rx,
        });
    }
    fn poll_preview(&mut self, ctx: &egui::Context) {
        let result = self.preview_task.as_ref().map(|t| t.receiver.try_recv());
        if matches!(&result, Some(Err(mpsc::TryRecvError::Disconnected))) {
            self.preview_task = None;
            self.preview_matte = Some(self.matte);
            self.error = Some("预览后台意外退出，可以重新打开导出窗口".into());
            return;
        }
        if let Some(Ok(result)) = result {
            let task = self.preview_task.take().unwrap();
            if self.export_dialog
                && task.generation == self.generation
                && task.matte == self.matte
                && task.cancel.check().is_ok()
            {
                match result {
                    Ok(p) => {
                        match light_image_tool::memory::reserve(p.as_raw().len() as u64 * 2) {
                            Ok(r) => self.preview_reservation = Some(r),
                            Err(e) => {
                                self.error = Some(e.to_string());
                                return;
                            }
                        }
                        self.preview_texture = Some(ctx.load_texture(
                            "export-preview",
                            egui::ColorImage::from_rgba_unmultiplied(
                                [p.width() as usize, p.height() as usize],
                                p.as_raw(),
                            ),
                            egui::TextureOptions::LINEAR,
                        ));
                        self.preview_matte = Some(task.matte);
                    }
                    Err(ToolError::Cancelled) => {}
                    Err(e) => {
                        self.preview_matte = Some(task.matte);
                        self.error = Some(e.to_string());
                    }
                }
            }
        }
    }
    fn navigate(&mut self, step: isize) {
        if self.siblings.is_empty() {
            return;
        }
        let next =
            (self.position as isize + step).rem_euclid(self.siblings.len() as isize) as usize;
        self.position = next;
        self.open(self.siblings[next].clone());
    }
    fn select_image(&mut self, position: usize) {
        if let Some(path) = self.siblings.get(position).cloned() {
            self.position = position;
            self.open(path);
        }
    }
    fn save_preferences(&mut self) {
        if let Some(path) = &self.settings_path {
            self.settings_warning = settings::save(path, &self.preferences)
                .err()
                .map(|error| format!("设置未保存，本次仍可使用：{error}"));
        }
    }
    fn poll_browser(&mut self, ctx: &egui::Context) {
        if let Some(response) = self.directory_worker.take_result()
            && response.generation == self.directory_generation
            && self.directory_path.as_ref() == Some(&browsing::directory(&response.path))
        {
            self.directory_scanning = false;
            match response.result {
                Ok(listing) => {
                    self.siblings = if let Some(path) = &self.requested_path {
                        browsing::ensure_current(listing.paths, path)
                            .unwrap_or_else(|_| vec![path.clone()])
                    } else {
                        listing.paths
                    };
                    if let Some(path) = &self.requested_path {
                        self.position = self.siblings.iter().position(|p| p == path).unwrap_or(0);
                    }
                    self.directory_warning = listing
                        .truncated
                        .then(|| "目录较大，列表已到安全上限；可直接打开其他图片".into());
                    self.locate_current = true;
                }
                Err(ToolError::Cancelled) => {}
                Err(error) => {
                    self.directory_warning =
                        Some(format!("目录列表不可用，仍可查看当前图片：{error}"))
                }
            }
        }
        if let Some(response) = self.thumbnail_worker.take_result() {
            self.accept_thumbnail(response, ctx);
        }
    }
    fn accept_thumbnail(
        &mut self,
        response: browsing::Response<image::RgbaImage>,
        ctx: &egui::Context,
    ) {
        if !self
            .thumbnail_busy
            .as_ref()
            .is_some_and(|(id, path)| *id == response.generation && path == &response.path)
        {
            return;
        }
        self.thumbnail_busy = None;
        if matches!(response.result, Err(ToolError::Cancelled)) {
            return;
        }
        let previous_attempts = self
            .thumbnail_cache
            .iter()
            .find(|entry| entry.path == response.path)
            .map_or(0, |entry| entry.retry_attempts);
        let retryable = response
            .result
            .as_ref()
            .err()
            .is_some_and(browsing::thumbnail_retryable);
        let attempts = if retryable {
            previous_attempts.saturating_add(1)
        } else {
            0
        };
        let retry_after = (retryable && attempts < 3)
            .then(|| Instant::now() + std::time::Duration::from_secs(1 << attempts));
        let texture = response.result.ok().map(|image| {
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [image.width() as usize, image.height() as usize],
                image.as_raw(),
            );
            ctx.load_texture("directory-thumbnail", color, egui::TextureOptions::LINEAR)
        });
        self.thumbnail_cache
            .retain(|entry| entry.path != response.path);
        while self.thumbnail_cache.len() >= browsing::THUMBNAIL_CACHE_ENTRIES {
            let oldest = self
                .thumbnail_cache
                .iter()
                .enumerate()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(index, _)| index)
                .unwrap();
            self.thumbnail_cache.swap_remove(oldest);
        }
        self.thumbnail_cache.push(ThumbnailEntry {
            path: response.path,
            texture,
            used: self.frames,
            retry_attempts: attempts,
            retry_after,
        });
    }
    fn image_list(&mut self, ui: &mut egui::Ui) {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 28.0),
            egui::Layout::left_to_right(egui::Align::Min),
            |ui| {
                ui.label(egui::RichText::new("同目录图片").strong());
                ui.weak(format!("{} / {}", self.position + 1, self.siblings.len()));
            },
        );
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true),
            |ui| {
                if ui
                    .add(
                        CompactAction::new(Icon::Locate, "定位当前图片")
                            .help("定位当前图片：滚动到当前图片"),
                    )
                    .clicked()
                {
                    self.locate_current = true;
                }
                if ui
                    .add(
                        CompactAction::new(Icon::Refresh, "刷新图片列表")
                            .help("刷新图片列表：重新读取目录和缩略图"),
                    )
                    .clicked()
                    && let Some(path) = self.requested_path.clone()
                {
                    self.directory_generation += 1;
                    self.directory_scanning = true;
                    self.thumbnail_worker.cancel();
                    self.thumbnail_busy = None;
                    self.thumbnail_cache.clear();
                    self.directory_worker.request(BrowserRequest {
                        generation: self.directory_generation,
                        path,
                        cancel: Cancellation::default(),
                    });
                }
            },
        );
        if self.directory_scanning {
            ui.weak("正在读取目录…");
        }
        if let Some(warning) = &self.directory_warning {
            ui.label(
                egui::RichText::new(warning)
                    .small()
                    .color(Color32::from_rgb(150, 91, 16)),
            );
        }
        ui.separator();
        let row_height = 62.0;
        let mut scroll = egui::ScrollArea::vertical()
            .animated(false)
            .stick_to_bottom(self.position + 1 == self.siblings.len())
            .id_salt("directory-rows")
            .auto_shrink([false, false]);
        if self.locate_current {
            scroll = scroll.vertical_scroll_offset(
                self.position.saturating_sub(1) as f32 * (row_height + ui.spacing().item_spacing.y),
            );
            self.locate_current = false;
        }
        let mut select = None;
        let mut request = None;
        let mut visible = Vec::new();
        self.selected_list_row_rect = None;
        let output = scroll.show_rows(ui, row_height, self.siblings.len(), |ui, rows| {
            for index in rows {
                let path = self.siblings[index].clone();
                visible.push(path.clone());
                let (rect, response) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), row_height),
                    egui::Sense::click(),
                );
                let selected = self.requested_path.as_ref() == Some(&path);
                if selected {
                    self.selected_list_row_rect = Some(rect);
                }
                let fill = if selected {
                    Color32::from_rgb(225, 237, 247)
                } else if response.hovered() {
                    Color32::from_gray(245)
                } else {
                    Color32::WHITE
                };
                ui.painter().rect_filled(rect, 5.0, fill);
                if response.has_focus() {
                    ui.painter().rect_stroke(
                        rect,
                        5.0,
                        egui::Stroke::new(1.0, Color32::from_rgb(38, 108, 159)),
                        egui::StrokeKind::Inside,
                    );
                }
                let image_rect =
                    Rect::from_min_size(rect.min + Vec2::splat(6.0), Vec2::splat(50.0));
                ui.painter()
                    .rect_filled(image_rect, 2.0, Color32::from_gray(235));
                if let Some(entry) = self
                    .thumbnail_cache
                    .iter_mut()
                    .find(|entry| entry.path == path)
                {
                    entry.used = self.frames;
                    if let Some(retry_after) = entry.retry_after {
                        let now = Instant::now();
                        if now >= retry_after {
                            if request.is_none() {
                                request = Some(path.clone());
                            }
                        } else {
                            ui.ctx().request_repaint_after(retry_after - now);
                        }
                    }
                    if let Some(texture) = &entry.texture {
                        let image_size = texture.size_vec2();
                        let size = image_size * (50.0 / image_size.x.max(image_size.y));
                        ui.painter().image(
                            texture.id(),
                            Rect::from_center_size(image_rect.center(), size),
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else {
                        ui.painter().text(
                            image_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "!",
                            egui::FontId::proportional(18.0),
                            Color32::from_gray(110),
                        );
                    }
                } else if request.is_none() {
                    request = Some(path.clone());
                }
                let text_rect = Rect::from_min_max(
                    rect.min + Vec2::new(64.0, 4.0),
                    rect.max - Vec2::splat(5.0),
                );
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let text_painter = ui.painter().with_clip_rect(text_rect);
                text_painter.text(
                    text_rect.min + Vec2::new(0.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    name.as_ref(),
                    egui::FontId::proportional(16.0),
                    Color32::from_gray(45),
                );
                text_painter.text(
                    text_rect.min + Vec2::new(0.0, 29.0),
                    egui::Align2::LEFT_TOP,
                    format!(
                        "{} · {}",
                        index + 1,
                        path.extension()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_uppercase()
                    ),
                    egui::FontId::proportional(14.0),
                    Color32::from_gray(99),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        true,
                        selected,
                        format!("{} {}", index + 1, name),
                    )
                });
                response
                    .clone()
                    .on_hover_text(path.file_name().unwrap_or_default().to_string_lossy());
                if response.clicked()
                    || (response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    select = Some(index);
                }
            }
        });
        self.list_viewport_rect = Some(output.inner_rect);
        if let Some(smoke) = &self.smoke {
            let trace = serde_json::json!({"viewport":self.list_viewport_rect.map(|r|[r.min.x,r.min.y,r.max.x,r.max.y]),"selected":self.selected_list_row_rect.map(|r|[r.min.x,r.min.y,r.max.x,r.max.y]),"offset":[output.state.offset.x,output.state.offset.y],"content_size":[output.content_size.x,output.content_size.y]});
            let _ = std::fs::write(
                smoke.directory.join("list-layout.json"),
                serde_json::to_vec_pretty(&trace).unwrap_or_default(),
            );
        }
        if self.thumbnail_cache.len() >= browsing::THUMBNAIL_CACHE_ENTRIES {
            self.thumbnail_cache
                .retain(|entry| visible.contains(&entry.path));
        }
        if self.thumbnail_busy.is_none()
            && !self.loading
            && self.work.is_none()
            && let Some(path) = request
        {
            self.thumbnail_sequence += 1;
            self.thumbnail_busy = Some((self.thumbnail_sequence, path.clone()));
            self.thumbnail_worker.request(BrowserRequest {
                generation: self.thumbnail_sequence,
                path,
                cancel: Cancellation::default(),
            });
        }
        if let Some(position) = select {
            self.select_image(position);
        }
    }
    fn dimensions(&self) -> [u32; 2] {
        self.pixels
            .as_ref()
            .map(|p| [p.width(), p.height()])
            .unwrap_or([1, 1])
    }
    fn undo_edit(&mut self, ctx: &egui::Context) {
        if self.work.is_some() || self.loading {
            return;
        }
        if let Some(previous) = self.undo.pop() {
            self.pixels = Some(previous);
            self.edited = self
                .loaded
                .as_ref()
                .zip(self.pixels.as_ref())
                .is_some_and(|(l, p)| !Arc::ptr_eq(&l.pixels, p));
            self.refresh_texture(ctx);
            let [w, h] = self.dimensions();
            self.resize = [w, h];
            self.crop = [0, 0, w, h];
            self.fit = true;
            self.pan = Vec2::ZERO;
        }
    }
    fn refresh_texture(&mut self, ctx: &egui::Context) {
        self.crop_selecting = false;
        self.crop_anchor = None;
        self.close_export();
        self.tiles.clear();
        if let Some(image) = &self.pixels {
            self.current_has_alpha = image.pixels().any(|p| p[3] < 255);
            // Existing egui texture path, tiles avoid device texture size limits without downsampling.
            const TILE: u32 = 2048;
            let mut reservation = match light_image_tool::memory::reserve(
                image.as_raw().len() as u64 * 2 + light_image_tool::memory::OVERHEAD,
            ) {
                Ok(r) => r,
                Err(e) => {
                    self.error = Some(e.to_string());
                    return;
                }
            };
            for y in (0..image.height()).step_by(TILE as usize) {
                for x in (0..image.width()).step_by(TILE as usize) {
                    let w = TILE.min(image.width() - x);
                    let h = TILE.min(image.height() - y);
                    let tile = image::imageops::crop_imm(&***image, x, y, w, h).to_image();
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [w as usize, h as usize],
                        tile.as_raw(),
                    );
                    let texture = ctx.load_texture(
                        format!("image-{}-{x}-{y}", self.generation),
                        color,
                        egui::TextureOptions::LINEAR,
                    );
                    self.tiles.push(Tile {
                        _reservation: reservation
                            .split(u64::from(w) * u64::from(h) * 8)
                            .expect("bounded tile accounting"),
                        texture,
                        x,
                        y,
                        w,
                        h,
                    });
                }
            }
        }
    }
    fn poll(&mut self, ctx: &egui::Context) {
        self.poll_browser(ctx);
        self.poll_preview(ctx);
        let evidence_result = self.evidence_task.as_ref().map(|t| t.receiver.try_recv());
        if let Some(Err(mpsc::TryRecvError::Disconnected)) = &evidence_result {
            let task = self.evidence_task.take().unwrap();
            if task.generation == self.generation {
                self.evidence_stage = EvidenceStage::Failed;
                self.evidence = Some(EvidenceReport::incomplete(
                    self.loaded
                        .as_ref()
                        .map(|l| l.source_sha256.clone())
                        .unwrap_or_default(),
                    "来源后台意外退出，可重新核验",
                ));
            }
        }
        if let Some(Ok(result)) = evidence_result {
            let task = self.evidence_task.take().unwrap();
            if task.generation == self.generation {
                let result = if task.cancel.check().is_err() {
                    Err(ToolError::Cancelled)
                } else {
                    result
                };
                let hash = self
                    .loaded
                    .as_ref()
                    .map(|i| i.source_sha256.clone())
                    .unwrap_or_default();
                let report = match result {
                    Ok(r) => {
                        self.evidence_stage = EvidenceStage::Complete;
                        r
                    }
                    Err(e) => {
                        self.evidence_stage = if matches!(e, ToolError::Cancelled) {
                            EvidenceStage::Cancelled
                        } else {
                            EvidenceStage::Failed
                        };
                        EvidenceReport::incomplete(hash, e.to_string())
                    }
                };
                if matches!(
                    report.state,
                    EvidenceState::Signed | EvidenceState::NoCredential
                ) {
                    self.evidence_cache.insert(report.clone());
                }
                self.evidence = Some(report);
            }
        }
        if let Some(response) = self.loader.take_result() {
            self.apply_load_response(response, ctx);
        }
        let done = self.work.as_ref().map(|w| w.receiver.try_recv());
        if let Some(Err(mpsc::TryRecvError::Disconnected)) = &done {
            self.work = None;
            self.error = Some("后台操作意外退出；可以重新打开、编辑或导出".into());
            self.status = "操作已结束，原图未修改".into();
            return;
        }
        let done = done.and_then(|r| r.ok());
        if let Some(result) = done {
            let work = self.work.take().unwrap();
            if work.generation != self.generation {
                return;
            }
            if matches!(&result, WorkResult::Edited(_)) && work.cancel.check().is_err() {
                self.status = "操作已取消".into();
                return;
            }
            match result {
                WorkResult::Edited(Ok(image)) => {
                    if let Some(previous) = self.pixels.take() {
                        self.undo.push(previous);
                        while self.undo.len() > 3
                            || self.undo.iter().map(|p| p.as_raw().len()).sum::<usize>()
                                > 256 * 1024 * 1024
                        {
                            self.undo.remove(0);
                        }
                    }
                    self.pixels = Some(Arc::new(image));
                    self.edited = true;
                    self.fit = true;
                    self.pan = Vec2::ZERO;
                    self.resize = self.dimensions();
                    let [w, h] = self.resize;
                    self.crop = [0, 0, w, h];
                    self.status = "编辑副本 · 来源签名不再代表当前像素".into();
                    self.refresh_texture(ctx);
                }
                WorkResult::Exported(Ok(path)) => {
                    self.status = format!("已导出新副本：{}", path.display());
                    self.notice_until = Some(Instant::now() + std::time::Duration::from_secs(6));
                    ctx.request_repaint_after(std::time::Duration::from_secs(6));
                    log::info!("export committed: {}", path.display());
                }
                WorkResult::Edited(Err(ToolError::Cancelled))
                | WorkResult::Exported(Err(ToolError::Cancelled)) => {
                    self.status = "操作已取消".into()
                }
                WorkResult::Edited(Err(e)) | WorkResult::Exported(Err(e)) => {
                    self.error = Some(e.to_string())
                }
            }
        }
    }
    fn apply_load_response(
        &mut self,
        response: light_image_tool::worker::LoadResponse,
        ctx: &egui::Context,
    ) {
        if response.generation != self.generation {
            return;
        }
        self.loading = false;
        let result = if self.load_cancel.check().is_err() {
            Err(ToolError::Cancelled)
        } else {
            response.result
        };
        match result {
            Ok(image) => {
                if !self.siblings.contains(&image.path) {
                    // A failed/limited scan must still allow the explicitly opened image.
                    if self.siblings.len() >= browsing::MAX_DIRECTORY_IMAGES {
                        self.siblings.pop();
                    }
                    self.siblings.push(image.path.clone());
                }
                self.position = self
                    .siblings
                    .iter()
                    .position(|p| p == &image.path)
                    .unwrap_or(0);
                self.status = format!("已打开 · {} ms · 原图只读", image.load_ms);
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                    "gloamere-image · {}",
                    image.path.file_name().unwrap_or_default().to_string_lossy()
                )));
                self.pixels = Some(image.pixels.clone());
                self.evidence = self.evidence_cache.get(&image.source_sha256);
                self.evidence_attempted = self.evidence.is_some();
                self.evidence_stage = if self.evidence.is_some() {
                    EvidenceStage::Complete
                } else {
                    EvidenceStage::NotStarted
                };
                self.loaded = Some(image);
                self.undo.clear();
                self.edited = false;
                self.fit = true;
                self.pan = Vec2::ZERO;
                let [w, h] = self.dimensions();
                self.crop = [0, 0, w, h];
                self.resize = [w, h];
                self.refresh_texture(ctx);
            }
            Err(ToolError::Cancelled) => {
                self.restore_browser_selection();
                self.status = "已取消打开".into();
            }
            Err(e) => {
                self.restore_browser_selection();
                self.error = Some(e.to_string());
                self.status = if self.loaded.is_some() {
                    "打开失败；上一张图片仍可查看"
                } else {
                    "打开失败；请选择另一张 JPEG / PNG 图片"
                }
                .into();
            }
        }
    }
    fn start_edit(&mut self, op: EditOp, ctx: &egui::Context) {
        if self.work.is_some() || self.loading {
            return;
        }
        let Some(pixels) = self.pixels.clone() else {
            return;
        };
        self.close_export();
        let extra = match edit::extra_bytes(&pixels, &op) {
            Ok(e) => e,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        self.evict_history(extra);
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let wake = ctx.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = protected(|| edit::apply(&pixels, &op, &token), "编辑异常；原图未修改");
            let _ = tx.send(WorkResult::Edited(result));
            wake.request_repaint();
        });
        self.work = Some(Work {
            generation: self.generation,
            cancel,
            receiver: rx,
            export_thread: None,
        });
        self.status = "正在处理编辑副本…".into();
    }
    fn begin_export(&mut self) {
        if self.loading || self.work.is_some() {
            return;
        }
        if let Some(image) = &self.loaded {
            if !image.color_managed {
                self.error = Some("此图片的 ICC 未完整处理，暂不能导出校准的色彩副本".into());
                return;
            }
            self.export_name = export::suggest_path(&image.path, self.format)
                .to_string_lossy()
                .into();
            self.export_dialog = true;
            self.export_generation = Some(self.generation);
        }
    }
    fn start_export(&mut self, ctx: &egui::Context) {
        if self.work.is_some() || self.loading {
            return;
        }
        let (Some(pixels), Some(loaded)) = (self.pixels.clone(), self.loaded.as_ref()) else {
            return;
        };
        if self.export_generation != Some(self.generation) || !loaded.color_managed {
            self.close_export();
            self.error = Some("导出确认已失效或 ICC 未校准，请重新打开当前图片的导出窗口".into());
            return;
        }
        let original = loaded.path.clone();
        self.evict_history(pixels.as_raw().len() as u64 * 4 + light_image_tool::memory::OVERHEAD);
        let options = ExportOptions {
            destination: PathBuf::from(&self.export_name),
            format: self.format,
            quality: self.quality,
            matte: self.matte,
        };
        self.work = Some(Work::spawn_export(self.generation, move |token| {
            export::save_new(&pixels, &original, &options, &token)
        }));
        // Export completion is polled on the UI thread so shutdown can join without
        // waiting on an egui lock held by this thread.
        ctx.request_repaint();
        self.close_export();
        self.status = "正在导出新副本…".into();
    }

    fn start_provenance(&mut self, ctx: &egui::Context) {
        if self.loading || self.evidence_task.is_some() || self.evidence_attempted {
            return;
        }
        let Some(image) = &self.loaded else {
            return;
        };
        let path = image.path.clone();
        let hash = image.source_sha256.clone();
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let wake = ctx.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                provenance::inspect(&path, &hash, &token)
            }))
            .unwrap_or_else(|_| Err(ToolError::Invalid("来源核验异常；结果不完整".into())));
            let _ = tx.send(result);
            wake.request_repaint();
        });
        self.evidence_task = Some(ProvenanceTask {
            generation: self.generation,
            cancel,
            receiver: rx,
        });
        self.evidence_attempted = true;
        self.evidence_stage = EvidenceStage::Running;
    }

    fn evidence_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(evidence_stage_label(self.evidence_stage));
        if let Some(report) = &self.evidence {
            ui.label(match report.state {
                EvidenceState::NoCredential => "无嵌入凭证",
                EvidenceState::Signed => "存在签名凭证",
                EvidenceState::Incomplete if self.evidence_stage == EvidenceStage::Complete => {
                    "离线资料不足 · 核验不完整"
                }
                EvidenceState::Incomplete => "未生成完整核验报告",
                EvidenceState::Error => "核验错误",
            });
            ui.label(format!(
                "签名：{}",
                evidence_check_label(&report.state, &report.signature)
            ));
            ui.label(format!(
                "资产绑定：{}",
                evidence_check_label(&report.state, &report.asset_binding)
            ));
            ui.label(format!(
                "信任：{}",
                if matches!(report.state, EvidenceState::NoCredential) {
                    "不适用（未发现凭证）"
                } else {
                    &report.trust
                }
            ));
            ui.label(&report.details);
            if !report.signed_declarations.is_empty() {
                ui.collapsing(
                    "凭证声明（结合签名、绑定和信任解读）",
                    |ui| {
                        for field in &report.signed_declarations {
                            ui.label(format!("{}：{}", field.label, field.value));
                        }
                    },
                );
            }
            if !report.camera_exif.is_empty() {
                ui.collapsing("相机 EXIF（未签名，不认证拍摄）", |ui| {
                    for field in &report.camera_exif {
                        ui.label(format!("{}：{}", field.label, field.value));
                    }
                });
            }
            if !report.unsigned_claims.is_empty() {
                ui.collapsing("未签名声明（不认证生成来源）", |ui| {
                    for field in &report.unsigned_claims {
                        ui.label(&field.label);
                        ui.small(&field.value);
                    }
                });
            }
            ui.collapsing("离线限制与核验记录", |ui| {
                for limitation in &report.offline_limitations {
                    ui.small(limitation);
                }
                ui.small(format!(
                    "检查时间 UTC Unix：{}",
                    report.checked_unix_seconds
                ));
                ui.small(format!("载入快照 SHA-256：{}", report.file_sha256));
                ui.small(format!(
                    "策略 {} / 信任 {}",
                    report.policy_version, report.trust_snapshot
                ));
                for code in &report.status_codes {
                    ui.small(code);
                }
            });
        }
        if ui
            .add_enabled(
                self.evidence_task.is_none(),
                CompactAction::new(Icon::Recheck, "重新核验来源"),
            )
            .clicked()
        {
            self.evidence = None;
            self.evidence_attempted = false;
            self.evidence_stage = EvidenceStage::NotStarted;
        }
        ui.small("来源证据不等于画面内容真实；无证据不等于非 AI。");
    }

    fn toggle_list(&mut self) {
        self.preferences.list_visible = !self.preferences.list_visible;
        if self.preferences.list_visible {
            self.locate_current = true;
        } else {
            self.thumbnail_worker.cancel();
            self.thumbnail_busy = None;
        }
        self.save_preferences();
    }
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let ready = self.pixels.is_some() && !self.loading && self.work.is_none();
        let modal = self.path_dialog || self.export_dialog || self.error.is_some();
        let (area, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), viewer_ui::TOOLBAR_HEIGHT),
            egui::Sense::hover(),
        );
        let layout = ToolbarLayout::new(area);
        ui.painter().line_segment(
            [area.left_top(), area.right_top()],
            egui::Stroke::new(1.0, Color32::from_gray(231)),
        );
        if let Some(rect) = layout.metadata {
            let text = if let Some(image) = &self.loaded {
                let [w, h] = self.dimensions();
                let size = if image.file_bytes >= 1024 * 1024 {
                    format!("{:.1} MB", image.file_bytes as f64 / 1048576.0)
                } else {
                    format!("{} KB", image.file_bytes.div_ceil(1024))
                };
                format!("{w} × {h}  ·  {size}")
            } else {
                "JPEG / PNG".into()
            };
            viewer_ui::edge_label(ui, rect, &text, false).on_hover_text(text);
        }
        if let Some(rect) = layout.zoom {
            viewer_ui::edge_label(ui, rect, &format!("{:.0}%", self.zoom * 100.0), true)
                .on_hover_text("实际显示比例 · 1 为实际像素，F 适应窗口");
        }
        for x in &layout.separators {
            ui.painter().line_segment(
                [
                    Pos2::new(*x, area.center().y - 11.0),
                    Pos2::new(*x, area.center().y + 11.0),
                ],
                egui::Stroke::new(1.0, Color32::from_gray(225)),
            );
        }
        for (index, icon) in Icon::ALL.into_iter().enumerate() {
            let enabled = !modal
                && !(icon == Icon::More && self.more_trigger_release_blocked)
                && match icon {
                    Icon::Previous | Icon::Next => !self.siblings.is_empty(),
                    Icon::Undo => ready && !self.undo.is_empty(),
                    Icon::Rotate | Icon::Export => ready,
                    Icon::Minus | Icon::Plus | Icon::Fit | Icon::Actual => self.pixels.is_some(),
                    Icon::Open | Icon::More => true,
                    _ => false,
                };
            let response = viewer_ui::icon_button(
                ui,
                layout.buttons[index],
                icon,
                enabled,
                icon == Icon::More && self.more_open,
            );
            if icon == Icon::More {
                self.more_button_rect = Some(response.rect);
            }
            if enabled && response.clicked() {
                if icon != Icon::More {
                    self.more_open = false;
                }
                match icon {
                    Icon::Open => self.open_dialog(),
                    Icon::Previous => self.navigate(-1),
                    Icon::Next => self.navigate(1),
                    Icon::Minus => {
                        self.fit = false;
                        self.zoom = (self.zoom / 1.25).max(0.01);
                    }
                    Icon::Plus => {
                        self.fit = false;
                        self.zoom = (self.zoom * 1.25).min(32.0);
                    }
                    Icon::Fit => {
                        self.fit = true;
                        self.pan = Vec2::ZERO;
                    }
                    Icon::Actual => {
                        self.fit = false;
                        self.zoom = 1.0;
                        self.pan = Vec2::ZERO;
                    }
                    Icon::Rotate => self.start_edit(EditOp::Rotate90, &ctx),
                    Icon::Undo => self.undo_edit(&ctx),
                    Icon::Export => self.begin_export(),
                    Icon::More => self.more_open = !self.more_open,
                    _ => {}
                }
            }
        }
        self.toolbar_layout = Some(layout);
    }
    fn more_menu(&mut self, ctx: &egui::Context) {
        let newly_opened = self.menu_rect.is_none();
        self.menu_rect = None;
        if !self.more_open {
            return;
        }
        ctx.memory_mut(|memory| {
            memory.set_modal_layer(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("viewer-more-menu"),
            ))
        });
        let ready = self.pixels.is_some() && !self.loading && self.work.is_none();
        let mut close = false;
        let anchor = self
            .more_button_rect
            .map(|rect| rect.right_top())
            .unwrap_or_else(|| ctx.viewport_rect().right_bottom() - Vec2::new(12.0, 52.0));
        let output = egui::Area::new(egui::Id::new("viewer-more-menu"))
            .order(egui::Order::Foreground)
            .default_height((ctx.viewport_rect().height() - 80.0).max(120.0))
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(anchor - Vec2::new(0.0, 10.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(270.0);
                    ui.spacing_mut().interact_size.y = 28.0;
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.spacing_mut().button_padding = Vec2::new(6.0, 4.0);
                    egui::ScrollArea::vertical()
                        .max_height((ctx.viewport_rect().height() - 92.0).max(120.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Image::new(&self.brand_texture)
                                        .fit_to_exact_size(Vec2::splat(22.0)),
                                );
                                ui.strong(viewer_ui::PRODUCT_NAME);
                            });
                            let path =
                                ui.add(ActionButton::new(Icon::Open, "通过路径打开…").menu());
                            if newly_opened {
                                path.request_focus();
                            }
                            if path.clicked() {
                                self.path_dialog = true;
                                close = true;
                            }
                            let list = ui.add(
                                ActionButton::new(Icon::List, "图片列表")
                                    .selected(self.preferences.list_visible)
                                    .menu(),
                            );
                            #[cfg(test)]
                            {
                                self.menu_list_rect = Some(list.rect);
                            }
                            if list.clicked() {
                                self.toggle_list();
                                close = true;
                            }
                            let edit = ui.add_enabled(
                                ready,
                                ActionButton::new(Icon::Crop, "裁剪 / 缩放")
                                    .selected(self.editing)
                                    .menu(),
                            );
                            #[cfg(test)]
                            {
                                self.menu_edit_rect = Some(edit.rect);
                            }
                            if edit.clicked() {
                                self.editing = !self.editing;
                                self.info_open = false;
                                close = true;
                            }
                            if ui
                                .add(
                                    ActionButton::new(Icon::Info, "信息 / 安全来源")
                                        .selected(self.info_open)
                                        .menu(),
                                )
                                .clicked()
                            {
                                self.info_open = !self.info_open;
                                self.editing = false;
                                close = true;
                            }
                            if ui
                                .add_enabled(
                                    ready,
                                    ActionButton::new(Icon::RotateLeft, "左转 90° · Shift+R")
                                        .menu(),
                                )
                                .clicked()
                            {
                                self.start_edit(EditOp::Rotate270, ctx);
                                close = true;
                            }
                            if ui
                                .add(
                                    ActionButton::new(Icon::Fullscreen, "全屏 · F11")
                                        .selected(self.fullscreen)
                                        .menu(),
                                )
                                .clicked()
                            {
                                self.fullscreen = !self.fullscreen;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
                                    self.fullscreen,
                                ));
                                close = true;
                            }
                            ui.separator();
                            ui.label("画布背景");
                            egui::Grid::new("canvas-background-menu")
                                .num_columns(2)
                                .spacing(Vec2::new(8.0, 4.0))
                                .show(ui, |ui| {
                                    for (index, background) in
                                        CanvasBackground::ALL.into_iter().enumerate()
                                    {
                                        if ui
                                            .add_sized(
                                                [124.0, 28.0],
                                                egui::Button::new(background.label())
                                                    .frame(false)
                                                    .selected(
                                                        self.preferences.background == background,
                                                    ),
                                            )
                                            .clicked()
                                        {
                                            self.preferences.background = background;
                                            self.save_preferences();
                                            close = true;
                                        }
                                        if index % 2 == 1 {
                                            ui.end_row();
                                        }
                                    }
                                });
                            ui.small("只改变画布，不改变透明像素或导出底色");
                            ui.separator();
                            let [w, h] = self.dimensions();
                            if self.loaded.is_some() && ctx.viewport_rect().width() < 720.0 {
                                ui.small(format!("{w} × {h} · {:.0}%", self.zoom * 100.0));
                            }
                            ui.small(&self.status);
                            if let Some(warning) = &self.settings_warning {
                                ui.colored_label(Color32::from_rgb(150, 91, 16), warning);
                            }
                            ui.small("原图只读 · 仅本地处理");
                            // Leave room for system CJK descenders at the scroll clip edge.
                            ui.add_space(8.0);
                        });
                });
            });
        self.menu_rect = Some(output.response.rect);
        // An open modal can suppress the underlying toolbar response. Handle
        // its trigger explicitly, and suppress the matching release so one
        // press cannot close and immediately reopen the menu.
        let trigger_pressed = ctx.input(|input| {
            input.pointer.primary_pressed()
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| self.more_button_rect.is_some_and(|rect| rect.contains(pos)))
        });
        let outside = ctx.input(|input| {
            input.pointer.any_pressed()
                && input.pointer.interact_pos().is_some_and(|pos| {
                    !output.response.rect.contains(pos)
                        && !self.more_button_rect.is_some_and(|rect| rect.contains(pos))
                })
        });
        if close || outside || trigger_pressed {
            self.more_open = false;
            self.more_trigger_release_blocked = trigger_pressed;
            ctx.memory_mut(|memory| memory.request_focus(viewer_ui::icon_id(Icon::More)));
        }
    }
    fn inspector(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading(if self.editing {
                "编辑副本"
            } else {
                "图片信息"
            });
            if ui
                .add(CompactAction::new(Icon::Close, "关闭面板").help("关闭面板 · Esc"))
                .clicked()
            {
                self.editing = false;
                self.info_open = false;
                self.crop_selecting = false;
                self.crop_anchor = None;
            }
        });
        if self.editing {
            ui.label("裁剪 · 旋转 · 缩放，导出到新副本");
        }
        ui.add_space(8.0);
        if let Some(image) = &self.loaded {
            let path = image.path.clone();
            let [w, h] = self.dimensions();
            ui.label(
                egui::RichText::new(path.file_name().unwrap_or_default().to_string_lossy())
                    .strong(),
            );
            ui.label(format!("{w} × {h} 像素"));
            ui.label(format!(
                "{:?} · {:.2} MiB",
                image.format,
                image.file_bytes as f64 / 1048576.0
            ));
            ui.label(if image.format == image::ImageFormat::Jpeg {
                "透明度不适用（JPEG 不含 alpha）"
            } else if self.current_has_alpha {
                "含透明像素"
            } else {
                "无透明像素"
            });
            ui.label(format!("EXIF 方向：{}（已应用）", image.orientation));
            ui.label(&image.color_status);
            for warning in &image.warnings {
                ui.colored_label(Color32::from_rgb(150, 91, 16), warning);
            }
            ui.separator();
            if self.editing {
                ui.add_enabled_ui(self.work.is_none() && !self.loading, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add(
                                CompactAction::new(Icon::Rotate, "顺时针旋转 90°")
                                    .help("顺时针旋转 90° · R"),
                            )
                            .clicked()
                        {
                            self.start_edit(EditOp::Rotate90, &ctx);
                        }
                        if ui
                            .add(
                                CompactAction::new(Icon::RotateLeft, "逆时针旋转 90°")
                                    .help("逆时针旋转 90° · Shift+R"),
                            )
                            .clicked()
                        {
                            self.start_edit(EditOp::Rotate270, &ctx);
                        }
                        if ui.add(CompactAction::new(Icon::Flip, "水平翻转")).clicked() {
                            self.start_edit(EditOp::FlipHorizontal, &ctx);
                        }
                    });
                    if ui
                        .add(
                            CompactAction::new(Icon::Crop, "拖拽选择裁剪")
                                .selected(self.crop_selecting)
                                .help("拖拽选择裁剪：左键选择，右键平移。Esc 返回浏览平移。"),
                        )
                        .clicked()
                    {
                        self.crop_selecting = !self.crop_selecting;
                        self.crop_anchor = None;
                    }
                    if self.crop_selecting {
                        ui.label("左键框选 · 右键平移 · 应用后才裁剪");
                    }
                    ui.label("裁剪（当前图片像素坐标）");
                    egui::Grid::new("crop").num_columns(2).show(ui, |ui| {
                        for (i, label) in ["左 X", "上 Y", "宽", "高"].iter().enumerate() {
                            ui.label(*label);
                            ui.add(egui::DragValue::new(&mut self.crop[i]).range(0..=32768));
                            ui.end_row();
                        }
                    });
                    if !self.crop_selecting
                        && ui
                            .add(CompactAction::new(Icon::CropApply, "应用裁剪"))
                            .clicked()
                    {
                        self.apply_crop(&ctx);
                    }
                    ui.separator();
                    ui.label("缩放尺寸");
                    ui.checkbox(&mut self.keep_ratio, "锁定宽高比");
                    let before = self.resize;
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.resize[0]).range(1..=32768));
                        ui.label("×");
                        ui.add(egui::DragValue::new(&mut self.resize[1]).range(1..=32768));
                    });
                    if self.keep_ratio {
                        if before[0] != self.resize[0] {
                            self.resize[1] = (self.resize[0] as f64 * h as f64 / w as f64)
                                .round()
                                .max(1.0) as u32;
                        } else if before[1] != self.resize[1] {
                            self.resize[0] = (self.resize[1] as f64 * w as f64 / h as f64)
                                .round()
                                .max(1.0) as u32;
                        }
                    }
                    if ui
                        .add(CompactAction::new(Icon::ResizeApply, "应用缩放"))
                        .clicked()
                    {
                        self.start_edit(
                            EditOp::Resize {
                                width: self.resize[0],
                                height: self.resize[1],
                            },
                            &ctx,
                        );
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.undo.is_empty(),
                                CompactAction::new(Icon::Undo, "撤销")
                                    .help("撤销上一次编辑 · Ctrl+Z"),
                            )
                            .clicked()
                        {
                            self.undo_edit(&ctx);
                        }
                        if ui
                            .add(
                                CompactAction::new(Icon::Restore, "恢复原图")
                                    .help("恢复原图：清除本次会话编辑和撤销记录"),
                            )
                            .clicked()
                        {
                            self.pixels = self.loaded.as_ref().map(|l| l.pixels.clone());
                            self.undo.clear();
                            self.edited = false;
                            self.refresh_texture(&ctx);
                            let [w, h] = self.dimensions();
                            self.resize = [w, h];
                            self.crop = [0, 0, w, h];
                        }
                    });
                });
                ui.separator();
            }
            ui.heading("来源证据");
            if self.edited {
                ui.colored_label(
                    Color32::from_rgb(150, 91, 16),
                    "编辑副本：原图签名不适用于当前像素",
                );
            }
            self.evidence_ui(ui);
            ui.separator();
            ui.small("仅本地处理 · 无图片上传 · 无遥测");
            ui.small(concat!("未签名测试版 ", env!("CARGO_PKG_VERSION")));
            ui.collapsing("运行信息", |ui| {
                ui.small(&self.adapter);
                ui.small(format!("UI 绘制次数：{}", self.frames));
                ui.small(format!(
                    "本次运行 {:.0} 秒",
                    self.start.elapsed().as_secs_f64()
                ));
                ui.small(path.to_string_lossy());
            });
        } else {
            ui.label("打开一张本地图片后，在这里核对尺寸、透明度与来源。");
            ui.separator();
            ui.small("支持 JPEG / PNG");
            ui.small("单图最多 1600 万像素、64 MiB");
        }
    }

    fn apply_crop(&mut self, ctx: &egui::Context) {
        self.start_edit(
            EditOp::Crop {
                x: self.crop[0],
                y: self.crop[1],
                width: self.crop[2],
                height: self.crop[3],
            },
            ctx,
        );
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        self.canvas_image_rect = None;
        if !self.editing {
            self.crop_selecting = false;
            self.crop_anchor = None;
        }
        let (area, response) =
            ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let painter = ui.painter_at(area);
        let (background, checker) = self.preferences.background.colors();
        painter.rect_filled(
            area,
            0.0,
            Color32::from_rgb(background[0], background[1], background[2]),
        );
        if let Some(colors) = checker {
            viewer_ui::paint_checker(&painter, area, colors);
        }
        let dark_canvas = matches!(
            self.preferences.background,
            CanvasBackground::Dark | CanvasBackground::CheckerDark
        );
        let text_color = if dark_canvas {
            Color32::from_gray(221)
        } else {
            Color32::from_gray(74)
        };
        if self.pixels.is_none() {
            painter.image(
                self.brand_texture.id(),
                viewer_ui::empty_brand_rect(area),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            painter.text(
                area.center() - Vec2::new(0.0, 35.0),
                egui::Align2::CENTER_CENTER,
                "把图片拖到这里",
                egui::FontId::proportional(25.0),
                text_color,
            );
            painter.text(
                area.center() + Vec2::new(0.0, 10.0),
                egui::Align2::CENTER_CENTER,
                "JPEG / PNG · 本地查看与编辑 · 导出新副本",
                egui::FontId::proportional(15.0),
                text_color,
            );
            if response.double_clicked() {
                self.open_dialog();
            }
            return;
        }
        let [w, h] = self.dimensions();
        let ppp = ui.ctx().pixels_per_point();
        let base = Vec2::new(w as f32 / ppp, h as f32 / ppp);
        if self.fit {
            self.zoom = ((area.width() - 40.0) / base.x)
                .min((area.height() - 40.0) / base.y)
                .clamp(0.01, 1.0);
        }
        if response.dragged_by(egui::PointerButton::Secondary) {
            self.crop_anchor = None;
        }
        if response.dragged()
            && (!self.crop_selecting || response.dragged_by(egui::PointerButton::Secondary))
        {
            self.pan += ui.input(|i| i.pointer.delta());
            self.fit = false;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.01 {
                let old = self.zoom;
                self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.01, 32.0);
                self.fit = false;
                if let Some(mouse) = ui.input(|i| i.pointer.hover_pos()) {
                    self.pan = (mouse - area.center())
                        - (mouse - area.center() - self.pan) * (self.zoom / old);
                }
            }
        }
        if response.double_clicked() && !self.crop_selecting {
            self.fit = true;
            self.pan = Vec2::ZERO;
        }
        let size = base * self.zoom;
        let top = area.center() + self.pan - size / 2.0;
        let rect = Rect::from_min_size(top, size);
        self.canvas_image_rect = Some(rect);
        if self.crop_selecting
            && !self.loading
            && self.work.is_none()
            && !self.tiles.is_empty()
            && !response.dragged_by(egui::PointerButton::Secondary)
        {
            if response.drag_started_by(egui::PointerButton::Primary) {
                self.crop_anchor = ui
                    .input(|i| i.pointer.press_origin())
                    .filter(|point| rect.contains(*point))
                    .and_then(|point| crate::crop_selection::image_point(rect, [w, h], point));
            }
            if (response.dragged_by(egui::PointerButton::Primary)
                || response.drag_stopped_by(egui::PointerButton::Primary))
                && let Some(start) = self.crop_anchor
                && let Some(point) = ui.input(|i| i.pointer.latest_pos())
                && let Some(end) = crate::crop_selection::image_point(rect, [w, h], point)
                && let Some(crop) = crate::crop_selection::from_points([w, h], start, end)
            {
                self.crop = crop;
            }
            if response.drag_stopped_by(egui::PointerButton::Primary) {
                self.crop_anchor = None;
            }
        }
        for tile in &self.tiles {
            let min = top + Vec2::new(tile.x as f32, tile.y as f32) * (self.zoom / ppp);
            let dest = Rect::from_min_size(
                min,
                Vec2::new(tile.w as f32, tile.h as f32) * (self.zoom / ppp),
            );
            if dest.intersects(area) {
                painter.image(
                    tile.texture.id(),
                    dest,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }
        if self.crop_selecting
            && let Some(selection) = crate::crop_selection::screen_rect(rect, [w, h], self.crop)
        {
            for shade in [
                Rect::from_min_max(rect.min, Pos2::new(rect.max.x, selection.min.y)),
                Rect::from_min_max(Pos2::new(rect.min.x, selection.max.y), rect.max),
                Rect::from_min_max(
                    Pos2::new(rect.min.x, selection.min.y),
                    Pos2::new(selection.min.x, selection.max.y),
                ),
                Rect::from_min_max(
                    Pos2::new(selection.max.x, selection.min.y),
                    Pos2::new(rect.max.x, selection.max.y),
                ),
            ] {
                let shade = shade.intersect(area);
                if shade.is_positive() {
                    painter.rect_filled(shade, 0.0, Color32::from_black_alpha(150));
                }
            }
            let stroke = egui::Stroke::new(1.5, Color32::from_rgb(38, 108, 159));
            painter.rect_stroke(selection, 0.0, stroke, egui::StrokeKind::Inside);
            for fraction in [1.0 / 3.0, 2.0 / 3.0] {
                let x = selection.min.x + selection.width() * fraction;
                let y = selection.min.y + selection.height() * fraction;
                painter.line_segment(
                    [Pos2::new(x, selection.min.y), Pos2::new(x, selection.max.y)],
                    egui::Stroke::new(1.0, Color32::from_white_alpha(150)),
                );
                painter.line_segment(
                    [Pos2::new(selection.min.x, y), Pos2::new(selection.max.x, y)],
                    egui::Stroke::new(1.0, Color32::from_white_alpha(150)),
                );
            }
        }
        if self.loading {
            painter.text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                "正在加载…",
                egui::FontId::proportional(20.0),
                text_color,
            );
        }
    }

    fn qa_workflow(&mut self, ctx: &egui::Context) -> light_image_tool::Result<()> {
        let Some(smoke) = &self.smoke else {
            return Ok(());
        };
        if smoke.workflow_image.is_none() || smoke.workflow_result.is_some() {
            return Ok(());
        }
        if let Some(error) = &self.error {
            return Err(ToolError::Invalid(error.clone()));
        }
        if self.loading
            || self.work.is_some()
            || self.preview_task.is_some()
            || self.loaded.is_none()
        {
            return Ok(());
        }
        if self.frames < smoke.workflow_frame + 2 {
            ctx.request_repaint();
            return Ok(());
        }
        if Instant::now() < smoke.workflow_not_before {
            ctx.request_repaint_after(
                smoke
                    .workflow_not_before
                    .saturating_duration_since(Instant::now()),
            );
            return Ok(());
        }
        let stage = smoke.workflow_stage;
        let opens = smoke.workflow_opens.clamp(4, 1000) & !1;
        let action = if stage >= 4 + opens {
            stage - (opens - 4)
        } else {
            stage
        };
        let opening = stage >= 4 && stage < 4 + opens;
        let directory = smoke.directory.clone();
        let second = smoke.workflow_image.clone().unwrap();
        let original = smoke.workflow_original.clone();
        if opening {
            self.open(if stage % 2 == 0 {
                second
            } else {
                original.as_ref().unwrap().0.clone()
            });
        } else {
            match action {
                0 => {
                    std::fs::create_dir_all(&directory)?;
                    let loaded = self.loaded.as_ref().unwrap();
                    self.smoke.as_mut().unwrap().workflow_original = Some((
                        loaded.path.clone(),
                        loaded.source_sha256.clone(),
                        self.dimensions(),
                    ));
                }
                1 => {
                    self.fit = false;
                    self.zoom = 1.0;
                    self.pan = Vec2::ZERO;
                }
                2 => {
                    self.fit = false;
                    self.zoom = 4.0;
                    self.pan = Vec2::new(200.0, 100.0);
                }
                3 => {
                    self.fit = true;
                    self.pan = Vec2::ZERO;
                }
                8 => self.start_edit(EditOp::Rotate90, ctx),
                9 => {
                    let [w, h] = original.as_ref().unwrap().2;
                    if self.dimensions() != [h, w] {
                        return Err(ToolError::Invalid("QA rotation dimensions mismatch".into()));
                    }
                    self.undo_edit(ctx);
                }
                10 => {
                    let [w, h] = original.as_ref().unwrap().2;
                    if self.dimensions() != [w, h] {
                        return Err(ToolError::Invalid("QA undo dimensions mismatch".into()));
                    }
                    self.start_edit(
                        EditOp::Crop {
                            x: w / 4,
                            y: h / 4,
                            width: (w / 2).max(1),
                            height: (h / 2).max(1),
                        },
                        ctx,
                    );
                }
                11 => {
                    self.format = ExportFormat::Png;
                    self.begin_export();
                    self.export_name = directory
                        .join("workflow-crop.png")
                        .to_string_lossy()
                        .into_owned();
                    self.start_export(ctx);
                }
                12 => {
                    let png = light_image_tool::pipeline::load(
                        &directory.join("workflow-crop.png"),
                        &Cancellation::default(),
                    )?;
                    if png.pixels.as_raw() != self.pixels.as_ref().unwrap().as_raw()
                        || png.pixels.dimensions() != self.pixels.as_ref().unwrap().dimensions()
                    {
                        return Err(ToolError::Invalid("QA PNG pixels mismatch".into()));
                    }
                    self.close_export();
                    self.format = ExportFormat::Jpeg;
                    self.begin_export();
                    self.export_name = directory
                        .join("workflow-crop.jpg")
                        .to_string_lossy()
                        .into_owned();
                    self.start_export(ctx);
                }
                13 => {
                    let jpeg = light_image_tool::pipeline::load(
                        &directory.join("workflow-crop.jpg"),
                        &Cancellation::default(),
                    )?;
                    if jpeg.pixels.dimensions() != self.pixels.as_ref().unwrap().dimensions()
                        || jpeg.pixels.pixels().any(|p| p[3] != 255)
                        || !jpeg.color_managed
                    {
                        return Err(ToolError::Invalid(
                            "QA JPEG dimensions/alpha/ICC mismatch".into(),
                        ));
                    }
                    let (path, hash, _) = original.as_ref().unwrap();
                    let source = light_image_tool::pipeline::load(path, &Cancellation::default())?;
                    if &source.source_sha256 != hash {
                        return Err(ToolError::Invalid("QA original identity changed".into()));
                    }
                    self.pixels = self.loaded.as_ref().map(|loaded| loaded.pixels.clone());
                    self.undo.clear();
                    self.edited = false;
                    self.refresh_texture(ctx);
                    let [w, h] = self.dimensions();
                    self.crop = [0, 0, w, h];
                    self.resize = [w, h];
                    self.fit = true;
                    self.pan = Vec2::ZERO;
                }
                _ => {
                    let dimensions = original.as_ref().unwrap().2;
                    if self.dimensions() != dimensions || self.edited {
                        return Err(ToolError::Invalid("QA restore failed".into()));
                    }
                    let result = serde_json::json!({"success":true,"native_input_tested":false,"original_unchanged":true,"png_exact_pixels":true,"jpeg_dimensions_alpha_icc":true,"records":self.smoke.as_ref().unwrap().workflow_records});
                    std::fs::write(
                        directory.join("workflow.json"),
                        serde_json::to_vec_pretty(&result)
                            .map_err(|error| ToolError::Invalid(error.to_string()))?,
                    )?;
                    self.smoke.as_mut().unwrap().workflow_result = Some(result);
                    return Ok(());
                }
            }
        }
        let record = serde_json::json!({"stage":stage,"elapsed_app_ms":self.start.elapsed().as_millis(),"frames":self.frames,"dimensions":self.dimensions(),"logical_owned_bytes":light_image_tool::memory::in_use()});
        let smoke = self.smoke.as_mut().unwrap();
        smoke.workflow_records.push(record);
        smoke.workflow_stage += 1;
        smoke.workflow_frame = self.frames;
        smoke.workflow_not_before = Instant::now()
            + std::time::Duration::from_millis(if opening {
                smoke.workflow_delay_ms.min(2000)
            } else {
                0
            });
        if stage % 20 == 0 {
            std::fs::write(
                directory.join("workflow-progress.json"),
                serde_json::to_vec_pretty(&smoke.workflow_records)
                    .map_err(|e| ToolError::Invalid(e.to_string()))?,
            )?;
        }
        ctx.request_repaint();
        Ok(())
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.path_dialog {
            let mut visible = true;
            let mut open = false;
            egui::Window::new("打开本地路径")
                .open(&mut visible)
                .resizable(false)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(480.0)
                .max_width((ctx.viewport_rect().width() - 32.0).max(200.0))
                .show(ctx, |ui| {
                    ui.label("粘贴或输入 JPEG / PNG 的完整路径");
                    accessible_path_input(
                        ui,
                        &mut self.open_path,
                        "open-path-input",
                        ui.available_width(),
                    );
                    open = ui.add(ActionButton::new(Icon::Open, "打开")).clicked();
                });
            self.path_dialog = visible;
            if open {
                self.open(PathBuf::from(self.open_path.trim().trim_matches('"')));
                self.path_dialog = false;
            }
        }
        if self.export_dialog {
            let mut visible = true;
            let mut save = false;
            egui::Window::new("导出新副本")
                .open(&mut visible)
                .resizable(false)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(480.0)
                .max_width((ctx.viewport_rect().width() - 32.0).max(200.0))
                .max_height((ctx.viewport_rect().height() - 60.0).max(120.0))
                .show(ctx, |ui| {
                    ui.label("原图保持不变。已存在的文件绝不会被覆盖。");
                    egui::ScrollArea::vertical()
                        .id_salt("export-dialog-options")
                        .max_height((ctx.viewport_rect().height() - 200.0).max(80.0))
                        .show(ui, |ui| {
                            let before = self.format;
                            ui.horizontal(|ui| {
                                ui.selectable_value(
                                    &mut self.format,
                                    ExportFormat::Png,
                                    "PNG（保留透明度）",
                                );
                                ui.selectable_value(&mut self.format, ExportFormat::Jpeg, "JPEG");
                            });
                            if before != self.format
                                && let Some(l) = &self.loaded
                            {
                                self.export_name = export::suggest_path(&l.path, self.format)
                                    .to_string_lossy()
                                    .into();
                            }
                            if self.format == ExportFormat::Jpeg {
                                ui.add(egui::Slider::new(&mut self.quality, 1..=100).text("质量"));
                                if self.current_has_alpha {
                                    ui.horizontal(|ui| {
                                        ui.label("透明区域背景（缩略预览）");
                                        ui.color_edit_button_srgb(&mut self.matte);
                                    });
                                }
                            }
                            ui.label("新文件完整路径");
                            accessible_path_input(
                                ui,
                                &mut self.export_name,
                                "export-path-input",
                                ui.available_width(),
                            );
                            if ui
                                .add(ActionButton::new(Icon::Open, "选择保存位置…"))
                                .clicked()
                            {
                                let ext = if self.format == ExportFormat::Png {
                                    "png"
                                } else {
                                    "jpg"
                                };
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_title("gloamere-image · 导出新副本")
                                    .set_file_name(
                                        PathBuf::from(&self.export_name)
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy(),
                                    )
                                    .add_filter(ext, &[ext])
                                    .save_file()
                                {
                                    self.export_name = path.to_string_lossy().into();
                                }
                            }
                            if self.format == ExportFormat::Jpeg && self.current_has_alpha {
                                if let Some(texture) = &self.preview_texture {
                                    ui.add(
                                        egui::Image::new(texture).max_size(Vec2::new(360.0, 180.0)),
                                    );
                                    ui.small("取样预览，仅展示底色合成；不模拟 JPEG 压缩");
                                } else {
                                    ui.small("正在生成底色预览…");
                                }
                            }
                            ui.small(
                                "副本不继承原 EXIF、位置隐私或 C2PA 有效签名。当前按 sRGB 输出。",
                            );
                        });
                    ui.separator();
                    save = ui
                        .add_enabled(
                            self.work.is_none(),
                            ActionButton::new(Icon::Export, "导出副本"),
                        )
                        .clicked();
                });
            self.export_dialog = visible;
            if save {
                self.start_export(ctx);
            }
        }
        if let Some(error) = self.error.clone() {
            let mut visible = true;
            egui::Window::new("操作未完成")
                .open(&mut visible)
                .resizable(false)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .default_width(440.0)
                .max_width((ctx.viewport_rect().width() - 32.0).max(200.0))
                .show(ctx, |ui| {
                    ui.label(error);
                    ui.small("原文件未被修改。请检查路径、文件格式与可用空间。");
                });
            if !visible {
                self.error = None;
            }
        }
    }
}

impl eframe::App for ImageApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if self
            .work
            .as_ref()
            .is_some_and(|work| work.export_thread.is_some())
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if let Err(error) = self.qa_workflow(ctx) {
            if let Some(smoke) = &mut self.smoke {
                smoke.workflow_result = Some(
                    serde_json::json!({"success":false,"error":error.to_string(),"stage":smoke.workflow_stage}),
                );
                let _ = std::fs::write(
                    smoke.directory.join("workflow.json"),
                    serde_json::to_vec_pretty(&smoke.workflow_result).unwrap_or_default(),
                );
            }
            self.error = Some(error.to_string());
        }
        let qa_state = self.smoke.as_ref().and_then(|s| {
            (!s.state_prepared && !self.loading && self.loaded.is_some()).then_some((
                s.show_edit,
                s.show_export,
                s.show_error,
                s.jpeg_preview,
                s.crop_preview,
            ))
        });
        if let Some((editing, exporting, error, jpeg, crop_preview)) = qa_state {
            self.smoke.as_mut().unwrap().state_prepared = true;
            self.editing = editing || crop_preview;
            if crop_preview {
                let [w, h] = self.dimensions();
                self.crop = [w / 4, h / 4, (w / 2).max(1), (h / 2).max(1)];
                self.crop_selecting = true;
            }
            if exporting {
                if jpeg {
                    self.format = ExportFormat::Jpeg;
                }
                self.begin_export();
            }
            if error {
                self.error =
                    Some("QA 渲染检查：目标名称已存在，请选择新的副本名称。原图未修改。".into());
            }
            ctx.request_repaint();
        }
        if self.smoke.is_some() {
            ctx.input(|i| {
                log::info!(
                    "smoke logic elapsed_ms={} frames={} loading={} viewport={:?}",
                    self.start.elapsed().as_millis(),
                    self.frames,
                    self.loading,
                    i.raw.viewports.get(&egui::ViewportId::ROOT)
                )
            });
        }
        if let Some(smoke) = &mut self.smoke
            && smoke.idle_start.is_some()
            && !smoke.idle_wake.load(std::sync::atomic::Ordering::Acquire)
            && !ctx.input(|i| i.raw.focused)
        {
            smoke.idle_lost_focus = true;
        }
        if let Some(smoke) = &self.smoke {
            let screenshots = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| {
                        if let egui::Event::Screenshot { image, .. } = e {
                            Some(image.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
            });
            for screenshot in screenshots {
                let result = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
                    std::fs::create_dir_all(&smoke.directory)?;
                    let bytes: Vec<u8> = screenshot
                        .pixels
                        .iter()
                        .flat_map(|p| p.to_array())
                        .collect();
                    image::save_buffer(
                        smoke.directory.join("window.png"),
                        &bytes,
                        screenshot.size[0] as u32,
                        screenshot.size[1] as u32,
                        image::ColorType::Rgba8,
                    )?;
                    let mut report = serde_json::json!({"adapter":self.adapter,"image_dimensions":self.dimensions(),"image_loaded":self.loaded.is_some(),"error":self.error,"frames":self.frames,"elapsed_ms":self.start.elapsed().as_millis(),"ui_scale":smoke.scale,"ui_input_tested":false,"workflow":smoke.workflow_result,"idle_seconds":smoke.idle_seconds,"idle_frame_delta":smoke.idle_frames,"idle_focus_start":smoke.idle_focus_start,"idle_lost_focus":smoke.idle_lost_focus,"resident_estimate_bytes":self.resident_estimate(),"owned_budget_bytes":light_image_tool::memory::in_use(),"owned_budget_peak_bytes":light_image_tool::memory::peak(),"evidence":self.evidence});
                    report["browser"] = serde_json::json!({"list_visible":self.preferences.list_visible,"directory_count":self.siblings.len(),"position":self.position,"directory_scanning":self.directory_scanning,"warning":self.directory_warning,"thumbnail_cache_entries":self.thumbnail_cache.len(),"thumbnail_busy":self.thumbnail_busy.is_some(),"background":self.preferences.background});
                    report["memory_breakdown"] = self.memory_breakdown();
                    report["viewer_ui"] = serde_json::json!({"toolbar_buttons":self.toolbar_layout.as_ref().map(|layout| layout.buttons.map(|r| [r.min.x,r.min.y,r.max.x,r.max.y])),"more_menu_open":self.more_open,"single_row_toolbar":true});
                    report["canvas"] = serde_json::json!({"image_rect":self.canvas_image_rect.map(|r| [r.min.x,r.min.y,r.max.x,r.max.y]),"crop":self.crop,"crop_selecting":self.crop_selecting,"zoom":self.zoom,"pan":[self.pan.x,self.pan.y]});
                    report["native_pixels_per_point"] =
                        serde_json::json!(ctx.native_pixels_per_point());
                    std::fs::write(
                        smoke.directory.join("runtime.json"),
                        serde_json::to_vec_pretty(&report)?,
                    )?;
                    Ok(())
                })();
                match result {
                    Ok(()) => log::info!(
                        "Windows rendering smoke saved to {}",
                        smoke.directory.display()
                    ),
                    Err(e) => log::error!("smoke output failed: {e}"),
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.frames += 1;
        if self.smoke.is_some() && self.frames <= 12 {
            log::info!(
                "smoke frame={} elapsed_ms={} loading={} evidence_ready={} idle_started={}",
                self.frames,
                self.start.elapsed().as_millis(),
                self.loading,
                self.evidence.is_some(),
                self.smoke.as_ref().is_some_and(|s| s.idle_start.is_some())
            );
        }
        let ctx = ui.ctx().clone();
        let more_was_open = self.more_open;
        if self.restore_more_focus {
            ctx.request_repaint();
        }
        if self.restore_more_focus
            && !self.more_open
            && ctx.memory(|memory| memory.top_modal_layer().is_none())
        {
            ctx.memory_mut(|memory| memory.request_focus(viewer_ui::icon_id(Icon::More)));
            self.restore_more_focus = false;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.more_open {
                self.more_open = false;
                ctx.memory_mut(|memory| memory.request_focus(viewer_ui::icon_id(Icon::More)));
            } else if self.error.is_some() {
                self.error = None;
            } else if self.path_dialog {
                self.path_dialog = false;
            } else if self.export_dialog {
                self.close_export();
            } else if self.crop_selecting {
                self.crop_selecting = false;
                self.crop_anchor = None;
            } else if self.editing {
                self.editing = false;
            } else if self.info_open {
                self.info_open = false;
            } else if self.fullscreen {
                self.fullscreen = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
        }
        if self.info_open || self.editing {
            self.start_provenance(&ctx);
        }
        let viewer_keyboard =
            !ctx.egui_wants_keyboard_input() || viewer_ui::command_has_focus(&ctx);
        if (viewer_keyboard || self.more_open)
            && !self.path_dialog
            && !self.export_dialog
            && self.error.is_none()
            && ctx.input(|i| i.key_pressed(egui::Key::F10))
        {
            self.more_open = !self.more_open;
            if !self.more_open {
                ctx.memory_mut(|memory| memory.request_focus(viewer_ui::icon_id(Icon::More)));
            }
        }
        if self.more_open {
            ctx.memory_mut(|memory| {
                memory.set_modal_layer(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("viewer-more-menu"),
                ))
            });
        }
        if viewer_keyboard
            && !self.more_open
            && !self.path_dialog
            && !self.export_dialog
            && self.error.is_none()
        {
            if ctx
                .input(|i| i.key_pressed(egui::Key::R) && !i.modifiers.command && !i.modifiers.alt)
            {
                let operation = if ctx.input(|i| i.modifiers.shift) {
                    EditOp::Rotate270
                } else {
                    EditOp::Rotate90
                };
                self.start_edit(operation, &ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::PageDown)) {
                self.navigate(1);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::PageUp)) {
                self.navigate(-1);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Home)) {
                self.select_image(0);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::End)) && !self.siblings.is_empty() {
                self.select_image(self.siblings.len() - 1);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                self.navigate(1);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                self.navigate(-1);
            }
            if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
                self.open_dialog();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F)) {
                self.fit = true;
                self.pan = Vec2::ZERO;
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Num1)) {
                self.fit = false;
                self.zoom = 1.0;
                self.pan = Vec2::ZERO;
            }
            if ctx
                .input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::S))
            {
                self.begin_export();
            }
            if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z)) {
                self.undo_edit(&ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
                self.fullscreen = !self.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
        }
        if let Some(path) =
            ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()))
        {
            self.open(path);
        }
        self.update_preview(&ctx);
        let dialog_open = self.path_dialog || self.export_dialog || self.error.is_some();
        egui::Panel::bottom("viewer-toolbar")
            .exact_size(viewer_ui::TOOLBAR_HEIGHT)
            .frame(egui::Frame::NONE.fill(Color32::WHITE))
            .show(ui, |ui| self.toolbar(ui));
        if self.preferences.list_visible && !self.siblings.is_empty() {
            egui::Panel::left("image-list")
                .default_size(210.0)
                .min_size(160.0)
                .max_size(300.0)
                .resizable(true)
                .show(ui, |ui| {
                    if dialog_open {
                        ui.disable();
                    }
                    self.image_list(ui);
                });
        }
        if self.info_open || self.editing {
            egui::Panel::right("inspector")
                .default_size(280.0)
                .min_size(240.0)
                .show(ui, |ui| {
                    if dialog_open {
                        ui.disable();
                    }
                    let sticky_crop = self.editing && self.crop_selecting;
                    #[cfg(test)]
                    {
                        self.crop_apply_rect = None;
                    }
                    let footer_height = if sticky_crop { 140.0 } else { 0.0 };
                    let body_height = (ui.available_height() - footer_height).max(0.0);
                    egui::ScrollArea::vertical()
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                        )
                        .max_height(body_height)
                        .show(ui, |ui| self.inspector(ui));
                    if sticky_crop {
                        ui.add_space((body_height - ui.min_rect().height()).max(0.0));
                        ui.separator();
                        ui.label(format!("选区 {} × {} 像素", self.crop[2], self.crop[3]));
                        let response = ui.add_enabled(
                            self.work.is_none() && !self.loading,
                            CompactAction::new(Icon::CropApply, "应用裁剪"),
                        );
                        #[cfg(test)]
                        {
                            self.crop_apply_rect = Some(response.rect);
                        }
                        if response.clicked() {
                            self.apply_crop(&ctx);
                        }
                        ui.small("Esc 返回浏览平移");
                    }
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let area = ui.available_rect_before_wrap();
                self.canvas(ui);
                if !self.siblings.is_empty() {
                    let label = format!(
                        "{} / {}{}",
                        self.position + 1,
                        self.siblings.len(),
                        if self.edited { " · 已编辑" } else { "" }
                    );
                    let width = if self.edited { 144.0 } else { 90.0 };
                    let rect = Rect::from_min_size(
                        area.right_top() + Vec2::new(-width - 16.0, 16.0),
                        Vec2::new(width, 27.0),
                    );
                    ui.painter()
                        .rect_filled(rect, 14.0, Color32::from_black_alpha(145));
                    ui.put(
                        rect,
                        egui::Label::new(
                            egui::RichText::new(label).size(13.0).color(Color32::WHITE),
                        ),
                    );
                }
                let busy = self.loading || self.work.is_some();
                if busy
                    || self
                        .notice_until
                        .is_some_and(|until| Instant::now() < until)
                {
                    let rect = Rect::from_center_size(
                        area.center_bottom() - Vec2::new(0.0, 25.0),
                        Vec2::new(area.width().min(420.0) - 24.0, 34.0),
                    );
                    ui.painter()
                        .rect_filled(rect, 7.0, Color32::from_white_alpha(242));
                    let cancel_rect = Rect::from_center_size(
                        Pos2::new(rect.right() - 18.0, rect.center().y),
                        Vec2::splat(viewer_ui::ACTION_SIZE),
                    );
                    let text_rect = Rect::from_min_max(
                        rect.min + Vec2::new(8.0, 0.0),
                        Pos2::new(
                            if busy {
                                cancel_rect.left() - 4.0
                            } else {
                                rect.right() - 8.0
                            },
                            rect.bottom(),
                        ),
                    );
                    ui.put(text_rect, egui::Label::new(&self.status).truncate())
                        .on_hover_text(&self.status);
                    if busy
                        && ui
                            .put(cancel_rect, CompactAction::new(Icon::Close, "取消当前操作"))
                            .clicked()
                    {
                        self.load_cancel.cancel();
                        if let Some(work) = &self.work {
                            work.cancel.cancel();
                        }
                    }
                }
            });
        self.more_menu(&ctx);
        if more_was_open && !self.more_open {
            self.restore_more_focus =
                !self.path_dialog && !self.export_dialog && self.error.is_none();
            ctx.request_repaint();
        }
        self.dialogs(&ctx);
        if !ctx.input(|input| input.pointer.primary_down()) {
            self.more_trigger_release_blocked = false;
        }
        if let Some(smoke) = &mut self.smoke {
            if smoke.destroy_device
                && (!smoke.destroy_during_export
                    || (self.format == ExportFormat::Jpeg && self.work.is_some()))
                && !smoke.fault_requested
                && !self.loading
                && self.loaded.is_some()
                && self.frames >= 2
                && let Some(state) = frame.wgpu_render_state()
            {
                smoke.fault_requested = true;
                let device = state.device.clone();
                let wake = ctx.clone();
                let directory = smoke.directory.clone();
                let report = serde_json::json!({"adapter":self.adapter,"original_sha256":self.loaded.as_ref().unwrap().source_sha256,"frames":self.frames,"controlled_logical_device_destroy":true,"export_in_flight":self.work.is_some(),"export_format":format!("{:?}",self.format)});
                let _ = std::fs::write(
                    directory.join("gpu-loss-ready.json"),
                    serde_json::to_vec_pretty(&report).unwrap_or_default(),
                );
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    device.destroy();
                    let _ = device.poll(wgpu::PollType::Poll);
                    wake.request_repaint();
                });
            }
            let sources_ready =
                !smoke.show_info || self.evidence.is_some() || self.loaded.is_none();
            let sources_ready = sources_ready
                && (!smoke.jpeg_preview || self.preview_texture.is_some())
                && !self.directory_scanning
                && self.thumbnail_busy.is_none();
            let idle_ready = if smoke.idle_seconds > 0
                && !self.loading
                && sources_ready
                && self.frames >= 2
                && (smoke.workflow_image.is_none() || smoke.workflow_result.is_some())
            {
                if smoke.idle_start.is_none() {
                    smoke.idle_focus_start = Some(ctx.input(|i| i.raw.focused));
                    let wake = ctx.clone();
                    let signal = smoke.idle_wake.clone();
                    let seconds = smoke.idle_seconds;
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(seconds));
                        log::info!("smoke one-shot idle wake fired");
                        signal.store(true, std::sync::atomic::Ordering::Release);
                        wake.send_viewport_cmd(egui::ViewportCommand::Focus);
                        wake.request_repaint();
                    });
                }
                let (started, frame) = *smoke
                    .idle_start
                    .get_or_insert((Instant::now(), self.frames));
                let duration = std::time::Duration::from_secs(smoke.idle_seconds);
                if started.elapsed() >= duration
                    && smoke.idle_wake.load(std::sync::atomic::Ordering::Acquire)
                {
                    smoke.idle_frames = Some(self.frames - frame);
                    true
                } else {
                    false
                }
            } else {
                smoke.idle_seconds == 0
            };
            if !self.loading
                && self.frames >= 2
                && sources_ready
                && idle_ready
                && !smoke.requested
                && !smoke.destroy_device
                && (smoke.workflow_image.is_none() || smoke.workflow_result.is_some())
            {
                smoke.requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                if let Some(state) = frame.wgpu_render_state() {
                    let device = state.device.clone();
                    let wake = ctx.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        let result = device.poll(wgpu::PollType::Wait {
                            submission_index: None,
                            timeout: Some(std::time::Duration::from_secs(2)),
                        });
                        log::info!("smoke one-shot screenshot GPU poll: {result:?}");
                        wake.request_repaint();
                    });
                }
            }
        }
        // No periodic repaint while idle. Exports poll only while active; other workers wake once.
    }
    fn on_exit(&mut self) {
        self.shutdown_background();
        log::info!(
            "shutdown: frames={} elapsed_s={:.3}",
            self.frames,
            self.start.elapsed().as_secs_f64()
        );
    }
}

impl Drop for ImageApp {
    fn drop(&mut self) {
        self.shutdown_background();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    fn click_at(app: &mut ImageApp, ctx: &egui::Context, position: Pos2, size: Vec2) {
        for pressed in [true, false] {
            pointer_ui_size(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                size,
            );
        }
        pointer_ui_size(app, ctx, vec![], size);
    }
    fn click_icon(app: &mut ImageApp, ctx: &egui::Context, icon: Icon, size: Vec2) {
        pointer_ui_size(app, ctx, vec![], size);
        let index = Icon::ALL
            .iter()
            .position(|candidate| *candidate == icon)
            .unwrap();
        click_at(
            app,
            ctx,
            app.toolbar_layout.as_ref().unwrap().buttons[index].center(),
            size,
        );
    }
    #[test]
    fn minimal_viewer_defaults_and_toolbar_leave_canvas_dominant() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("minimal.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        configure(&ctx);
        assert!(!app.preferences.list_visible && !app.editing && !app.info_open);
        assert_eq!(app.preferences.background, CanvasBackground::CheckerLight);
        for size in [
            Vec2::new(800.0, 550.0),
            Vec2::new(1180.0, 780.0),
            Vec2::new(400.0, 275.0),
        ] {
            pointer_ui_size(&mut app, &ctx, vec![], size);
            let layout = app.toolbar_layout.as_ref().unwrap();
            for button in layout.buttons {
                assert!(
                    Rect::from_min_size(Pos2::ZERO, size).contains_rect(button),
                    "screen={size:?}, button={button:?}"
                );
            }
            assert!(
                layout
                    .buttons
                    .iter()
                    .all(|button| button.bottom() > size.y - 52.0)
            );
            assert!(app.canvas_image_rect.unwrap().center().y < size.y * 0.5);
        }
    }
    #[test]
    fn toolbar_pointer_zoom_rotate_undo_and_export_use_existing_actions() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("controls.png");
        fixture(&source, false);
        let bytes = std::fs::read(&source).unwrap();
        let (mut app, ctx) = app_with(&source);
        configure(&ctx);
        let size = Vec2::new(800.0, 550.0);
        click_icon(&mut app, &ctx, Icon::Plus, size);
        assert!(!app.fit && app.zoom > 1.0);
        click_icon(&mut app, &ctx, Icon::Minus, size);
        assert_eq!(app.zoom, 1.0);
        click_icon(&mut app, &ctx, Icon::Fit, size);
        assert!(app.fit);
        click_icon(&mut app, &ctx, Icon::Actual, size);
        assert!(!app.fit && app.zoom == 1.0);
        click_icon(&mut app, &ctx, Icon::Rotate, size);
        wait(&mut app, &ctx);
        assert_eq!(app.dimensions(), [3, 4]);
        assert!(app.edited);
        click_icon(&mut app, &ctx, Icon::Undo, size);
        wait(&mut app, &ctx);
        assert_eq!(app.dimensions(), [4, 3]);
        click_icon(&mut app, &ctx, Icon::Export, size);
        assert!(app.export_dialog);
        assert_eq!(std::fs::read(source).unwrap(), bytes);
    }
    #[test]
    fn more_menu_closes_after_selection_and_outside_click() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("menu.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        configure(&ctx);
        let size = Vec2::new(800.0, 550.0);
        click_icon(&mut app, &ctx, Icon::More, size);
        assert!(app.more_open);
        let menu = app.menu_rect.unwrap();
        assert!(Rect::from_min_size(Pos2::ZERO, size).contains_rect(menu));
        let list = app.menu_list_rect.unwrap().center();
        click_at(&mut app, &ctx, list, size);
        assert!(app.preferences.list_visible && !app.more_open);
        click_icon(&mut app, &ctx, Icon::More, size);
        let edit = app.menu_edit_rect.unwrap().center();
        click_at(&mut app, &ctx, edit, size);
        assert!(app.editing && !app.more_open);
        click_icon(&mut app, &ctx, Icon::More, size);
        assert!(app.more_open);
        click_at(&mut app, &ctx, Pos2::new(12.0, 12.0), size);
        assert!(!app.more_open && app.editing);
    }
    #[test]
    fn menu_blocks_viewer_shortcuts_and_escape_only_closes_top_layer() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("menu-keys.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        app.more_open = true;
        app.editing = true;
        app.fullscreen = true;
        raw_ui(&mut app, &ctx, Some(egui::Key::R), egui::Modifiers::NONE);
        assert!(app.work.is_none());
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!app.more_open && app.editing && app.fullscreen);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!app.editing && app.fullscreen);
    }
    #[test]
    fn toolbar_keyboard_focus_keeps_viewer_shortcuts_available() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("focus.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        raw_ui(&mut app, &ctx, None, egui::Modifiers::NONE);
        ctx.memory_mut(|memory| memory.request_focus(viewer_ui::icon_id(Icon::Rotate)));
        raw_ui(&mut app, &ctx, Some(egui::Key::R), egui::Modifiers::NONE);
        wait(&mut app, &ctx);
        assert_eq!(app.dimensions(), [3, 4]);
        ctx.memory_mut(|memory| memory.request_focus(viewer_ui::icon_id(Icon::More)));
        raw_ui(&mut app, &ctx, Some(egui::Key::F10), egui::Modifiers::NONE);
        assert!(app.more_open);
    }

    fn fixture(path: &std::path::Path, invalid_icc: bool) {
        let pixels = image::RgbaImage::from_pixel(4, 3, image::Rgba([80, 90, 100, 128]));
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        if invalid_icc {
            encoder
                .set_icc_profile(b"invalid ICC fixture".to_vec())
                .unwrap();
        }
        encoder
            .write_image(pixels.as_raw(), 4, 3, image::ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    fn app_with(path: &std::path::Path) -> (ImageApp, egui::Context) {
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "headless-test".into(), None, None);
        let loaded = light_image_tool::pipeline::load(path, &Cancellation::default()).unwrap();
        app.pixels = Some(loaded.pixels.clone());
        app.loaded = Some(loaded);
        (app, ctx)
    }
    fn wait(app: &mut ImageApp, ctx: &egui::Context) {
        let start = Instant::now();
        while app.loading || app.work.is_some() || app.preview_task.is_some() {
            assert!(start.elapsed() < std::time::Duration::from_secs(5));
            app.poll(ctx);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    #[test]
    fn final_directory_row_is_fully_visible_in_minimum_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("last.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        configure(&ctx);
        app.siblings = (0..70)
            .map(|index| dir.path().join(format!("{index:03}.png")))
            .chain(std::iter::once(path))
            .collect();
        app.position = 70;
        app.preferences.list_visible = true;
        app.requested_path = app.loaded.as_ref().map(|loaded| loaded.path.clone());
        app.locate_current = true;
        app.info_open = true;
        for _ in 0..8 {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 550.0))),
                ..Default::default()
            };
            let mut frame = eframe::Frame::_new_kittest();
            let mut output = ctx.run_ui(input, |ui| eframe::App::ui(&mut app, ui, &mut frame));
            output.textures_delta.clear();
        }
        let viewport = app.list_viewport_rect.unwrap();
        let selected = app.selected_list_row_rect.unwrap();
        assert!(selected.min.y >= viewport.min.y - 1.0);
        assert!(selected.max.y <= viewport.max.y + 1.0);
    }
    #[test]
    fn rotation_shortcuts_cycle_and_undo_without_touching_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rotate.png");
        let image = image::RgbaImage::from_fn(7, 3, |x, y| {
            image::Rgba([
                (x * 23) as u8,
                (y * 51) as u8,
                (x + y) as u8,
                if x == 0 { 0 } else { 255 },
            ])
        });
        image.save(&path).unwrap();
        let original_bytes = std::fs::read(&path).unwrap();
        let (mut app, ctx) = app_with(&path);
        let original = app.pixels.as_ref().unwrap().as_raw().clone();
        for _ in 0..4 {
            pointer_ui(
                &mut app,
                &ctx,
                vec![egui::Event::Key {
                    key: egui::Key::R,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            wait(&mut app, &ctx);
            pointer_ui(
                &mut app,
                &ctx,
                vec![egui::Event::Key {
                    key: egui::Key::R,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        assert_eq!(app.dimensions(), [7, 3]);
        assert_eq!(app.pixels.as_ref().unwrap().as_raw(), &original);
        assert!(app.edited);
        app.undo_edit(&ctx);
        assert_eq!(app.dimensions(), [3, 7]);
        assert_eq!(std::fs::read(path).unwrap(), original_bytes);
    }
    #[test]
    fn canvas_settings_never_change_pixels_or_export_matte() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("alpha.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        let original = app.pixels.as_ref().unwrap().as_raw().clone();
        let matte = app.matte;
        let mut exports = Vec::new();
        let mut jpeg_exports = Vec::new();
        for (index, background) in CanvasBackground::ALL.into_iter().enumerate() {
            app.preferences.background = background;
            pointer_ui(&mut app, &ctx, vec![]);
            assert_eq!(app.pixels.as_ref().unwrap().as_raw(), &original);
            assert_eq!(app.matte, matte);
            let destination = dir.path().join(format!("background-{index}.png"));
            export::save_new(
                app.pixels.as_ref().unwrap(),
                &source,
                &ExportOptions {
                    destination: destination.clone(),
                    format: ExportFormat::Png,
                    quality: 92,
                    matte,
                },
                &Cancellation::default(),
            )
            .unwrap();
            exports.push(image::open(destination).unwrap().to_rgba8());
            let jpeg_destination = dir.path().join(format!("background-{index}.jpg"));
            export::save_new(
                app.pixels.as_ref().unwrap(),
                &source,
                &ExportOptions {
                    destination: jpeg_destination.clone(),
                    format: ExportFormat::Jpeg,
                    quality: 92,
                    matte,
                },
                &Cancellation::default(),
            )
            .unwrap();
            jpeg_exports.push(image::open(jpeg_destination).unwrap().to_rgb8());
        }
        assert!(exports.iter().all(|image| image.as_raw() == &original));
        assert!(exports.windows(2).all(|p| p[0] == p[1]));
        assert!(jpeg_exports.windows(2).all(|p| p[0] == p[1]));
    }
    #[test]
    fn modal_blocks_rotation_and_directory_navigation() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.png");
        fixture(&source, false);
        let second = dir.path().join("b.png");
        fixture(&second, false);
        let (mut app, ctx) = app_with(&source);
        let generation = app.generation;
        app.siblings = vec![source, second];
        app.export_dialog = true;
        pointer_ui(
            &mut app,
            &ctx,
            vec![
                egui::Event::Key {
                    key: egui::Key::R,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::Key {
                    key: egui::Key::End,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert!(app.work.is_none());
        assert_eq!(app.generation, generation);
    }
    #[test]
    fn thumbnails_old_directory_work_cannot_enter_new_cache() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let old = first.join("a.png");
        let new = second.join("b.png");
        fixture(&old, false);
        fixture(&new, false);
        let (mut app, ctx) = app_with(&old);
        app.thumbnail_sequence += 1;
        app.thumbnail_busy = Some((app.thumbnail_sequence, old.clone()));
        app.thumbnail_worker.request(BrowserRequest {
            generation: app.thumbnail_sequence,
            path: old,
            cancel: Cancellation::default(),
        });
        app.open(new.clone());
        wait(&mut app, &ctx);
        for _ in 0..40 {
            app.poll_browser(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(app.thumbnail_cache.is_empty());
        assert_eq!(app.loaded.as_ref().unwrap().path, new);
        assert!(
            app.siblings
                .iter()
                .all(|p| p.parent() == Some(second.as_path()))
        );
    }
    #[test]
    fn thumbnail_cache_is_bounded_and_cancelled_or_stale_replies_do_not_poison_it() {
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "headless".into(), None, None);
        for index in 0..100u64 {
            let path = PathBuf::from(format!("{index}.png"));
            app.frames = index;
            app.thumbnail_busy = Some((index, path.clone()));
            app.accept_thumbnail(
                browsing::Response {
                    generation: index,
                    path,
                    result: Ok(image::RgbaImage::new(96, 96)),
                },
                &ctx,
            );
            assert!(app.thumbnail_cache.len() <= browsing::THUMBNAIL_CACHE_ENTRIES);
        }
        assert_eq!(app.thumbnail_cache.len(), 48);
        assert!(app.thumbnail_cache.iter().all(|entry| {
            entry
                .path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .parse::<u64>()
                .unwrap()
                >= 52
        }));
        let current = PathBuf::from("current.png");
        app.thumbnail_busy = Some((102, current.clone()));
        app.accept_thumbnail(
            browsing::Response {
                generation: 101,
                path: current.clone(),
                result: Ok(image::RgbaImage::new(96, 96)),
            },
            &ctx,
        );
        assert_eq!(app.thumbnail_busy.as_ref().unwrap().0, 102);
        app.accept_thumbnail(
            browsing::Response {
                generation: 102,
                path: current.clone(),
                result: Err(ToolError::Cancelled),
            },
            &ctx,
        );
        assert!(
            !app.thumbnail_cache
                .iter()
                .any(|entry| entry.path == current)
        );
        assert!(app.thumbnail_busy.is_none());
    }
    #[test]
    fn failed_cross_directory_open_restores_displayed_list_selection() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        let missing = dir.path().join("other/missing.png");
        app.open(missing);
        wait(&mut app, &ctx);
        assert!(app.error.is_some());
        assert_eq!(app.requested_path.as_ref(), Some(&source));
        assert_eq!(app.directory_path.as_ref(), Some(&dir.path().to_path_buf()));
        assert_eq!(app.loaded.as_ref().unwrap().path, source);
    }
    #[test]
    fn same_directory_open_outside_cap_and_late_listing_keep_current_item() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("current.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        app.directory_worker.cancel();
        app.siblings = (0..browsing::MAX_DIRECTORY_IMAGES)
            .map(|i| dir.path().join(format!("{i:05}.png")))
            .collect();
        app.prepare_browser(&source);
        assert_eq!(app.siblings[app.position], source);
        assert!(app.siblings.len() <= browsing::MAX_DIRECTORY_IMAGES);
        // The same merge used by a late capped listing must keep the latest selection.
        let incoming = (0..browsing::MAX_DIRECTORY_IMAGES)
            .map(|i| dir.path().join(format!("{i:05}.png")))
            .collect();
        app.siblings =
            browsing::ensure_current(incoming, app.requested_path.as_ref().unwrap()).unwrap();
        app.position = app
            .siblings
            .iter()
            .position(|p| p == app.requested_path.as_ref().unwrap())
            .unwrap();
        assert_eq!(app.siblings[app.position], source);
        pointer_ui(&mut app, &ctx, vec![]);
    }
    #[test]
    fn thumbnail_busy_failure_has_a_bounded_retry_and_success_clears_it() {
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "headless".into(), None, None);
        let path = PathBuf::from("busy.png");
        for id in 1..=3 {
            app.thumbnail_busy = Some((id, path.clone()));
            app.accept_thumbnail(
                browsing::Response {
                    generation: id,
                    path: path.clone(),
                    result: Err(ToolError::ResourceBusy("temporary fixture".into())),
                },
                &ctx,
            );
            let entry = app.thumbnail_cache.iter().find(|e| e.path == path).unwrap();
            assert_eq!(entry.retry_attempts, id as u8);
            assert_eq!(entry.retry_after.is_some(), id < 3);
        }
        app.thumbnail_busy = Some((4, path.clone()));
        app.accept_thumbnail(
            browsing::Response {
                generation: 4,
                path: path.clone(),
                result: Ok(image::RgbaImage::new(8, 8)),
            },
            &ctx,
        );
        let entry = app.thumbnail_cache.iter().find(|e| e.path == path).unwrap();
        assert!(entry.texture.is_some());
        assert!(entry.retry_after.is_none());
        assert_eq!(entry.retry_attempts, 0);
    }
    #[test]
    fn switching_to_invalid_icc_closes_and_rejects_old_export_intent() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png");
        let b = dir.path().join("B.png");
        fixture(&a, false);
        fixture(&b, true);
        let (mut app, ctx) = app_with(&a);
        app.begin_export();
        assert!(app.export_dialog);
        app.open(b);
        assert!(!app.export_dialog);
        assert!(app.export_generation.is_none());
        wait(&mut app, &ctx);
        assert!(!app.loaded.as_ref().unwrap().color_managed);
        app.export_dialog = true;
        app.export_generation = Some(app.generation);
        app.export_name = dir.path().join("forbidden.png").to_string_lossy().into();
        app.start_export(&ctx);
        assert!(app.work.is_none());
        assert!(app.error.is_some());
        assert!(!dir.path().join("forbidden.png").exists());
    }
    #[test]
    fn edit_replacement_invalidates_export_and_late_generation_cannot_replace_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png");
        fixture(&a, false);
        let (mut app, ctx) = app_with(&a);
        app.begin_export();
        app.start_edit(EditOp::Rotate90, &ctx);
        assert!(!app.export_dialog);
        wait(&mut app, &ctx);
        assert_eq!(app.dimensions(), [3, 4]);
        let (tx, rx) = mpsc::sync_channel(1);
        app.work = Some(Work {
            generation: app.generation.wrapping_sub(1),
            cancel: Cancellation::default(),
            receiver: rx,
            export_thread: None,
        });
        tx.send(WorkResult::Edited(Ok(
            light_image_tool::memory::Pixels::from_image(image::RgbaImage::new(1, 1)).unwrap(),
        )))
        .unwrap();
        app.poll(&ctx);
        assert_eq!(app.dimensions(), [3, 4]);
    }
    #[test]
    fn panic_and_disconnected_worker_recover_for_the_next_operation() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png");
        fixture(&a, false);
        let (mut app, ctx) = app_with(&a);
        let caught: light_image_tool::Result<PathBuf> =
            protected(|| panic!("injected encoder panic"), "test panic");
        assert!(caught.is_err());
        let (tx, rx) = mpsc::sync_channel(1);
        app.work = Some(Work {
            generation: app.generation,
            cancel: Cancellation::default(),
            receiver: rx,
            export_thread: None,
        });
        drop(tx);
        app.poll(&ctx);
        assert!(app.work.is_none());
        assert!(app.error.is_some());
        app.start_edit(EditOp::Rotate90, &ctx);
        wait(&mut app, &ctx);
        assert_eq!(app.dimensions(), [3, 4]);
    }
    #[test]
    fn jpeg_preview_does_not_rebuild_canvas_and_uses_current_matte() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png");
        fixture(&a, false);
        let (mut app, ctx) = app_with(&a);
        app.refresh_texture(&ctx);
        let ids: Vec<_> = app.tiles.iter().map(|t| t.texture.id()).collect();
        app.format = ExportFormat::Jpeg;
        app.begin_export();
        app.update_preview(&ctx);
        app.matte = [10, 20, 30];
        app.update_preview(&ctx);
        wait(&mut app, &ctx);
        app.update_preview(&ctx);
        wait(&mut app, &ctx);
        assert_eq!(app.preview_matte, Some([10, 20, 30]));
        assert!(app.preview_texture.is_some());
        assert_eq!(
            ids,
            app.tiles.iter().map(|t| t.texture.id()).collect::<Vec<_>>()
        );
    }
    fn raw_ui(
        app: &mut ImageApp,
        ctx: &egui::Context,
        key: Option<egui::Key>,
        modifiers: egui::Modifiers,
    ) {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1180.0, 780.0))),
            focused: true,
            ..Default::default()
        };
        input.events.push(egui::Event::ModifiersChanged(modifiers));
        if let Some(key) = key {
            input.events.push(egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            });
            input.events.push(egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers,
            });
        }
        let mut frame = eframe::Frame::_new_kittest();
        let mut output = ctx.run_ui(input, |ui| {
            app.poll(ctx);
            eframe::App::ui(app, ui, &mut frame);
        });
        output.textures_delta.clear(); // Headless test intentionally does not upload textures.
    }
    fn pointer_ui(app: &mut ImageApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        pointer_ui_size(app, ctx, events, Vec2::new(1180.0, 780.0));
    }
    fn pointer_ui_size(
        app: &mut ImageApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
        size: Vec2,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            focused: true,
            events,
            ..Default::default()
        };
        let mut frame = eframe::Frame::_new_kittest();
        let mut output = ctx.run_ui(input, |ui| {
            app.poll(ctx);
            eframe::App::ui(app, ui, &mut frame);
        });
        output.textures_delta.clear();
    }
    fn drag(
        app: &mut ImageApp,
        ctx: &egui::Context,
        start: Pos2,
        end: Pos2,
        button: egui::PointerButton,
    ) {
        pointer_ui(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        pointer_ui(app, ctx, vec![egui::Event::PointerMoved(end)]);
        pointer_ui(
            app,
            ctx,
            vec![egui::Event::PointerButton {
                pos: end,
                button,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
    }
    #[test]
    fn minimum_window_crop_confirmation_is_visible_and_clicks_shared_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("original.png");
        image::RgbaImage::from_pixel(100, 80, image::Rgba([80, 90, 100, 128]))
            .save(&path)
            .unwrap();
        let original = std::fs::read(&path).unwrap();
        let (mut app, ctx) = app_with(&path);
        app.refresh_texture(&ctx);
        app.editing = true;
        app.crop_selecting = true;
        app.crop = [25, 20, 50, 40];
        let size = Vec2::new(800.0, 550.0);
        pointer_ui_size(&mut app, &ctx, vec![], size);
        pointer_ui_size(&mut app, &ctx, vec![], size);
        let rect = app.crop_apply_rect.unwrap();
        assert!(
            Rect::from_min_size(Pos2::ZERO, size).contains_rect(rect),
            "{rect:?}"
        );
        assert!(rect.min.y > 350.0 && rect.max.y < 530.0, "{rect:?}");
        for pressed in [true, false] {
            pointer_ui_size(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(rect.center()),
                    egui::Event::PointerButton {
                        pos: rect.center(),
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                size,
            );
        }
        wait(&mut app, &ctx);
        assert_eq!(app.dimensions(), [50, 40]);
        assert!(app.edited && !app.crop_selecting);
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    #[test]
    fn raw_pointer_crop_selects_without_panning_and_secondary_drag_pans() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png");
        image::RgbaImage::from_pixel(100, 80, image::Rgba([80, 90, 100, 128]))
            .save(&a)
            .unwrap();
        let (mut app, ctx) = app_with(&a);
        app.refresh_texture(&ctx);
        app.editing = true;
        app.crop_selecting = true;
        pointer_ui(&mut app, &ctx, vec![]);
        let rect = app.canvas_image_rect.unwrap();
        let start = rect.min + rect.size() * Vec2::splat(0.25);
        let end = rect.min + rect.size() * Vec2::splat(0.75);
        let pan = app.pan;
        drag(&mut app, &ctx, start, end, egui::PointerButton::Primary);
        assert_eq!(app.crop, [25, 20, 50, 40]);
        assert_eq!(app.pan, pan);
        assert!(app.crop_anchor.is_none());
        let crop = app.crop;
        drag(
            &mut app,
            &ctx,
            end,
            end + Vec2::new(20.0, 10.0),
            egui::PointerButton::Secondary,
        );
        assert_ne!(app.pan, pan);
        assert_eq!(app.crop, crop);
        assert!(app.crop_anchor.is_none());
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!app.crop_selecting && app.editing);
    }
    #[test]
    fn crop_apply_uses_shared_pipeline_and_switch_clears_pending_drag() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png");
        let b = dir.path().join("B.png");
        image::RgbaImage::from_pixel(100, 80, image::Rgba([80, 90, 100, 128]))
            .save(&a)
            .unwrap();
        fixture(&b, false);
        let bytes = std::fs::read(&a).unwrap();
        let (mut app, ctx) = app_with(&a);
        app.crop = [25, 20, 50, 40];
        app.crop_selecting = true;
        app.crop_anchor = Some([25.0, 20.0]);
        app.start_edit(
            EditOp::Crop {
                x: 25,
                y: 20,
                width: 50,
                height: 40,
            },
            &ctx,
        );
        wait(&mut app, &ctx);
        assert_eq!(app.dimensions(), [50, 40]);
        assert!(!app.crop_selecting && app.crop_anchor.is_none());
        assert!(app.edited);
        assert_eq!(std::fs::read(&a).unwrap(), bytes);
        assert_eq!(app.loaded.as_ref().unwrap().pixels.dimensions(), (100, 80));
        app.crop_selecting = true;
        app.crop_anchor = Some([1.0, 1.0]);
        app.open(b.clone());
        assert!(!app.crop_selecting && app.crop_anchor.is_none());
        wait(&mut app, &ctx);
        assert_eq!(app.loaded.as_ref().unwrap().path, b);
        assert_eq!(app.dimensions(), [4, 3]);
    }
    #[test]
    fn raw_keyboard_text_focus_does_not_navigate_and_escape_closes_export() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png");
        let b = dir.path().join("B.png");
        fixture(&a, false);
        fixture(&b, false);
        let (mut app, ctx) = app_with(&a);
        app.siblings = vec![a.clone(), b.clone()];
        raw_ui(&mut app, &ctx, None, egui::Modifiers::NONE);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::S),
            egui::Modifiers {
                ctrl: true,
                command: true,
                shift: true,
                ..Default::default()
            },
        );
        assert!(app.export_dialog);
        raw_ui(&mut app, &ctx, None, egui::Modifiers::NONE);
        ctx.memory_mut(|m| m.request_focus(egui::Id::new("export-path-input")));
        assert!(ctx.egui_wants_keyboard_input());
        let generation = app.generation;
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::ArrowRight),
            egui::Modifiers::NONE,
        );
        assert_eq!(app.generation, generation);
        assert_eq!(app.loaded.as_ref().unwrap().path, a);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!app.export_dialog);
        assert!(app.export_generation.is_none());
        assert!(app.preview_texture.is_none());
        raw_ui(&mut app, &ctx, None, egui::Modifiers::NONE);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::ArrowRight),
            egui::Modifiers::NONE,
        );
        wait(&mut app, &ctx);
        assert_eq!(app.loaded.as_ref().unwrap().path, b);
    }
    #[test]
    fn raw_escape_closes_top_modal_before_editing_and_fullscreen() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png");
        fixture(&a, false);
        let (mut app, ctx) = app_with(&a);
        app.editing = true;
        app.path_dialog = true;
        app.error = Some("test error".into());
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(app.error.is_none() && app.path_dialog && app.editing);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!app.path_dialog && app.editing);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!app.editing);
        app.fullscreen = true;
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!app.fullscreen);
    }
    #[test]
    fn accessible_path_set_value_is_scoped_bounded_and_ignores_closed_modal() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png");
        fixture(&a, false);
        let (mut app, ctx) = app_with(&a);
        app.path_dialog = true;
        fn event(name: &str, value: String) -> egui::Event {
            egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::SetValue,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: egui::Id::new(name).accesskit_id(),
                data: Some(egui::accesskit::ActionData::Value(value.into())),
            })
        }
        fn run(app: &mut ImageApp, ctx: &egui::Context, event: egui::Event) {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1180.0, 780.0))),
                events: vec![event],
                ..Default::default()
            };
            let mut frame = eframe::Frame::_new_kittest();
            let mut output = ctx.run_ui(input, |ui| eframe::App::ui(app, ui, &mut frame));
            output.textures_delta.clear();
        }
        let value = "D:\\素材\\中文图片.png";
        run(&mut app, &ctx, event("open-path-input", value.into()));
        assert_eq!(app.open_path, value);
        assert_eq!(app.loaded.as_ref().unwrap().path, a);
        run(&mut app, &ctx, event("other-widget", "wrong".into()));
        assert_eq!(app.open_path, value);
        run(&mut app, &ctx, event("open-path-input", "x".repeat(32768)));
        assert_eq!(app.open_path, value);
        app.path_dialog = false;
        run(&mut app, &ctx, event("open-path-input", "closed".into()));
        assert_eq!(app.open_path, value);
        assert!(!app.loading);
    }
    #[test]
    #[cfg(windows)]
    fn missing_primary_cjk_font_uses_real_system_fallback_and_has_chinese_glyphs() {
        let ctx = egui::Context::default();
        let missing = "C:\\Windows\\Fonts\\__light_image_qa_missing__.ttf";
        assert!(!std::path::Path::new(missing).exists());
        let selected =
            configure_with_font_candidates(&ctx, &[missing, "C:\\Windows\\Fonts\\simhei.ttf"]);
        assert_eq!(selected.as_deref(), Some("C:\\Windows\\Fonts\\simhei.ttf"));
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                assert!(ui.ctx().fonts_mut(|fonts| {
                    fonts.has_glyphs(&egui::FontId::new(16.0, family), "裁剪编辑透明来源导出")
                }));
            }
            ui.label("裁剪编辑透明来源导出");
        });
        output.textures_delta.clear();
    }
    #[test]
    fn queued_load_and_edit_successes_are_not_applied_after_same_generation_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.png");
        let replacement = dir.path().join("replacement.png");
        fixture(&original, false);
        image::RgbaImage::from_pixel(17, 13, image::Rgba([10, 20, 30, 255]))
            .save(&replacement)
            .unwrap();
        let (mut app, ctx) = app_with(&original);
        let before = std::fs::read(&original).unwrap();
        app.open(replacement);
        let started = Instant::now();
        let response = loop {
            if let Some(response) = app.loader.take_result() {
                break response;
            }
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        assert!(response.result.is_ok());
        assert_eq!(response.generation, app.generation);
        app.load_cancel.cancel();
        app.apply_load_response(response, &ctx);
        assert_eq!(app.dimensions(), [4, 3]);
        assert_eq!(app.loaded.as_ref().unwrap().path, original);
        assert!(!app.loading && app.error.is_none());
        let cancel = Cancellation::default();
        let (tx, receiver) = mpsc::sync_channel(1);
        app.work = Some(Work {
            generation: app.generation,
            cancel: cancel.clone(),
            receiver,
            export_thread: None,
        });
        tx.send(WorkResult::Edited(Ok(
            light_image_tool::memory::Pixels::from_image(image::RgbaImage::new(2, 2)).unwrap(),
        )))
        .unwrap();
        cancel.cancel();
        app.poll(&ctx);
        assert_eq!(app.dimensions(), [4, 3]);
        assert!(!app.edited && app.undo.is_empty() && app.work.is_none());
        assert_eq!(std::fs::read(&original).unwrap(), before);
    }
    #[test]
    fn queued_published_export_is_successful_even_if_cancel_is_requested_before_ui_receives_it() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.png");
        fixture(&original, false);
        let (mut app, ctx) = app_with(&original);
        let destination = dir.path().join("copy.png");
        let cancel = Cancellation::default();
        let path = export::save_new(
            app.pixels.as_ref().unwrap(),
            &original,
            &ExportOptions {
                destination: destination.clone(),
                format: ExportFormat::Png,
                quality: 90,
                matte: [255; 3],
            },
            &cancel,
        )
        .unwrap();
        let (tx, receiver) = mpsc::sync_channel(1);
        app.work = Some(Work {
            generation: app.generation,
            cancel: cancel.clone(),
            receiver,
            export_thread: None,
        });
        tx.send(WorkResult::Exported(Ok(path))).unwrap();
        cancel.cancel();
        app.poll(&ctx);
        assert!(app.status.starts_with("已导出新副本"));
        assert!(app.error.is_none() && app.work.is_none());
        assert_eq!(
            image::open(&destination).unwrap().into_rgba8().as_raw(),
            app.pixels.as_ref().unwrap().as_raw()
        );
    }
    #[test]
    fn queued_evidence_and_preview_successes_do_not_apply_or_cache_after_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.png");
        fixture(&original, false);
        let (mut app, ctx) = app_with(&original);
        let hash = app.loaded.as_ref().unwrap().source_sha256.clone();
        let cancel = Cancellation::default();
        let (tx, receiver) = mpsc::sync_channel(1);
        app.evidence_task = Some(ProvenanceTask {
            generation: app.generation,
            cancel: cancel.clone(),
            receiver,
        });
        let mut report = EvidenceReport::incomplete(hash.clone(), "queued success");
        report.state = EvidenceState::NoCredential;
        tx.send(Ok(report)).unwrap();
        cancel.cancel();
        app.poll(&ctx);
        assert_eq!(app.evidence_stage, EvidenceStage::Cancelled);
        assert!(app.evidence_cache.get(&hash).is_none());
        app.export_dialog = true;
        let cancel = Cancellation::default();
        let (tx, receiver) = mpsc::sync_channel(1);
        app.preview_task = Some(PreviewTask {
            generation: app.generation,
            matte: app.matte,
            cancel: cancel.clone(),
            receiver,
        });
        tx.send(Ok(light_image_tool::memory::Pixels::from_image(
            image::RgbaImage::new(2, 2),
        )
        .unwrap()))
            .unwrap();
        cancel.cancel();
        app.poll_preview(&ctx);
        assert!(app.preview_texture.is_none() && app.preview_matte.is_none());
    }
    #[test]
    fn rapid_overlapping_requests_cancel_old_tokens_and_finish_with_latest_image() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.png");
        let final_path = dir.path().join("last.png");
        let invalid = dir.path().join("invalid.png");
        image::RgbaImage::from_pixel(512, 512, image::Rgba([80, 90, 100, 128]))
            .save(&first)
            .unwrap();
        image::RgbaImage::from_pixel(37, 23, image::Rgba([15, 25, 35, 255]))
            .save(&final_path)
            .unwrap();
        std::fs::write(&invalid, b"invalid image").unwrap();
        let first_bytes = std::fs::read(&first).unwrap();
        let final_bytes = std::fs::read(&final_path).unwrap();
        let baseline = light_image_tool::memory::in_use();
        {
            let (mut app, ctx) = app_with(&first);
            let mut prior = Vec::new();
            for index in 0..1000 {
                prior.push(app.load_cancel.clone());
                app.open(if index % 2 == 0 {
                    first.clone()
                } else {
                    final_path.clone()
                });
                assert!(
                    light_image_tool::memory::in_use()
                        <= light_image_tool::memory::OPERATION_BUDGET
                );
            }
            for cancel in prior {
                assert!(matches!(cancel.check(), Err(ToolError::Cancelled)));
            }
            let generation = app.generation;
            wait(&mut app, &ctx);
            assert_eq!(app.generation, generation);
            assert_eq!(app.loaded.as_ref().unwrap().path, final_path);
            assert_eq!(app.dimensions(), [37, 23]);
            assert_eq!(
                app.pixels.as_ref().unwrap().get_pixel(0, 0).0,
                [15, 25, 35, 255]
            );
            assert!(app.error.is_none());
            app.open(invalid);
            wait(&mut app, &ctx);
            assert!(app.error.is_some());
            app.open(final_path.clone());
            wait(&mut app, &ctx);
            assert!(app.error.is_none());
            assert_eq!(app.dimensions(), [37, 23]);
            assert_eq!(std::fs::read(&first).unwrap(), first_bytes);
            assert_eq!(std::fs::read(&final_path).unwrap(), final_bytes);
        }
        let started = Instant::now();
        while light_image_tool::memory::in_use() > baseline {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "owned allocation not released"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    #[test]
    fn renderer_unwind_waits_for_owned_export_temp_cleanup() {
        use std::{io::Write, time::Duration};
        for contents in [b"".as_slice(), b"partial JPEG".as_slice()] {
            let dir = tempfile::tempdir().unwrap();
            let original = dir.path().join("original.png");
            fixture(&original, false);
            let original_bytes = std::fs::read(&original).unwrap();
            let existing = dir.path().join("completed.png");
            std::fs::write(&existing, &original_bytes).unwrap();
            let unrelated = dir.path().join(".light-image-export-unrelated.tmp");
            std::fs::write(&unrelated, b"not owned by this export").unwrap();
            let destination = dir.path().join("unfinished.jpg");
            let (mut app, _) = app_with(&original);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let (cancel_tx, cancel_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::sync_channel(1);
            let directory = dir.path().to_owned();
            app.work = Some(Work::spawn_export(app.generation, move |cancel| {
                let mut temp = tempfile::Builder::new()
                    .prefix(".light-image-export-")
                    .suffix(".tmp")
                    .tempfile_in(directory)?;
                temp.write_all(contents)?;
                ready_tx.send(temp.path().to_owned()).unwrap();
                let started = Instant::now();
                while cancel.check().is_ok() {
                    assert!(started.elapsed() < Duration::from_secs(5));
                    std::thread::sleep(Duration::from_millis(1));
                }
                cancel_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                // The temporary is still alive until this operation returns.
                Err(ToolError::Cancelled)
            }));
            let temporary = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let (done_tx, done_rx) = mpsc::sync_channel(1);
            let shutdown = std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _app = app;
                    panic!("controlled renderer unwind while export owns a file");
                }));
                assert!(result.is_err());
                done_tx.send(()).unwrap();
            });
            cancel_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let early = done_rx.recv_timeout(Duration::from_millis(50));
            let temp_still_owned = temporary.exists();
            release_tx.send(()).unwrap();
            if early.is_err() {
                done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            shutdown.join().unwrap();
            assert!(
                early.is_err(),
                "renderer boundary returned before export cleanup"
            );
            assert!(temp_still_owned);
            assert!(!temporary.exists());
            assert!(!destination.exists());
            assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
            assert_eq!(std::fs::read(&existing).unwrap(), original_bytes);
            assert_eq!(
                std::fs::read(&unrelated).unwrap(),
                b"not owned by this export"
            );
        }
    }
    #[test]
    fn normal_exit_joins_export_and_preserves_completed_publication() {
        use eframe::App;
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.png");
        fixture(&original, false);
        let (mut app, ctx) = app_with(&original);
        let output = dir.path().join("published.png");
        let pixels = app.pixels.clone().unwrap();
        let destination = output.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        app.work = Some(Work::spawn_export(app.generation, move |cancel| {
            let result = export::save_new(
                &pixels,
                &original,
                &ExportOptions {
                    destination,
                    format: ExportFormat::Png,
                    quality: 90,
                    matte: [255; 3],
                },
                &cancel,
            );
            ready_tx.send(()).unwrap();
            result
        }));
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // Even an unconsumed result must not block joining. No export worker may
        // acquire this UI lock when the application is closing.
        ctx.memory_mut(|_| app.on_exit());
        assert!(app.work.is_none());
        app.on_exit();
        drop(app);
        assert!(image::open(output).is_ok());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
    #[test]
    fn polling_completed_and_stale_exports_joins_the_file_owner() {
        for stale in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let original = dir.path().join("original.png");
            fixture(&original, false);
            let (mut app, ctx) = app_with(&original);
            let cancel = Cancellation::default();
            let token = cancel.clone();
            let directory = dir.path().to_owned();
            let (tx, receiver) = mpsc::sync_channel(1);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let temporary = tempfile::NamedTempFile::new_in(directory).unwrap();
                tx.try_send(WorkResult::Exported(Err(ToolError::Cancelled)))
                    .unwrap();
                ready_tx.send(temporary.path().to_owned()).unwrap();
                let started = Instant::now();
                while token.check().is_ok() {
                    assert!(started.elapsed() < std::time::Duration::from_secs(5));
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            });
            app.work = Some(Work {
                generation: if stale {
                    app.generation.wrapping_sub(1)
                } else {
                    app.generation
                },
                cancel,
                receiver,
                export_thread: Some(worker),
            });
            let temporary = ready_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            app.poll(&ctx);
            assert!(app.work.is_none());
            assert!(!temporary.exists());
        }
    }
    #[test]
    fn export_thread_panic_is_joined_without_panicking_in_app_drop() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original.png");
        fixture(&original, false);
        let (mut app, ctx) = app_with(&original);
        let directory = dir.path().to_owned();
        let (tx, receiver) = mpsc::sync_channel(1);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _tx = tx;
            let temporary = tempfile::NamedTempFile::new_in(directory).unwrap();
            ready_tx.send(temporary.path().to_owned()).unwrap();
            panic!("controlled unprotected export thread failure");
        });
        app.work = Some(Work {
            generation: app.generation,
            cancel: Cancellation::default(),
            receiver,
            export_thread: Some(worker),
        });
        let temporary = ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let started = Instant::now();
        while !app
            .work
            .as_ref()
            .unwrap()
            .export_thread
            .as_ref()
            .unwrap()
            .is_finished()
        {
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            std::thread::yield_now();
        }
        app.poll(&ctx);
        assert!(app.work.is_none() && app.error.is_some());
        assert!(!temporary.exists());
        drop(app);
    }
    #[test]
    fn renderer_unwind_cancels_all_background_tokens_without_on_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("original.png");
        fixture(&path, false);
        let (mut app, _ctx) = app_with(&path);
        let load = app.load_cancel.clone();
        let work = Cancellation::default();
        let evidence = Cancellation::default();
        let preview = Cancellation::default();
        let (_tx, receiver) = mpsc::sync_channel(1);
        app.work = Some(Work {
            generation: app.generation,
            cancel: work.clone(),
            receiver,
            export_thread: None,
        });
        let (_tx, receiver) = mpsc::sync_channel(1);
        app.evidence_task = Some(ProvenanceTask {
            generation: app.generation,
            cancel: evidence.clone(),
            receiver,
        });
        let (_tx, receiver) = mpsc::sync_channel(1);
        app.preview_task = Some(PreviewTask {
            generation: app.generation,
            matte: [255; 3],
            cancel: preview.clone(),
            receiver,
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _app = app;
            panic!("controlled renderer unwind");
        }));
        assert!(result.is_err());
        for token in [load, work, evidence, preview] {
            assert!(matches!(token.check(), Err(ToolError::Cancelled)));
        }
        assert!(path.exists());
    }
    #[test]
    fn source_task_failure_cancel_and_no_credential_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png");
        fixture(&a, false);
        let (mut app, ctx) = app_with(&a);
        for (result, expected) in [
            (Err(ToolError::Cancelled), EvidenceStage::Cancelled),
            (
                Err(ToolError::Invalid("test worker error".into())),
                EvidenceStage::Failed,
            ),
        ] {
            let (tx, rx) = mpsc::sync_channel(1);
            app.evidence_stage = EvidenceStage::Running;
            app.evidence_task = Some(ProvenanceTask {
                generation: app.generation,
                cancel: Cancellation::default(),
                receiver: rx,
            });
            tx.send(result).unwrap();
            app.poll(&ctx);
            assert_eq!(app.evidence_stage, expected);
            assert!(
                app.evidence_cache
                    .get(&app.loaded.as_ref().unwrap().source_sha256)
                    .is_none()
            );
        }
        let (tx, rx) = mpsc::sync_channel(1);
        app.evidence_task = Some(ProvenanceTask {
            generation: app.generation,
            cancel: Cancellation::default(),
            receiver: rx,
        });
        drop(tx);
        app.poll(&ctx);
        assert_eq!(app.evidence_stage, EvidenceStage::Failed);
        assert_eq!(
            evidence_check_label(&EvidenceState::NoCredential, &CheckState::NotChecked),
            "不适用（未发现凭证）"
        );
        assert_eq!(
            evidence_check_label(&EvidenceState::Signed, &CheckState::NotChecked),
            "未核验"
        );
        assert_eq!(
            evidence_check_label(&EvidenceState::Signed, &CheckState::Failed),
            "失败"
        );
        assert_ne!(
            evidence_stage_label(EvidenceStage::Cancelled),
            evidence_stage_label(EvidenceStage::Complete)
        );
    }
}

#[cfg(test)]
#[path = "app_keyboard_tests.rs"]
mod keyboard_tests;
