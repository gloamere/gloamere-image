#[path = "app_actions.rs"]
mod actions_ui;
#[path = "app_ai.rs"]
mod ai_ui;
#[path = "app_chrome.rs"]
mod chrome_ui;
#[path = "app_comparison.rs"]
mod comparison_ui;
#[path = "app_context_menu.rs"]
mod context_menu_ui;
#[path = "app_icon.rs"]
mod icon_ui;
#[path = "app_inspector.rs"]
mod inspector_ui;
#[path = "app_menu.rs"]
mod menu_ui;
#[path = "app_rename.rs"]
mod rename_ui;
#[path = "app_stickers.rs"]
mod stickers_ui;
#[path = "app_update.rs"]
mod update_ui;
use crate::app_theme;
use crate::viewer_ui::{self, ActionButton, CompactAction, Icon, ToolbarLayout};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use light_image_tool::{
    Cancellation, ToolError,
    animation::{
        AnimationFrame, AnimationRequest, AnimationSource, AnimationWorker, PlaybackClock,
    },
    browsing::{self, DirectoryListing, Request as BrowserRequest, Worker as BrowserWorker},
    edit::{self, EditOp},
    export::{self, ExportFormat, ExportOptions},
    pipeline::LoadedImage,
    provenance::{self, CheckState, EvidenceCache, EvidenceReport, EvidenceState},
    settings::{self, Appearance, CanvasBackground, Preferences},
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
        ui.colored_label(
            app_theme::palette(ui.ctx()).error,
            "路径超过 Windows 长度限制",
        );
    }
    ui.add(egui::TextEdit::singleline(text).id(id).desired_width(width));
}

#[derive(Clone, Debug, Default)]
enum DocumentSource {
    #[default]
    File,
    Clipboard,
    ExtractedFrame {
        name: String,
        frame: u32,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingIntent {
    Open(PathBuf),
    OpenIconEntry(PathBuf, usize),
    ReloadIcon(PathBuf, [u32; 2], u16),
    Clipboard,
    Restore,
    Close,
}

#[derive(Clone, Copy)]
enum IconLoad {
    Entry(usize),
    Reload([u32; 2], u16),
}

enum WorkResult {
    Edited(light_image_tool::Result<light_image_tool::memory::Pixels>),
    Exported(light_image_tool::Result<PathBuf>),
    ClipboardRead(light_image_tool::Result<light_image_tool::clipboard::ClipboardInput>),
    ClipboardCopied(light_image_tool::Result<String>),
    FileLocated(light_image_tool::Result<String>),
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
    thread: Option<std::thread::JoinHandle<()>>,
}
struct Work {
    generation: u64,
    cancel: Cancellation,
    receiver: mpsc::Receiver<WorkResult>,
    // Every operation is joined; the name is retained for v0.4 test compatibility.
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
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for PreviewTask {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("shutdown: preview worker panicked");
        }
    }
}
impl Drop for ProvenanceTask {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("shutdown: provenance worker panicked");
        }
    }
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
struct Playback {
    source: Arc<AnimationSource>,
    clock: PlaybackClock,
    cancel: Cancellation,
    pending: Option<AnimationFrame>,
    in_flight: Option<u64>,
    sequence: u64,
    manual_paused: bool,
    requires_restart: bool,
    suspended: bool,
    step_requested: bool,
    ended: bool,
    eof_pending: bool,
    frame_delay: std::time::Duration,
    frame_index: u32,
    traversal: u32,
    elapsed: std::time::Duration,
    warning: Option<String>,
}
impl Playback {
    fn new(source: Arc<AnimationSource>) -> Self {
        let frame_delay = source.first_delay;
        let mut clock = PlaybackClock::new(frame_delay);
        clock.resume(Instant::now());
        Self {
            source,
            clock,
            cancel: Cancellation::default(),
            pending: None,
            in_flight: None,
            sequence: 0,
            manual_paused: false,
            requires_restart: false,
            suspended: false,
            step_requested: false,
            ended: false,
            eof_pending: false,
            frame_delay,
            frame_index: 1,
            traversal: 1,
            elapsed: std::time::Duration::ZERO,
            warning: None,
        }
    }
}
#[derive(Clone)]
struct Tile {
    _reservation: Arc<light_image_tool::memory::Reservation>,
    _display: Arc<light_image_tool::memory::PreparedDisplay>,
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
    pub appearance: Option<Appearance>,
    pub icon_entry: Option<usize>,
    pub hide_list: bool,
    pub compare_image: Option<PathBuf>,
    pub inspect_pixels: bool,
    pub alpha_view: bool,
    pub nearest_pixels: bool,
    pub reference_on_top: bool,
    pub show_sticker: bool,
    pub sticker_image: Option<PathBuf>,
    pub show_context_menu: bool,
    pub show_rename: bool,
    pub sticker_zoom_smoke: bool,
    context_menu_started: Option<Instant>,
    feature_stage: u8,
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
            appearance: None,
            icon_entry: None,
            hide_list: true,
            compare_image: None,
            inspect_pixels: false,
            alpha_view: false,
            nearest_pixels: false,
            reference_on_top: false,
            show_sticker: false,
            sticker_image: None,
            show_context_menu: false,
            show_rename: false,
            sticker_zoom_smoke: false,
            context_menu_started: None,
            feature_stage: 0,
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
    animation_worker: AnimationWorker,
    playback: Option<Playback>,
    extract_dialog: bool,
    generation: u64,
    load_cancel: Cancellation,
    loading: bool,
    load_started: Option<Instant>,
    loaded: Option<LoadedImage>,
    pixels: Option<Arc<light_image_tool::memory::Pixels>>,
    tiles: Vec<Tile>,
    canvas_unavailable: bool,
    retired_texture_reservations: Vec<(u64, Arc<light_image_tool::memory::Reservation>)>,
    retired_displays: Vec<(u64, Arc<light_image_tool::memory::PreparedDisplay>)>,
    undo: Vec<Arc<light_image_tool::memory::Pixels>>,
    redo: Vec<Arc<light_image_tool::memory::Pixels>>,
    inspector: inspector_ui::Inspector,
    comparison: comparison_ui::Comparison,
    reference_on_top: bool,
    stickers: stickers_ui::Stickers,
    edited: bool,
    source: DocumentSource,
    temporary_original: Option<Arc<light_image_tool::memory::Pixels>>,
    temporary_color_status: String,
    temporary_color_managed: bool,
    exported_pixels: Option<std::sync::Weak<light_image_tool::memory::Pixels>>,
    pending_intent: Option<PendingIntent>,
    pending_export: bool,
    pending_focus: bool,
    close_approved: bool,
    shutting_down: bool,
    siblings: Vec<PathBuf>,
    ai: light_image_tool::ai::Controller,
    ai_view: ai_ui::AiView,
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
    updater: light_image_tool::updater::client::Client,
    model_updater: light_image_tool::updater::model_client::ModelClient,
    update_health_sent: bool,
    update_health_probe: Option<light_image_tool::updater::launch::HealthProbe>,
    update_normal_exit: bool,
    update_modal_open: bool,
    update_shutdown_observed: bool,
    settings_path: Option<PathBuf>,
    settings_warning: Option<String>,
    position: usize,
    zoom: f32,
    fit: bool,
    long_image: bool,
    fit_width: bool,
    long_scroll: f32,
    long_to_bottom: bool,
    canvas_viewport: Option<Rect>,
    fullscreen_activity: f64,
    toolbar_hidden: bool,
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
    #[cfg(test)]
    sticker_button_rect: Option<Rect>,
    #[cfg(test)]
    title_name_rect: Option<Rect>,
    rename: rename_ui::RenameState,
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
    resize_tool: bool,
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
    app_theme::configure(ctx);
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
        app.update_shutdown_observed =
            light_image_tool::updater::platform::install_shutdown_observer(cc).is_ok();
        if persistent {
            app.settings_path = settings::user_settings_path();
            app.updater = light_image_tool::updater::client::Client::load(
                light_image_tool::updater::launch::state_directory()
                    .and_then(|p| p.parent().map(|d| d.join("update-settings.json"))),
            );
            app.ai = light_image_tool::ai::Controller::new(
                app.settings_path
                    .as_ref()
                    .map(|path| path.with_file_name("ai-records.sqlite")),
            );
            app.ai_sync_scope();
            app.load_session_preferences();
        }
        cc.egui_ctx
            .set_theme(app_theme::preference(app.preferences.appearance));
        app
    }
    fn cancel_background(&mut self) {
        if let Some(playback) = &self.playback {
            playback.cancel.cancel();
        }
        self.animation_worker.stop();
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
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        self.rename.shutdown();
        let started = Instant::now();
        log::info!("shutdown: admission stopped; cancellation begins");
        self.cancel_background();
        drop(self.work.take());
        log::info!(
            "shutdown: edit/export owners joined; elapsed_ms={}",
            started.elapsed().as_millis()
        );
        drop(self.preview_task.take());
        drop(self.evidence_task.take());
        log::info!(
            "shutdown: preview/provenance owners joined; elapsed_ms={}",
            started.elapsed().as_millis()
        );
        self.animation_worker.shutdown();
        self.playback = None;
        log::info!(
            "shutdown: animation joined; elapsed_ms={}",
            started.elapsed().as_millis()
        );
        self.loader.shutdown();
        self.directory_worker.shutdown();
        self.thumbnail_worker.shutdown();
        self.ai.shutdown();
        self.inspector.shutdown();
        self.comparison.clear();
        self.stickers.shutdown();
        log::info!(
            "shutdown: loader/browser joined; elapsed_ms={}",
            started.elapsed().as_millis()
        );
        self.tiles.clear();
        self.thumbnail_cache.clear();
        self.preview_texture = None;
        log::info!(
            "shutdown: app textures released to renderer; GPU teardown follows outside app; elapsed_ms={}",
            started.elapsed().as_millis()
        );
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
            animation_worker: AnimationWorker::new(ctx.clone()),
            playback: None,
            extract_dialog: false,
            generation: 0,
            load_cancel: Cancellation::default(),
            loading: false,
            load_started: None,
            loaded: None,
            pixels: None,
            tiles: Vec::new(),
            canvas_unavailable: false,
            retired_texture_reservations: Vec::new(),
            retired_displays: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            inspector: inspector_ui::Inspector::default(),
            comparison: comparison_ui::Comparison::default(),
            reference_on_top: false,
            stickers: stickers_ui::Stickers::new(ctx.clone()),
            edited: false,
            source: DocumentSource::File,
            temporary_original: None,
            temporary_color_status: String::new(),
            temporary_color_managed: true,
            exported_pixels: None,
            pending_intent: None,
            pending_export: false,
            pending_focus: false,
            close_approved: false,
            shutting_down: false,
            siblings: Vec::new(),
            ai: light_image_tool::ai::Controller::new(None),
            ai_view: ai_ui::AiView::default(),
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
            updater: light_image_tool::updater::client::Client::default(),
            model_updater: light_image_tool::updater::model_client::ModelClient::default(),
            update_health_sent: false,
            update_health_probe: None,
            update_normal_exit: false,
            update_modal_open: false,
            update_shutdown_observed: false,
            settings_path: None,
            settings_warning: None,
            position: 0,
            zoom: 1.0,
            fit: true,
            long_image: false,
            fit_width: false,
            long_scroll: 0.0,
            long_to_bottom: false,
            canvas_viewport: None,
            fullscreen_activity: 0.0,
            toolbar_hidden: false,
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
            #[cfg(test)]
            sticker_button_rect: None,
            #[cfg(test)]
            title_name_rect: None,
            rename: rename_ui::RenameState::default(),
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
            resize_tool: false,
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
            if let Some(appearance) = smoke.appearance {
                app.preferences.appearance = appearance;
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
            if let Some(index) = app.smoke.as_ref().and_then(|smoke| smoke.icon_entry) {
                app.open_with_icon_entry(path, Some(index));
            } else {
                app.open(path);
            }
        }
        app
    }

    fn prepare_browser(&mut self, path: &PathBuf) {
        let parent = browsing::directory(path);
        let scope_changed =
            self.directory_path.as_ref() != Some(&parent) || !self.siblings.contains(path);
        if self.directory_path.as_ref() != Some(&parent) {
            self.browser_clear_metadata();
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
        if scope_changed {
            self.ai_sync_scope();
        } else {
            self.ai.set_current(path);
            self.ai_refresh_current(path);
        }
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
    fn unsaved(&self) -> bool {
        self.edited
            && !self
                .exported_pixels
                .as_ref()
                .zip(self.pixels.as_ref())
                .is_some_and(|(saved, current)| saved.ptr_eq(&Arc::downgrade(current)))
    }
    fn original_pixels(&self) -> Option<Arc<light_image_tool::memory::Pixels>> {
        self.temporary_original
            .clone()
            .or_else(|| self.loaded.as_ref().map(|l| l.pixels.clone()))
    }
    fn open(&mut self, path: PathBuf) {
        self.open_or_request(path, None);
    }
    fn open_or_request(&mut self, path: PathBuf, icon_entry: Option<IconLoad>) {
        if self.shutting_down || self.pending_intent.is_some() || self.rename.blocks_document() {
            return;
        }
        if self.unsaved() {
            self.pending_intent = Some(match icon_entry {
                Some(IconLoad::Entry(index)) => PendingIntent::OpenIconEntry(path, index),
                Some(IconLoad::Reload(dimensions, bit_depth)) => {
                    PendingIntent::ReloadIcon(path, dimensions, bit_depth)
                }
                None => PendingIntent::Open(path),
            });
            self.pending_focus = true;
            return;
        }
        self.open_with_icon_request(path, icon_entry);
    }
    fn open_now(&mut self, path: PathBuf) {
        self.open_with_icon_entry(path, None);
    }
    fn select_icon_entry(&mut self, index: usize) {
        if self.loading || self.work.is_some() {
            return;
        }
        let Some(image) = &self.loaded else {
            return;
        };
        let Some(icon) = &image.icon else {
            return;
        };
        if index >= icon.entries.len() || index == icon.selected_index {
            return;
        }
        self.open_or_request(image.path.clone(), Some(IconLoad::Entry(index)));
    }
    fn open_with_icon_entry(&mut self, path: PathBuf, icon_entry: Option<usize>) {
        self.open_with_icon_request(path, icon_entry.map(IconLoad::Entry));
    }
    fn open_with_icon_request(&mut self, path: PathBuf, icon_entry: Option<IconLoad>) {
        if self.rename.blocks_document() {
            return;
        }
        if let Some(playback) = self.playback.take() {
            playback.cancel.cancel();
        }
        self.animation_worker.stop();
        self.extract_dialog = false;
        self.prepare_browser(&path);
        self.crop_selecting = false;
        self.crop_anchor = None;
        self.close_export();
        self.clear_edit_history();
        self.exported_pixels = None;
        self.thumbnail_worker.cancel();
        self.thumbnail_busy = None;
        self.thumbnail_cache.retain(|entry| entry.path != path);
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
        self.load_started = Some(Instant::now());
        self.error = None;
        self.status = format!(
            "正在打开 {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let request = LoadRequest {
            generation: self.generation,
            path,
            cancel: self.load_cancel.clone(),
        };
        match icon_entry {
            Some(IconLoad::Entry(index)) => self.loader.request_icon_entry(request, index),
            Some(IconLoad::Reload(dimensions, bit_depth)) => {
                self.loader
                    .request_icon_reload(request, dimensions, bit_depth);
            }
            None => self.loader.request(request),
        }
    }
    fn load_feedback(&self, now: Instant) -> Option<String> {
        if !self.loading
            || self.load_started.is_none_or(|started| {
                now.saturating_duration_since(started) < std::time::Duration::from_millis(200)
            })
        {
            return None;
        }
        Some(if self.pixels.is_some() {
            format!("画面仍是上一张 · {}", self.status)
        } else {
            self.status.clone()
        })
    }
    fn request_intent(&mut self, intent: PendingIntent, ctx: &egui::Context) {
        if self.shutting_down || self.pending_intent.is_some() || self.rename.blocks_document() {
            return;
        }
        if self.unsaved() {
            self.pending_intent = Some(intent);
            self.pending_focus = true;
            self.more_open = false;
        } else {
            self.perform_intent(intent, ctx);
        }
    }
    fn perform_intent(&mut self, intent: PendingIntent, ctx: &egui::Context) {
        if self.rename.blocks_document() {
            return;
        }
        match intent {
            PendingIntent::Open(path) => self.open_now(path),
            PendingIntent::OpenIconEntry(path, index) => {
                self.open_with_icon_entry(path, Some(index));
            }
            PendingIntent::ReloadIcon(path, dimensions, bit_depth) => {
                self.open_with_icon_request(path, Some(IconLoad::Reload(dimensions, bit_depth)));
            }
            PendingIntent::Clipboard => self.open_clipboard_now(ctx),
            PendingIntent::Restore => {
                self.pixels = self.original_pixels();
                self.clear_edit_history();
                self.edited = !matches!(self.source, DocumentSource::File);
                self.exported_pixels = None;
                self.refresh_texture(ctx);
                let [w, h] = self.dimensions();
                self.resize = [w, h];
                self.crop = [0, 0, w, h];
            }
            PendingIntent::Close => {
                self.close_approved = true;
                self.update_normal_exit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
    fn open_clipboard_now(&mut self, ctx: &egui::Context) {
        if self.work.is_some() || self.loading || self.rename.blocks_document() {
            return;
        }
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let (tx, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result = protected(
                || {
                    token.check()?;
                    let input = light_image_tool::clipboard::ClipboardAdapter::new()
                        .read_image_or_file()
                        .map_err(|e| ToolError::Invalid(e.to_string()))?;
                    if let light_image_tool::clipboard::ClipboardInput::Pixels(pixels) = &input {
                        pixels.pixels.prepare_display(&token)?;
                    }
                    token.check()?;
                    Ok(input)
                },
                "剪贴板读取异常；当前图像未修改",
            );
            let _ = tx.try_send(WorkResult::ClipboardRead(result));
        });
        self.work = Some(Work {
            generation: self.generation,
            cancel,
            receiver,
            export_thread: Some(thread),
        });
        self.status = "正在读取剪贴板图像…".into();
        ctx.request_repaint();
    }
    fn copy_image(&mut self, ctx: &egui::Context) {
        if self.work.is_some() || self.loading || self.rename.blocks_document() {
            return;
        }
        let Some(pixels) = self.pixels.clone() else {
            return;
        };
        let success = if self.playback.is_some() {
            "已复制当前帧（完整静态像素）"
        } else {
            "已复制完整图像像素"
        }
        .to_string();
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let (tx, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result = protected(
                || {
                    token.check()?;
                    light_image_tool::clipboard::ClipboardAdapter::new()
                        .copy_pixels_cancellable(&pixels, &token)
                        .map(|()| success)
                        .map_err(|e| match e {
                            light_image_tool::clipboard::ClipboardError::Cancelled => {
                                ToolError::Cancelled
                            }
                            e => ToolError::Invalid(e.to_string()),
                        })
                },
                "剪贴板写入异常；请重试或导出 PNG 副本",
            );
            let _ = tx.try_send(WorkResult::ClipboardCopied(result));
        });
        self.work = Some(Work {
            generation: self.generation,
            cancel,
            receiver,
            export_thread: Some(thread),
        });
        self.status = "正在复制完整图像像素…".into();
        ctx.request_repaint();
    }
    fn copy_path(&mut self, ctx: &egui::Context) {
        if self.work.is_some() || self.loading {
            return;
        }
        let Some(path) = self
            .loaded
            .as_ref()
            .map(|l| l.path.to_string_lossy().into_owned())
        else {
            return;
        };
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let (tx, receiver) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result = protected(
                || {
                    token.publish(|| {
                        light_image_tool::clipboard::ClipboardAdapter::new()
                            .copy_text(&path)
                            .map_err(|e| ToolError::Invalid(e.to_string()))?;
                        Ok("已复制文件路径".into())
                    })
                },
                "路径复制未完成，请重试",
            );
            let _ = tx.try_send(WorkResult::ClipboardCopied(result));
        });
        self.work = Some(Work {
            generation: self.generation,
            cancel,
            receiver,
            export_thread: Some(thread),
        });
        self.status = "正在复制文件路径…".into();
        ctx.request_repaint();
    }
    fn set_temporary_image(
        &mut self,
        image: light_image_tool::clipboard::ClipboardImage,
        ctx: &egui::Context,
    ) {
        self.cancel_background();
        self.generation += 1;
        self.directory_generation += 1;
        self.directory_worker.cancel();
        self.directory_scanning = false;
        self.thumbnail_busy = None;
        self.thumbnail_cache.clear();
        self.siblings.clear();
        self.ai.set_scope(&[], None);
        self.ai_rebuild_view();
        self.directory_path = None;
        self.requested_path = None;
        self.position = 0;
        self.loaded = None;
        self.evidence = None;
        self.evidence_attempted = true;
        self.evidence_stage = EvidenceStage::NotStarted;
        self.playback = None;
        self.source = DocumentSource::Clipboard;
        self.temporary_color_status = image.color_status;
        if !image.warnings.is_empty() {
            self.temporary_color_status
                .push_str(&format!("；{}", image.warnings.join("；")));
        }
        self.temporary_color_managed = image.color_managed;
        self.temporary_original = Some(image.pixels.clone());
        self.pixels = Some(image.pixels);
        self.clear_edit_history();
        self.edited = true;
        self.exported_pixels = None;
        self.loading = false;
        self.load_started = None;
        self.fit_window();
        self.resize = self.dimensions();
        self.crop = [0, 0, self.resize[0], self.resize[1]];
        self.refresh_texture(ctx);
        self.status = "剪贴板图片 · 尚未保存 · 未读取原文件来源".into();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            "gloamere-image · 剪贴板图片".into(),
        ));
    }
    fn unsaved_dialog(&mut self, ctx: &egui::Context) {
        if self.pending_intent.is_none() || self.pending_export {
            return;
        }
        let mut visible = true;
        let mut choice = 0;
        egui::Window::new("有未导出的改动")
            .open(&mut visible)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .max_width((ctx.viewport_rect().width() - 40.0).max(240.0))
            .show(ctx, |ui| {
                ui.label("继续会丢弃当前编辑。原图保持不变。");
                if ui
                    .add_enabled(self.work.is_none(), egui::Button::new("导出副本后继续"))
                    .clicked()
                {
                    choice = 1;
                }
                if ui
                    .add_enabled(self.work.is_none(), egui::Button::new("放弃改动并继续"))
                    .clicked()
                {
                    choice = 2;
                }
                let cancel = ui.button("取消");
                if self.pending_focus {
                    self.pending_focus = false;
                    cancel.request_focus();
                }
                if cancel.clicked() {
                    choice = 3;
                }
            });
        if !visible {
            self.pending_intent = None;
            return;
        }
        self.resolve_pending_choice(choice, ctx);
    }
    fn resolve_pending_choice(&mut self, choice: u8, ctx: &egui::Context) {
        match choice {
            1 => {
                self.pending_export = true;
                self.begin_export();
                if !self.export_dialog {
                    self.pending_export = false;
                    self.pending_intent = None;
                }
            }
            2 => {
                if let Some(intent) = self.pending_intent.take() {
                    self.perform_intent(intent, ctx);
                }
            }
            3 => self.pending_intent = None,
            _ => {}
        }
    }
    fn open_dialog(&mut self) {
        if self.rename.blocks_document() {
            return;
        }
        if let Some(path) = rfd::FileDialog::new()
            .set_title("gloamere-image · 打开图片")
            .add_filter(
                "JPEG / PNG / GIF / WebP / BMP / ICO",
                &["jpg", "jpeg", "png", "gif", "webp", "bmp", "ico"],
            )
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
            .chain(self.temporary_original.iter())
            .chain(self.undo.iter())
            .chain(self.redo.iter())
            .chain(self.comparison.pinned.as_ref().map(|p| &p.pixels))
        {
            if !unique.iter().any(|p| Arc::ptr_eq(p, pixels)) {
                unique.push(pixels);
            }
        }
        let images: u64 = unique.iter().map(|p| p.as_raw().len() as u64).sum();
        let mut texture_ids = std::collections::HashSet::new();
        let textures: u64 = self
            .tiles
            .iter()
            .chain(self.comparison.pinned.iter().flat_map(|p| &p.tiles))
            .chain(self.stickers.tiles())
            .filter(|tile| texture_ids.insert(tile.texture.id()))
            .map(|t| u64::from(t.w) * u64::from(t.h) * 8)
            .sum();
        images
            + textures
            + 64 * 1024 * 1024
            + self._browser_reservation.bytes()
            + self.stickers.window_bytes()
    }
    fn evict_history(&mut self, extra: u64) {
        self.evict_edit_history(extra);
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
            "redo_entries": self.redo.len(),
            "stickers": self.stickers.diagnostics(),
            "edit_history_with_display_bytes": self.edit_history_bytes(),
            "pinned_reference_rgba_bytes": self.comparison.pinned.as_ref().map_or(0, |p| p.pixels.as_raw().len()),
            "pinned_reference_texture_bytes": self.comparison.pinned.as_ref().map_or(0, |p| p.tiles.iter().filter(|t| !self.tiles.iter().any(|current| current.texture.id() == t.texture.id())).map(|t| u64::from(t.w) * u64::from(t.h) * 4).sum::<u64>()),
            "history_referenced_bytes_not_deduplicated": self.undo.iter().map(|p| p.as_raw().len()).sum::<usize>(),
            "texture_cpu_colorimage_estimate_bytes": texture_bytes,
            "texture_gpu_estimate_bytes": texture_bytes,
            "ui_fixed_reservation_bytes": 64 * 1024 * 1024,
            "browser_fixed_reservation_bytes": self._browser_reservation.bytes(),
            "thumbnail_cpu_gpu_estimate_bytes": self.thumbnail_cache.iter().filter_map(|entry| entry.texture.as_ref()).map(|texture| texture.size()[0] as u64 * texture.size()[1] as u64 * 8).sum::<u64>(),
            "preview_reservation_bytes": self.preview_reservation.as_ref().map_or(0, |r| r.bytes()),
            "texture_handoff_reservations_bytes": self.retired_texture_reservations.iter().map(|(_, r)| r.bytes()).sum::<u64>(),
            "total_resident_logical_estimate_bytes": self.resident_estimate(),
            "owned_total_bytes": light_image_tool::memory::in_use(),
            "owned_peak_bytes": light_image_tool::memory::peak(),
            "driver_allocator_and_delayed_resource_release": "not measured separately; logical estimates are not process memory limits"
        })
    }
    fn update_preview(&mut self, _ctx: &egui::Context) {
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
        let (tx, rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
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
        });
        self.preview_task = Some(PreviewTask {
            generation: self.generation,
            matte,
            cancel,
            receiver: rx,
            thread: Some(thread),
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
        if let Some(next) = self.ai_navigation_target(step) {
            self.select_image(next);
        }
    }
    fn select_image(&mut self, position: usize) {
        if self.rename.blocks_document() {
            return;
        }
        if let Some(path) = self.siblings.get(position).cloned() {
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
    fn set_appearance(&mut self, ctx: &egui::Context, appearance: Appearance) {
        if self.preferences.appearance == appearance {
            return;
        }
        self.preferences.appearance = appearance;
        ctx.set_theme(app_theme::preference(appearance));
        self.save_preferences();
        ctx.request_repaint();
    }
    fn canvas_colors(
        &self,
        ctx: &egui::Context,
    ) -> (settings::Rgb, Option<settings::CheckerColors>) {
        if self.preferences.background == CanvasBackground::Automatic {
            let color = app_theme::palette(ctx).canvas;
            ([color.r(), color.g(), color.b()], None)
        } else {
            self.preferences.background.colors()
        }
    }
    fn load_session_preferences(&mut self) {
        if let Some(path) = &self.settings_path {
            match settings::load(path) {
                Ok(preferences) => self.preferences = preferences,
                Err(ToolError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => self.settings_warning = Some(error.to_string()),
            }
        }
        // Each launch starts with the picture in focus, including older saved layouts.
        self.preferences.list_visible = false;
    }
    fn poll_browser(&mut self, ctx: &egui::Context) {
        if let Some(response) = self.directory_worker.take_result()
            && response.generation == self.directory_generation
            && self.directory_path.as_ref() == Some(&browsing::directory(&response.path))
        {
            self.directory_scanning = false;
            match response.result {
                Ok(listing) => {
                    let previous_paths = self.siblings.clone();
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
                    if self.siblings != previous_paths {
                        self.ai_view.metadata = listing.metadata;
                        self.ai_sync_scope();
                    } else {
                        self.browser_set_metadata(listing.metadata);
                    }
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
    fn refresh_browser(&mut self) {
        if let Some(path) = self.requested_path.clone() {
            self.ai.refresh();
            self.ai_rebuild_view();
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
        let colors = app_theme::palette(ui.ctx());
        let count = self.ai_visible_indices().len();
        let mut hide = false;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("同目录图片")
                    .size(13.0)
                    .strong()
                    .color(colors.text),
            );
            ui.weak(if count == self.siblings.len() {
                format!("{count} 张")
            } else {
                format!("{count} / {}", self.siblings.len())
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                hide = ui
                    .add(CompactAction::new(Icon::Close, "隐藏图片列表"))
                    .clicked();
            });
        });
        if hide {
            self.toggle_list();
            return;
        }
        self.ai_list_header(ui);
        let indices = self.ai_visible_indices();
        if self.directory_scanning {
            ui.weak("正在读取目录…");
        }
        if let Some(warning) = &self.directory_warning {
            ui.label(egui::RichText::new(warning).small().color(colors.warning));
        }
        ui.add_space(4.0);
        let row_height = 60.0;
        let mut scroll = egui::ScrollArea::vertical()
            .animated(false)
            .stick_to_bottom(indices.last() == Some(&self.position))
            .id_salt("directory-rows")
            .auto_shrink([false, false]);
        if self.locate_current {
            scroll = scroll.vertical_scroll_offset(
                indices
                    .iter()
                    .position(|index| *index == self.position)
                    .unwrap_or(0)
                    .saturating_sub(1) as f32
                    * (row_height + ui.spacing().item_spacing.y),
            );
            self.locate_current = false;
        }
        let mut select = None;
        let mut request = None;
        let mut visible = Vec::new();
        self.selected_list_row_rect = None;
        let output = scroll.show_rows(ui, row_height, indices.len(), |ui, rows| {
            for row in rows {
                let index = indices[row];
                let path = self.siblings[index].clone();
                visible.push(path.clone());
                let (_, rect) = ui.allocate_space(Vec2::new(ui.available_width(), row_height));
                let response = ui.interact(
                    rect,
                    ui.make_persistent_id(("directory-row", &path)),
                    egui::Sense::click(),
                );
                let selected = self.requested_path.as_ref() == Some(&path);
                if selected {
                    self.selected_list_row_rect = Some(rect);
                }
                let fill = if selected {
                    colors.selected
                } else if response.hovered() {
                    colors.hover
                } else {
                    Color32::TRANSPARENT
                };
                ui.painter().rect_filled(rect, 5.0, fill);
                if selected {
                    ui.painter().rect_filled(
                        Rect::from_min_size(
                            rect.left_center() - Vec2::new(0.0, 12.0),
                            Vec2::new(2.0, 24.0),
                        ),
                        1.0,
                        colors.accent,
                    );
                }
                if response.has_focus() {
                    ui.painter().rect_stroke(
                        rect,
                        5.0,
                        egui::Stroke::new(1.5, colors.accent),
                        egui::StrokeKind::Inside,
                    );
                }
                let image_rect =
                    Rect::from_min_size(rect.min + Vec2::new(9.0, 7.0), Vec2::splat(46.0));
                ui.painter().rect_filled(image_rect, 3.0, colors.hover);
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
                        let size = image_size * (46.0 / image_size.x.max(image_size.y));
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
                            colors.muted,
                        );
                    }
                } else if request.is_none() {
                    request = Some(path.clone());
                }
                let decision = self.ai.decision(&path);
                if self.ai.settings().show_badges
                    && let Some(badge) = &decision.badge
                {
                    let label = ai_ui::thumbnail_badge_label(&decision, badge);
                    viewer_ui::paint_ai_badge(
                        &ui.painter_at(image_rect),
                        image_rect.left_bottom() - Vec2::new(0.0, 19.0),
                        label,
                        decision.suspected,
                        decision.manual_active,
                        false,
                    );
                }
                let text_rect = Rect::from_min_max(
                    rect.min + Vec2::new(65.0, 4.0),
                    rect.max - Vec2::splat(8.0),
                );
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                let text_painter = ui.painter().with_clip_rect(text_rect);
                let name_color = if selected { colors.accent } else { colors.text };
                let mut title = egui::RichText::new(&*name).size(13.0).color(name_color);
                if selected {
                    title = title.strong();
                }
                let galley = egui::WidgetText::from(title).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Truncate),
                    text_rect.width().max(0.0),
                    egui::TextStyle::Body,
                );
                text_painter.galley(text_rect.min + Vec2::new(0.0, 4.0), galley, name_color);
                let format = path
                    .extension()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_uppercase();
                text_painter.text(
                    text_rect.min + Vec2::new(0.0, 29.0),
                    egui::Align2::LEFT_TOP,
                    format!("{} / {} · {format}", index + 1, self.siblings.len()),
                    egui::FontId::proportional(12.0),
                    colors.muted,
                );
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::SelectableLabel,
                        true,
                        selected,
                        format!("{} {} · {}", index + 1, name, decision.label),
                    )
                });
                response.clone().on_hover_text(format!(
                    "{}\n{}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    decision.label
                ));
                if response.clicked()
                    || (response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    select = Some(index);
                }
            }
        });
        self.ai.prioritize_visible(&visible);
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
            // This starts after logic() has decided whether to poll; workers never lock egui.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(16));
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
        self.undo_image_edit(ctx);
    }
    fn refresh_texture(&mut self, ctx: &egui::Context) {
        self.crop_selecting = false;
        self.crop_anchor = None;
        self.close_export();
        if let Some(image) = self.pixels.clone() {
            if let Err(error) = self.upload_pixels(&image, ctx, &Cancellation::default()) {
                // Edits have already changed document identity; never label old pixels as the edit.
                self.canvas_unavailable = true;
                self.retire_tiles();
                self.error = Some(error.to_string());
            }
        } else {
            self.retire_tiles();
        }
    }
    fn retire_tiles(&mut self) {
        for tile in self.tiles.drain(..) {
            self.retired_texture_reservations
                .push((self.frames + 2, tile._reservation));
            self.retired_displays.push((self.frames + 2, tile._display));
        }
    }
    fn create_sticker(&mut self, ctx: &egui::Context) {
        if !self.canvas_actions_ready() || self.tiles.is_empty() {
            return;
        }
        self.status = match self.stickers.push(
            self.document_name(),
            self.dimensions(),
            self.tiles.clone(),
            self.preferences.background,
        ) {
            Ok(()) => "已创建独立贴图；主窗口可继续切图".into(),
            Err(error) => {
                let message = error.to_string();
                if self.smoke.is_some() {
                    self.error = Some(message.clone());
                }
                message
            }
        };
        self.notice_until = Some(Instant::now() + std::time::Duration::from_secs(5));
        ctx.request_repaint();
    }
    fn upload_pixels(
        &mut self,
        image: &Arc<light_image_tool::memory::Pixels>,
        ctx: &egui::Context,
        cancel: &Cancellation,
    ) -> light_image_tool::Result<()> {
        // Normal loads, edits and animation frames arrive with this work already done off-thread.
        let display = image.prepare_display(cancel)?.clone();
        let same_grid = self.tiles.len() == display.tiles.len()
            && !self.tiles.iter().any(|t| {
                self.comparison.shares_texture(t.texture.id())
                    || self.stickers.shares_texture(t.texture.id())
            })
            && self.tiles.iter().zip(&display.tiles).all(|(old, new)| {
                old.x == new.x
                    && old.y == new.y
                    && [old.w as usize, old.h as usize] == new.image.size
            });
        let mut reservation = light_image_tool::memory::reserve(
            if same_grid {
                0
            } else {
                image.as_raw().len() as u64
            } + light_image_tool::memory::OVERHEAD,
        )?;
        cancel.check()?;
        if same_grid {
            for (tile, prepared) in self.tiles.iter_mut().zip(&display.tiles) {
                tile.texture.set_partial(
                    [0, 0],
                    prepared.image.clone(),
                    self.inspector.texture_options(),
                );
                let previous = std::mem::replace(&mut tile._display, display.clone());
                self.retired_displays.push((self.frames + 2, previous));
            }
        } else {
            let mut tiles = Vec::with_capacity(display.tiles.len());
            for prepared in &display.tiles {
                let [w, h] = prepared.image.size;
                let texture = ctx.load_texture(
                    format!("image-{}-{}-{}", self.generation, prepared.x, prepared.y),
                    prepared.image.clone(),
                    self.inspector.texture_options(),
                );
                tiles.push(Tile {
                    _reservation: Arc::new(reservation.split((w * h * 4) as u64)?),
                    _display: display.clone(),
                    texture,
                    x: prepared.x,
                    y: prepared.y,
                    w: w as u32,
                    h: h as u32,
                });
            }
            self.retire_tiles();
            self.tiles = tiles;
        }
        self.retired_texture_reservations
            .push((self.frames + 1, Arc::new(reservation)));
        self.current_has_alpha = display.has_alpha;
        self.canvas_unavailable = false;
        self.inspector.on_pixels_changed(Some(image));
        Ok(())
    }
    fn poll(&mut self, ctx: &egui::Context) {
        self.poll_rename(ctx);
        self.inspector.set_alpha_available(
            !self.comparison.active()
                && self.playback.is_none()
                && !self.loading
                && self.work.is_none(),
        );
        self.inspector.poll(ctx, self.pixels.as_ref(), self.frames);
        self.ai_poll(ctx);
        self.poll_animation(ctx);
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
        if self.loading && !self.loader.is_busy() {
            self.loading = false;
            self.load_started = None;
            if self.load_cancel.check().is_err() {
                self.restore_browser_selection();
                self.status = "已取消打开".into();
            }
        }
        if self.thumbnail_busy.is_some() && !self.thumbnail_worker.is_busy() {
            self.thumbnail_busy = None;
        }
        if self.directory_scanning && !self.directory_worker.is_busy() {
            self.directory_scanning = false;
        }
        let done = self.work.as_ref().map(|w| w.receiver.try_recv());
        if let Some(Err(mpsc::TryRecvError::Disconnected)) = &done {
            self.work = None;
            self.pending_intent = None;
            self.pending_export = false;
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
            if matches!(
                &result,
                WorkResult::Edited(_) | WorkResult::ClipboardRead(_)
            ) && work.cancel.check().is_err()
            {
                self.status = "操作已取消".into();
                return;
            }
            match result {
                WorkResult::Edited(Ok(image)) => {
                    if let Some(previous) = self.pixels.take() {
                        self.record_edit_history(previous);
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
                    self.exported_pixels = self.pixels.as_ref().map(Arc::downgrade);
                    self.status = format!("已导出新副本：{}", path.display());
                    self.notice_until = Some(Instant::now() + std::time::Duration::from_secs(6));
                    ctx.request_repaint_after(std::time::Duration::from_secs(6));
                    log::info!("export committed: {}", path.display());
                    if self.pending_export {
                        self.pending_export = false;
                        if let Some(intent) = self.pending_intent.take() {
                            self.perform_intent(intent, ctx);
                        }
                    }
                }
                WorkResult::ClipboardRead(Ok(input)) => match input {
                    light_image_tool::clipboard::ClipboardInput::Pixels(image) => {
                        self.set_temporary_image(image, ctx)
                    }
                    light_image_tool::clipboard::ClipboardInput::File(path) => self.open_now(path),
                },
                WorkResult::ClipboardCopied(Ok(message)) | WorkResult::FileLocated(Ok(message)) => {
                    self.status = message;
                    self.notice_until = Some(Instant::now() + std::time::Duration::from_secs(2));
                    ctx.request_repaint_after(std::time::Duration::from_secs(2));
                }
                WorkResult::Edited(Err(ToolError::Cancelled))
                | WorkResult::Exported(Err(ToolError::Cancelled))
                | WorkResult::ClipboardRead(Err(ToolError::Cancelled))
                | WorkResult::ClipboardCopied(Err(ToolError::Cancelled))
                | WorkResult::FileLocated(Err(ToolError::Cancelled)) => {
                    self.pending_intent = None;
                    self.pending_export = false;
                    self.status = "操作已取消".into()
                }
                WorkResult::Edited(Err(e))
                | WorkResult::Exported(Err(e))
                | WorkResult::ClipboardRead(Err(e))
                | WorkResult::ClipboardCopied(Err(e))
                | WorkResult::FileLocated(Err(e)) => {
                    self.pending_intent = None;
                    self.pending_export = false;
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
        self.load_started = None;
        let result = if self.load_cancel.check().is_err() {
            Err(ToolError::Cancelled)
        } else {
            response.result
        };
        match result {
            Ok(image) => {
                if let Err(error) =
                    self.upload_pixels(&image.pixels, ctx, &self.load_cancel.clone())
                {
                    self.error = Some(error.to_string());
                    self.status = if self.loaded.is_some() {
                        "新图片尚未完成显示准备；上一张图片仍可查看"
                    } else {
                        "图片显示准备失败；可重新打开"
                    }
                    .into();
                    return;
                }
                if image.animation.is_some() {
                    self.close_comparison();
                }
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
                self.playback = image.animation.clone().map(Playback::new);
                self.loaded = Some(image);
                self.source = DocumentSource::File;
                self.temporary_original = None;
                self.exported_pixels = None;
                self.clear_edit_history();
                self.edited = false;
                self.long_image = false;
                self.fit_width = false;
                self.long_scroll = 0.0;
                self.fit = true;
                self.pan = Vec2::ZERO;
                let [w, h] = self.dimensions();
                self.crop = [0, 0, w, h];
                self.resize = [w, h];
            }
            Err(ToolError::Cancelled) => {
                self.restore_browser_selection();
                self.status = "已取消打开".into();
            }
            Err(e) => {
                // Retain the attempted row after failure so Next can move beyond a bad file.
                self.error = Some(e.to_string());
                self.status = if self.loaded.is_some() {
                    "打开失败；上一张图片仍可查看"
                } else {
                    "打开失败；请选择另一张 JPEG / PNG / GIF / WebP / BMP / ICO 图片"
                }
                .into();
            }
        }
    }
    fn start_edit(&mut self, op: EditOp, _ctx: &egui::Context) {
        if self.rename.blocks_document() {
            return;
        }
        if self.playback.is_some() {
            self.extract_dialog = true;
            return;
        }
        if self.work.is_some() || self.loading {
            return;
        }
        let Some(pixels) = self.pixels.clone() else {
            return;
        };
        self.close_export();
        self.loader.clear_cache();
        let extra = match edit::extra_bytes(&pixels, &op) {
            Ok(e) => e,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        let (output_w, output_h) = match &op {
            EditOp::Crop { width, height, .. } | EditOp::Resize { width, height } => {
                (*width, *height)
            }
            EditOp::Rotate90 | EditOp::Rotate270 => (pixels.height(), pixels.width()),
            EditOp::FlipHorizontal => pixels.dimensions(),
        };
        let display_peak = light_image_tool::memory::rgba_bytes(output_w, output_h) * 3
            + light_image_tool::memory::OVERHEAD;
        self.evict_history(extra.max(display_peak));
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result = protected(
                || {
                    let edited = edit::apply(&pixels, &op, &token)?;
                    edited.prepare_display(&token)?;
                    Ok(edited)
                },
                "编辑异常；原图未修改",
            );
            let _ = tx.send(WorkResult::Edited(result));
        });
        self.work = Some(Work {
            generation: self.generation,
            cancel,
            receiver: rx,
            export_thread: Some(thread),
        });
        self.status = "正在处理编辑副本…".into();
    }
    fn request_animation_frame(&mut self, restart: bool) {
        let Some(playback) = &mut self.playback else {
            return;
        };
        if playback.in_flight.is_some() || playback.ended {
            return;
        }
        playback.sequence = playback.sequence.wrapping_add(1);
        let id = playback.sequence;
        playback.in_flight = Some(id);
        self.animation_worker.request(AnimationRequest {
            generation: self.generation,
            request_id: id,
            source: playback.source.clone(),
            cancel: playback.cancel.clone(),
            restart,
        });
    }
    fn update_animation_activity(&mut self, ctx: &egui::Context) {
        let blocked = self.loading
            || self.rename.blocks_document()
            || self.editing
            || self.export_dialog
            || self.path_dialog
            || self.more_open
            || egui::Popup::is_any_open(ctx)
            || self.error.is_some()
            || self.pending_intent.is_some()
            || self.extract_dialog
            || self.ai_view.confirm.is_some()
            || self.work.is_some()
            || ctx.input(|i| !i.raw.focused || i.viewport().minimized == Some(true));
        if let Some(playback) = &mut self.playback {
            if blocked && !playback.suspended {
                playback.clock.pause();
                playback.suspended = true;
            } else if !blocked && playback.suspended {
                playback.suspended = false;
                if !playback.manual_paused && !playback.ended {
                    playback.clock.resume(Instant::now());
                }
            }
        }
    }
    fn toggle_playback(&mut self) {
        if let Some(playback) = &mut self.playback {
            if playback.ended || playback.requires_restart {
                self.restart_animation();
                return;
            }
            playback.manual_paused = !playback.manual_paused;
            if playback.manual_paused {
                playback.clock.pause();
            } else if !playback.suspended {
                playback.clock.resume(Instant::now());
            }
        }
    }
    fn step_animation(&mut self) {
        if let Some(playback) = &mut self.playback {
            if playback.ended || playback.requires_restart {
                return;
            }
            playback.manual_paused = true;
            playback.clock.pause();
            playback.step_requested = true;
        }
    }
    fn restart_animation(&mut self) {
        let Some(playback) = &mut self.playback else {
            return;
        };
        playback.cancel.cancel();
        self.animation_worker.stop();
        playback.cancel = Cancellation::default();
        playback.pending = None;
        playback.in_flight = None;
        playback.ended = false;
        playback.eof_pending = false;
        playback.requires_restart = false;
        playback.manual_paused = false;
        playback.step_requested = true;
        playback.warning = None;
        playback.clock = PlaybackClock::new(playback.source.first_delay);
        if !playback.suspended {
            playback.clock.resume(Instant::now());
        }
        self.request_animation_frame(true);
    }
    fn poll_animation(&mut self, ctx: &egui::Context) {
        self.update_animation_activity(ctx);
        if let Some(response) = self.animation_worker.take_result()
            && response.generation == self.generation
            && let Some(playback) = &mut self.playback
            && playback.in_flight == Some(response.request_id)
        {
            playback.in_flight = None;
            match response.result {
                Ok(Some(frame)) => playback.pending = Some(frame),
                Ok(None) => {
                    playback.eof_pending = true;
                }
                Err(error) => {
                    playback.clock.pause();
                    playback.manual_paused = true;
                    playback.requires_restart = true;
                    playback.step_requested = false;
                    playback.warning = Some(format!(
                        "动画已暂停，保留最后正确帧：{error}；只能从头重新播放"
                    ));
                }
            }
        }
        let now = Instant::now();
        if let Some(playback) = &mut self.playback
            && playback.eof_pending
            && !playback.suspended
            && (playback.clock.due(now) || playback.step_requested)
        {
            playback.ended = true;
            playback.clock.pause();
            playback.step_requested = false;
            playback.eof_pending = false;
            playback.elapsed = playback.elapsed.saturating_add(playback.frame_delay);
        }
        let display = self.playback.as_ref().is_some_and(|p| {
            !p.suspended && (p.step_requested || p.clock.due(now)) && p.pending.is_some()
        });
        if display {
            let playback = self.playback.as_mut().unwrap();
            let frame = playback.pending.take().unwrap();
            playback.step_requested = false;
            playback.frame_index = frame.frame_index;
            playback.traversal = frame.traversal;
            playback.elapsed = frame.elapsed;
            playback.frame_delay = frame.delay;
            if frame.delay_normalized {
                playback.warning = Some("部分帧延时不足 20 ms，按 20 ms 播放".into());
            }
            if !playback.clock.accept_frame(frame.delay, now) {
                playback.manual_paused = true;
                playback.warning = Some("解码或绘制延迟过大，动画已暂停；可从头播放".into());
            }
            self.pixels = Some(frame.pixels.clone());
            if let Some(loaded) = &mut self.loaded {
                loaded.pixels = frame.pixels;
            }
            self.refresh_texture(ctx);
        }
        let needs = self.playback.as_ref().is_some_and(|p| {
            !p.suspended
                && !p.ended
                && !p.eof_pending
                && p.pending.is_none()
                && p.in_flight.is_none()
                && (p.clock.is_playing() || p.step_requested)
        });
        if needs {
            self.request_animation_frame(false);
        }
        if let Some(playback) = &self.playback {
            if playback.in_flight.is_some() {
                ctx.request_repaint_after(std::time::Duration::from_millis(8));
            } else if let Some(delay) = playback.clock.wake_after(now) {
                ctx.request_repaint_after(delay.max(std::time::Duration::from_millis(1)));
            }
        }
    }
    fn playback_controls(&mut self, ui: &mut egui::Ui) {
        let Some(playback) = &self.playback else {
            return;
        };
        let paused = playback.manual_paused || playback.ended;
        let busy = playback.ended
            || playback.requires_restart
            || (playback.in_flight.is_some() && playback.step_requested);
        let label = format!(
            "{:.2}s · 第 {} 帧{}",
            playback.elapsed.as_secs_f64(),
            playback.frame_index,
            if playback.ended { " · 结束" } else { "" }
        );
        let warning = playback.warning.clone();
        ui.horizontal_centered(|ui| {
            if ui
                .add(CompactAction::new(
                    if paused { Icon::Play } else { Icon::Pause },
                    if paused {
                        "播放动画"
                    } else {
                        "暂停动画"
                    },
                ))
                .clicked()
            {
                self.toggle_playback();
            }
            if ui
                .add_enabled(!busy, CompactAction::new(Icon::Step, "下一帧（暂停）"))
                .clicked()
            {
                self.step_animation();
            }
            if ui
                .add(CompactAction::new(Icon::Restart, "从头播放"))
                .clicked()
            {
                self.restart_animation();
            }
            ui.small(label).on_hover_text(
                warning.unwrap_or_else(|| "按文件时序播放；总时长尚未完整统计".into()),
            );
        });
    }
    fn extract_current_frame(&mut self, ctx: &egui::Context) {
        let Some(playback) = self.playback.take() else {
            self.extract_dialog = false;
            return;
        };
        playback.cancel.cancel();
        self.animation_worker.stop();
        let name = self.document_name();
        let frame = playback.frame_index;
        self.temporary_color_status = playback.source.color_status.clone();
        self.temporary_color_managed = playback.source.color_managed;
        self.temporary_original = self.pixels.clone();
        self.loaded = None;
        self.source = DocumentSource::ExtractedFrame { name, frame };
        self.generation += 1;
        self.directory_generation += 1;
        self.directory_worker.cancel();
        self.directory_scanning = false;
        self.siblings.clear();
        self.ai.set_scope(&[], None);
        self.ai_rebuild_view();
        self.requested_path = None;
        self.directory_path = None;
        self.position = 0;
        self.thumbnail_worker.cancel();
        self.thumbnail_busy = None;
        self.thumbnail_cache.clear();
        if let Some(task) = &self.evidence_task {
            task.cancel.cancel();
        }
        self.evidence = None;
        self.evidence_attempted = true;
        self.clear_edit_history();
        self.edited = true;
        self.exported_pixels = None;
        self.extract_dialog = false;
        self.editing = true;
        self.info_open = false;
        self.status = "当前帧 · 静态副本 · 尚未保存".into();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "gloamere-image · {}",
            self.document_name()
        )));
    }
    fn fit_window(&mut self) {
        self.fit = true;
        self.fit_width = false;
        self.long_image = false;
        self.long_scroll = 0.0;
        self.pan = Vec2::ZERO;
    }
    fn enter_long_image(&mut self) {
        if self.pixels.is_none() {
            return;
        }
        self.long_image = true;
        self.fit_width = true;
        self.fit = false;
        self.long_scroll = 0.0;
        self.pan = Vec2::ZERO;
        self.status = "长图模式：滚轮纵向滚动，Ctrl+滚轮缩放；F 返回适应窗口".into();
        self.notice_until = Some(Instant::now() + std::time::Duration::from_secs(3));
    }
    fn actual_pixels(&mut self) {
        if self.long_image {
            let anchor = self
                .canvas_viewport
                .map_or(0.0, |r| r.height() / 2.0 - 12.0);
            self.long_scroll = (anchor + self.long_scroll) / self.zoom - anchor;
        } else {
            self.pan = Vec2::ZERO;
        }
        self.fit = false;
        self.fit_width = false;
        self.zoom = 1.0;
    }
    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.fullscreen = !self.fullscreen;
        self.fullscreen_activity = ctx.input(|i| i.time);
        self.toolbar_hidden = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        if self.fullscreen {
            self.status = "全屏 · Esc 逐层返回；F11 退出全屏".into();
            self.notice_until = Some(Instant::now() + std::time::Duration::from_secs(2));
        }
    }
    fn document_name(&self) -> String {
        match &self.source {
            DocumentSource::File => self
                .loaded
                .as_ref()
                .map(|l| {
                    l.path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .unwrap_or_else(|| "未打开图片".into()),
            DocumentSource::Clipboard => "剪贴板图片".into(),
            DocumentSource::ExtractedFrame { name, frame } => {
                format!("{name} · 第 {frame} 帧 · 静态副本")
            }
        }
    }
    fn color_managed(&self) -> bool {
        self.loaded
            .as_ref()
            .map_or(self.temporary_color_managed, |l| l.color_managed)
    }
    fn suggested_export(&self) -> String {
        if let Some(image) = &self.loaded {
            return export::suggest_path(&image.path, self.format)
                .to_string_lossy()
                .into();
        }
        // A suggested destination is not a claimed source path.
        let stem = match self.source {
            DocumentSource::ExtractedFrame { .. } => "当前帧-副本",
            _ => "剪贴板图片-副本",
        };
        format!(
            "{stem}.{}",
            if self.format == ExportFormat::Png {
                "png"
            } else {
                "jpg"
            }
        )
    }
    fn begin_export(&mut self) {
        if self.rename.blocks_document() {
            return;
        }
        if self.playback.is_some() {
            self.extract_dialog = true;
            return;
        }
        if self.loading || self.work.is_some() || self.pixels.is_none() {
            return;
        }
        if !self.color_managed() {
            self.error = Some("此图片的 ICC 未完整处理，暂不能导出校准的色彩副本".into());
            return;
        }
        self.export_name = self.suggested_export();
        self.export_dialog = true;
        self.export_generation = Some(self.generation);
    }
    fn start_export(&mut self, ctx: &egui::Context) {
        if self.work.is_some() || self.loading || self.rename.blocks_document() {
            return;
        }
        let Some(pixels) = self.pixels.clone() else {
            return;
        };
        if self.export_generation != Some(self.generation) || !self.color_managed() {
            self.close_export();
            self.pending_intent = None;
            self.pending_export = false;
            self.error = Some("导出确认已失效或 ICC 未校准，请重新打开当前图片的导出窗口".into());
            return;
        }
        let original = self.loaded.as_ref().map(|l| l.path.clone());
        self.loader.clear_cache();
        self.evict_history(pixels.as_raw().len() as u64 * 4 + light_image_tool::memory::OVERHEAD);
        let options = ExportOptions {
            destination: PathBuf::from(&self.export_name),
            format: self.format,
            quality: self.quality,
            matte: self.matte,
        };
        self.work = Some(Work::spawn_export(
            self.generation,
            move |token| match original {
                Some(original) => export::save_new(&pixels, &original, &options, &token),
                None => export::save_temporary(&pixels, &options, &token),
            },
        ));
        ctx.request_repaint();
        self.close_export();
        self.status = "正在导出新副本…".into();
    }

    fn start_provenance(&mut self, _ctx: &egui::Context) {
        if self.rename.blocks_document() {
            return;
        }
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
        let (tx, rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                provenance::inspect(&path, &hash, &token)
            }))
            .unwrap_or_else(|_| Err(ToolError::Invalid("来源核验异常；结果不完整".into())));
            let _ = tx.send(result);
        });
        self.evidence_task = Some(ProvenanceTask {
            generation: self.generation,
            cancel,
            receiver: rx,
            thread: Some(thread),
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
            self.list_viewport_rect = None;
            self.selected_list_row_rect = None;
        }
        self.save_preferences();
    }
    fn toolbar_metadata(&self, compact: bool) -> String {
        let Some(image) = &self.loaded else {
            return "JPEG / PNG / GIF / WebP / BMP / ICO".into();
        };
        // Navigation selects its target before decoding; metadata follows the
        // image that is still visible until the new image is committed.
        let sequence = self
            .siblings
            .iter()
            .position(|path| path == &image.path)
            .map(|index| format!("{} / {}", index + 1, self.siblings.len()));
        let edited = if self.edited { " · 已编辑" } else { "" };
        if compact && let Some(sequence) = &sequence {
            return format!("{sequence}{edited}");
        }
        let [w, h] = self.dimensions();
        let size = if image.file_bytes >= 1024 * 1024 {
            format!("{:.1} MB", image.file_bytes as f64 / 1048576.0)
        } else {
            format!("{} KB", image.file_bytes.div_ceil(1024))
        };
        let prefix = sequence.map_or_else(String::new, |sequence| format!("{sequence} · "));
        format!("{prefix}{w} × {h} · {size}{edited}")
    }
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let colors = app_theme::palette(&ctx);
        let ready = self.pixels.is_some() && !self.loading && self.work.is_none();
        let modal = self.path_dialog
            || self.export_dialog
            || self.error.is_some()
            || self.pending_intent.is_some()
            || self.extract_dialog
            || self.ai_view.confirm.is_some();
        let (area, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), viewer_ui::TOOLBAR_HEIGHT),
            egui::Sense::hover(),
        );
        let layout = ToolbarLayout::new(area);
        ui.painter().rect(
            layout.island,
            8.0,
            colors.toolbar,
            egui::Stroke::new(1.0, colors.border),
            egui::StrokeKind::Inside,
        );
        if let Some(rect) = layout.metadata {
            let text = self.toolbar_metadata(rect.width() < 270.0);
            viewer_ui::edge_label(ui, rect, &text, false).on_hover_text(text);
        }
        if let Some(rect) = layout.zoom {
            viewer_ui::edge_label(ui, rect, &format!("{:.0}%", self.zoom * 100.0), true)
                .on_hover_text("实际显示比例 · 1 为实际像素，F 适应窗口");
        }
        for x in layout.separators.iter().take(1) {
            ui.painter().line_segment(
                [
                    Pos2::new(*x, area.center().y - 8.0),
                    Pos2::new(*x, area.center().y + 8.0),
                ],
                egui::Stroke::new(1.0, colors.separator),
            );
        }
        for (index, icon) in Icon::ALL.into_iter().enumerate() {
            if !layout.visible[index] {
                continue;
            }
            let enabled = !(modal || icon == Icon::More && self.more_trigger_release_blocked)
                && match icon {
                    Icon::Previous | Icon::Next => !self.ai_visible_indices().is_empty(),
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
                        self.fit_width = false;
                        self.fit = false;
                        self.zoom = (self.zoom / 1.25).max(0.01);
                    }
                    Icon::Plus => {
                        self.fit_width = false;
                        self.fit = false;
                        self.zoom = (self.zoom * 1.25).min(32.0);
                    }
                    Icon::Fit => self.fit_window(),
                    Icon::Actual => self.actual_pixels(),
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
    fn bottom_controls(&mut self, ui: &mut egui::Ui) {
        if self.toolbar_hidden {
            return;
        }
        let ctx = ui.ctx().clone();
        let (background, _) = self.canvas_colors(&ctx);
        egui::Panel::bottom("viewer-toolbar")
            .exact_size(viewer_ui::TOOLBAR_HEIGHT)
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(
                background[0],
                background[1],
                background[2],
            )))
            .show(ui, |ui| {
                if self.rename.blocks_document() {
                    ui.disable();
                }
                self.toolbar(ui)
            });
        if self.playback.is_some() {
            egui::Panel::bottom("animation-controls")
                .exact_size(44.0)
                .frame(egui::Frame::NONE.fill(app_theme::palette(&ctx).window))
                .show(ui, |ui| self.playback_controls(ui));
        }
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
                    ui.spacing_mut().interact_size.y = 34.0;
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.spacing_mut().button_padding = Vec2::new(6.0, 4.0);
                    egui::ScrollArea::vertical()
                        .max_height((ctx.viewport_rect().height() - 92.0).max(120.0))
                        .show(ui, |ui| {
                            close |= self.more_menu_contents(ui, ready, newly_opened, ctx);
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
        let outside = !egui::Popup::is_any_open(ctx)
            && ctx.input(|input| {
                (input.pointer.any_pressed() || input.pointer.any_click())
                    && input.pointer.interact_pos().is_some_and(|pos| {
                        !output.response.rect.contains(pos)
                            && !self.more_button_rect.is_some_and(|rect| rect.contains(pos))
                    })
            });
        if close || outside || trigger_pressed {
            self.more_open = false;
            egui::Popup::close_all(ctx);
            self.more_trigger_release_blocked = trigger_pressed;
            ctx.memory_mut(|memory| memory.request_focus(viewer_ui::icon_id(Icon::More)));
        }
    }
    fn inspector_panel(&mut self, ui: &mut egui::Ui) {
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
        let sticky_crop = self.editing && self.crop_selecting;
        #[cfg(test)]
        {
            self.crop_apply_rect = None;
        }
        let footer = if self.editing { 58.0 } else { 0.0 };
        let height = (ui.available_height() - footer).max(16.0);
        egui::ScrollArea::vertical()
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| self.inspector(ui));
        if self.editing {
            ui.separator();
            ui.horizontal(|ui| {
                if sticky_crop {
                    let response = ui.add_enabled(
                        self.work.is_none() && !self.loading,
                        CompactAction::new(Icon::CropApply, "应用裁剪"),
                    );
                    #[cfg(test)]
                    {
                        self.crop_apply_rect = Some(response.rect);
                    }
                    if response.clicked() {
                        self.apply_crop(&ui.ctx().clone());
                    }
                }
                if ui
                    .add_enabled(
                        self.work.is_none() && !self.loading,
                        CompactAction::new(Icon::Export, "导出新副本"),
                    )
                    .clicked()
                {
                    self.begin_export();
                }
                ui.small(if self.unsaved() {
                    "有未导出的改动"
                } else {
                    "只写入新副本"
                });
            });
        }
    }
    fn inspector(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.pixels.is_some() {
            let path = self.loaded.as_ref().map(|l| l.path.clone());
            let [w, h] = self.dimensions();
            if !self.editing {
                ui.label(egui::RichText::new(self.document_name()).strong());
                ui.label(format!("{w} × {h} 像素"));
                self.icon_size_selector(ui);
                ui.separator();
                self.ai_details(ui);
                ui.separator();
                if let Some(image) = &self.loaded {
                    ui.label(format!(
                        "{:?} · {:.2} MiB",
                        image.format,
                        image.file_bytes as f64 / 1048576.0
                    ));
                }
                ui.label(
                    if self
                        .loaded
                        .as_ref()
                        .is_some_and(|image| image.format == image::ImageFormat::Jpeg)
                    {
                        "透明度不适用（JPEG 不含 alpha）"
                    } else if self.current_has_alpha {
                        "含透明像素"
                    } else {
                        "无透明像素"
                    },
                );
                if let Some(image) = &self.loaded {
                    ui.label(format!("EXIF 方向：{}（已应用）", image.orientation));
                    ui.label(&image.color_status);
                    for warning in &image.warnings {
                        ui.colored_label(app_theme::palette(ui.ctx()).warning, warning);
                    }
                } else {
                    ui.label(&self.temporary_color_status);
                    ui.small("临时像素没有可信原文件字节、EXIF 或 C2PA；复制不继承有效签名。");
                }
                ui.separator();
            }
            if self.editing {
                ui.add_enabled_ui(self.work.is_none() && !self.loading, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add(
                                CompactAction::new(Icon::Crop, "裁剪参数")
                                    .selected(!self.resize_tool),
                            )
                            .clicked()
                        {
                            self.resize_tool = false;
                        }
                        if ui
                            .add(
                                CompactAction::new(Icon::ResizeApply, "缩放参数")
                                    .selected(self.resize_tool),
                            )
                            .clicked()
                        {
                            self.resize_tool = true;
                            self.crop_selecting = false;
                            self.crop_anchor = None;
                        }
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
                    if !self.resize_tool {
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
                    } else {
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
                                    .max(1.0)
                                    as u32;
                            } else if before[1] != self.resize[1] {
                                self.resize[0] = (self.resize[1] as f64 * w as f64 / h as f64)
                                    .round()
                                    .max(1.0)
                                    as u32;
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
                            self.request_intent(PendingIntent::Restore, &ctx);
                        }
                    });
                });
                ui.separator();
            }
            if !self.editing {
                ui.heading("来源证据");
                if self.edited {
                    ui.colored_label(
                        app_theme::palette(ui.ctx()).warning,
                        "编辑副本：原图签名不适用于当前像素",
                    );
                }
                if matches!(self.source, DocumentSource::File) {
                    self.evidence_ui(ui);
                }
                ui.separator();
            } else if ui
                .add(ActionButton::new(Icon::Info, "信息与来源").menu())
                .clicked()
            {
                self.editing = false;
                self.info_open = true;
            }
            ui.small("仅本地处理 · 无图片上传 · 无遥测");
            ui.small(concat!("未签名测试版 ", env!("CARGO_PKG_VERSION")));
            ui.collapsing("运行信息", |ui| {
                ui.small(&self.adapter);
                ui.small(format!("UI 绘制次数：{}", self.frames));
                ui.small(format!(
                    "本次运行 {:.0} 秒",
                    self.start.elapsed().as_secs_f64()
                ));
                if let Some(path) = &path {
                    ui.small(path.to_string_lossy());
                }
            });
        } else {
            ui.label("打开一张本地图片后，在这里核对尺寸、透明度与来源。");
            ui.separator();
            ui.small("支持 JPEG / PNG / GIF / WebP / BMP / ICO");
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
        self.canvas_viewport = Some(area);
        let menu_was_open = egui::Popup::is_any_open(ui.ctx());
        if self
            .smoke
            .as_ref()
            .is_some_and(|smoke| smoke.show_context_menu)
            && !self.loading
            && self.work.is_none()
        {
            let started = self
                .smoke
                .as_mut()
                .unwrap()
                .context_menu_started
                .get_or_insert_with(Instant::now);
            if started.elapsed() < std::time::Duration::from_millis(250) {
                ui.ctx().request_repaint_after(
                    std::time::Duration::from_millis(250) - started.elapsed(),
                );
            }
            egui::Popup::context_menu(&response)
                .at_position(area.center())
                .open(true)
                .show(|ui| self.canvas_context_menu(ui));
        } else {
            response.context_menu(|ui| self.canvas_context_menu(ui));
        }
        let canvas_input = !menu_was_open && !egui::Popup::is_any_open(ui.ctx());
        let painter = ui.painter_at(area);
        let colors = app_theme::palette(ui.ctx());
        let (background, checker) = self.canvas_colors(ui.ctx());
        painter.rect_filled(
            area,
            0.0,
            Color32::from_rgb(background[0], background[1], background[2]),
        );
        if let Some(colors) = checker {
            viewer_ui::paint_checker(&painter, area, colors);
        }
        let dark_canvas =
            u32::from(background[0]) + u32::from(background[1]) + u32::from(background[2]) < 384;
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
            if !self.loading {
                painter.text(
                    area.center() - Vec2::new(0.0, 35.0),
                    egui::Align2::CENTER_CENTER,
                    "拖入图片，或 Ctrl+O",
                    egui::FontId::proportional(25.0),
                    text_color,
                );
                painter.text(
                    area.center() + Vec2::new(0.0, 10.0),
                    egui::Align2::CENTER_CENTER,
                    "也可显式粘贴剪贴板图片 · Ctrl+V",
                    egui::FontId::proportional(15.0),
                    text_color,
                );
            }
            if canvas_input && response.double_clicked() {
                self.open_dialog();
            }
            return;
        }
        if self.canvas_unavailable {
            let panel = Rect::from_center_size(
                area.center(),
                Vec2::new(
                    (area.width() - 24.0).clamp(140.0, 380.0),
                    area.height().min(86.0),
                ),
            );
            painter.rect_filled(panel, 6.0, colors.popover);
            painter.text(
                panel.center() - Vec2::new(0.0, 20.0),
                egui::Align2::CENTER_CENTER,
                "画布上传未完成；未显示旧图片",
                egui::FontId::proportional(15.0),
                colors.text,
            );
            let retry =
                Rect::from_center_size(panel.center() + Vec2::new(0.0, 18.0), Vec2::splat(32.0));
            if ui
                .put(retry, CompactAction::new(Icon::Refresh, "重试画布上传"))
                .clicked()
            {
                self.refresh_texture(&ui.ctx().clone());
            }
            return;
        }
        let [w, h] = self.dimensions();
        let ppp = ui.ctx().pixels_per_point();
        let base = Vec2::new(w as f32 / ppp, h as f32 / ppp);
        if self.long_image && self.fit_width {
            self.zoom = ((area.width() - 40.0) / base.x).clamp(0.01, 32.0);
        } else if self.fit {
            self.zoom = ((area.width() - 40.0) / base.x)
                .min((area.height() - 40.0) / base.y)
                .clamp(0.01, 1.0);
        }
        if canvas_input && response.dragged_by(egui::PointerButton::Secondary) {
            self.crop_anchor = None;
        }
        if canvas_input
            && response.dragged()
            && (!self.crop_selecting || response.dragged_by(egui::PointerButton::Secondary))
        {
            let delta = ui.input(|i| i.pointer.delta());
            if self.long_image {
                self.long_scroll -= delta.y;
                self.pan.x += delta.x;
            } else {
                self.pan += delta;
            }
            self.fit = false;
        }
        if canvas_input && response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.01 {
                if self.long_image && !ui.input(|i| i.modifiers.ctrl) {
                    self.long_scroll -= scroll;
                } else {
                    let old = self.zoom;
                    self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.01, 32.0);
                    self.fit = false;
                    self.fit_width = false;
                    if let Some(mouse) = ui.input(|i| i.pointer.hover_pos()) {
                        if self.long_image {
                            let anchor = mouse.y - area.top() - 12.0;
                            self.long_scroll =
                                (anchor + self.long_scroll) * self.zoom / old - anchor;
                            self.pan.x = (mouse.x - area.center().x)
                                - (mouse.x - area.center().x - self.pan.x) * self.zoom / old;
                        } else {
                            self.pan = (mouse - area.center())
                                - (mouse - area.center() - self.pan) * (self.zoom / old);
                        }
                    }
                }
            }
        }
        if canvas_input
            && response.double_clicked()
            && !self.crop_selecting
            && !self.inspector.is_enabled()
        {
            self.fit_window();
        }
        let size = base * self.zoom;
        let max_scroll = (size.y - area.height() + 24.0).max(0.0);
        if self.long_to_bottom {
            self.long_scroll = max_scroll;
            self.long_to_bottom = false;
        }
        self.long_scroll = self.long_scroll.clamp(0.0, max_scroll);
        let top = if self.long_image {
            Pos2::new(
                area.center().x + self.pan.x - size.x / 2.0,
                area.top() + 12.0 - self.long_scroll,
            )
        } else {
            area.center() + self.pan - size / 2.0
        };
        let rect = Rect::from_min_size(top, size);
        self.canvas_image_rect = Some(rect);
        if self.crop_selecting
            && canvas_input
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
        let alpha_drawn = self
            .pixels
            .as_ref()
            .is_some_and(|pixels| self.inspector.draw_alpha(&painter, rect, area, pixels));
        for tile in self.tiles.iter().filter(|_| !alpha_drawn) {
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
        if let Some(pixels) = &self.pixels {
            self.inspector.overlay(
                ui,
                &response,
                rect,
                area,
                pixels,
                canvas_input && !self.loading && !self.crop_selecting && self.work.is_none(),
            );
        }
        if self.long_image {
            let label = if self.fit_width {
                "长图 · 适应宽度"
            } else {
                "长图 · 滚轮纵向滚动"
            };
            let state_rect = Rect::from_min_size(
                area.left_top() + Vec2::new(10.0, 10.0),
                Vec2::new(if self.fit_width { 142.0 } else { 174.0 }, 26.0),
            );
            painter.rect_filled(state_rect, 5.0, colors.popover);
            painter.rect_stroke(
                state_rect,
                5.0,
                egui::Stroke::new(1.0, colors.border),
                egui::StrokeKind::Inside,
            );
            painter.text(
                state_rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(13.0),
                colors.text,
            );
            if max_scroll > 0.0 {
                let rail = Rect::from_min_size(
                    area.right_top() + Vec2::new(-32.0, 58.0),
                    Vec2::new(28.0, (area.height() - 110.0).clamp(32.0, 180.0)),
                );
                let response = ui.interact(
                    rail,
                    egui::Id::new("long-image-position"),
                    egui::Sense::click_and_drag(),
                );
                let thumb_height =
                    (rail.height() * area.height() / size.y).clamp(12.0, rail.height());
                let thumb_y =
                    rail.top() + (rail.height() - thumb_height) * self.long_scroll / max_scroll;
                painter.rect_filled(
                    rail.shrink2(Vec2::new(7.0, 0.0)),
                    5.0,
                    Color32::from_black_alpha(38),
                );
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(rail.left() + 5.0, thumb_y),
                        Vec2::new(18.0, thumb_height),
                    ),
                    4.0,
                    colors.accent,
                );
                if canvas_input
                    && (response.clicked() || response.dragged())
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    self.long_scroll = ((pointer.y - rail.top() - thumb_height / 2.0)
                        / (rail.height() - thumb_height).max(1.0))
                    .clamp(0.0, 1.0)
                        * max_scroll;
                }
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, "长图阅读位置")
                });
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
            let stroke = egui::Stroke::new(1.5, colors.accent);
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
        if !self.loading
            && self
                .requested_path
                .as_ref()
                .zip(self.loaded.as_ref())
                .is_some_and(|(requested, loaded)| requested != &loaded.path)
        {
            let warning_rect = Rect::from_min_size(
                area.left_top() + Vec2::splat(12.0),
                Vec2::new((area.width() - 24.0).clamp(160.0, 370.0), 38.0),
            );
            painter.rect_filled(warning_rect, 6.0, colors.error_bg);
            painter.rect_stroke(
                warning_rect,
                6.0,
                egui::Stroke::new(1.0, colors.border),
                egui::StrokeKind::Inside,
            );
            painter.text(
                warning_rect.center(),
                egui::Align2::CENTER_CENTER,
                "打开失败；画面仍是上一张",
                egui::FontId::proportional(15.0),
                colors.error,
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
        if self.loading
            && smoke.workflow_original.is_some()
            && (self.pixels.is_none() || self.tiles.is_empty() || self.canvas_unavailable)
        {
            return Err(ToolError::Invalid(
                "QA navigation lost the previous canvas before replacement".into(),
            ));
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
                    self.clear_edit_history();
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
                    let result = serde_json::json!({"success":true,"native_input_tested":false,"original_unchanged":true,"png_exact_pixels":true,"jpeg_dimensions_alpha_icc":true,"previous_canvas_retained_during_navigation":true,"loader_cache":self.loader.cache_stats(),"records":self.smoke.as_ref().unwrap().workflow_records});
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
        self.ai_clear_dialog(ctx);
        if self.extract_dialog {
            let mut visible = true;
            let mut extract = false;
            egui::Window::new("提取当前帧为静态副本")
                .open(&mut visible)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .max_width((ctx.viewport_rect().width() - 40.0).max(240.0))
                .show(ctx, |ui| {
                    ui.label(
                        "只提取当前合成画面。随后编辑或导出 PNG / JPEG 静态副本，不保留动画。",
                    );
                    ui.small("动画原件保持只读，副本不继承原有效签名。");
                    extract = ui.button("提取当前帧并编辑").clicked();
                });
            self.extract_dialog = visible;
            if extract {
                self.extract_current_frame(ctx);
            }
        }
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
                    ui.label("粘贴或输入 JPEG / PNG / GIF / WebP / BMP / ICO 的本地完整路径");
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
                            let mut selected_format = self.format;
                            ui.horizontal(|ui| {
                                ui.selectable_value(
                                    &mut selected_format,
                                    ExportFormat::Png,
                                    "PNG（保留透明度）",
                                );
                                ui.selectable_value(
                                    &mut selected_format,
                                    ExportFormat::Jpeg,
                                    "JPEG",
                                );
                            });
                            if selected_format != self.format {
                                self.set_export_format(selected_format);
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
            if !visible {
                self.close_export();
                self.pending_intent = None;
                self.pending_export = false;
            }
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
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let appearance = app_theme::preference(self.preferences.appearance);
        if ctx.options(|options| options.theme_preference) != appearance {
            ctx.set_theme(appearance);
        }
        if self.smoke.is_some() {
            let screenshots = ctx.input(|input| {
                input
                    .events
                    .iter()
                    .filter(|event| matches!(event, egui::Event::Screenshot { .. }))
                    .cloned()
                    .collect::<Vec<_>>()
            });
            self.stickers.handle_smoke_screenshots(&screenshots);
        }
        self.poll(ctx);
        self.prepare_feature_smoke(ctx);
        if self.loading
            || self.rename.is_busy()
            || self.directory_scanning
            || self.thumbnail_busy.is_some()
            || self.work.is_some()
            || self.preview_task.is_some()
            || self.evidence_task.is_some()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
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
                        if let egui::Event::Screenshot {
                            image, viewport_id, ..
                        } = e
                            && *viewport_id == egui::ViewportId::ROOT
                        {
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
                    report["appearance"] = serde_json::json!({"preference":self.preferences.appearance,"effective":format!("{:?}",ctx.theme()),"canvas_colors":self.canvas_colors(ctx),"toolbar_island":self.toolbar_layout.as_ref().map(|layout|[layout.island.left(),layout.island.top(),layout.island.width(),layout.island.height()])});
                    report["image_format"] = self
                        .loaded
                        .as_ref()
                        .map(|image| format!("{:?}", image.format))
                        .into();
                    report["displayed_rgba_sha256"] = self
                        .pixels
                        .as_ref()
                        .map(|pixels| {
                            use sha2::Digest;
                            format!("{:x}", sha2::Sha256::digest(pixels.as_raw()))
                        })
                        .into();
                    report["icon"] = self.loaded.as_ref().and_then(|image| image.icon.as_ref()).map(|icon| serde_json::json!({"selected_index":icon.selected_index,"entries":icon.entries.iter().map(|entry| serde_json::json!({"index":entry.index,"width":entry.width,"height":entry.height,"bit_depth":entry.bit_depth,"encoding":entry.encoding.label()})).collect::<Vec<_>>()})).into();
                    report["playback"] = self.playback.as_ref().map(|playback| serde_json::json!({"frame_index":playback.frame_index,"traversal":playback.traversal,"elapsed_ms":playback.elapsed.as_millis(),"frame_delay_ms":playback.frame_delay.as_millis(),"ended":playback.ended,"manual_paused":playback.manual_paused,"suspended":playback.suspended,"warning":playback.warning,"loop_policy":format!("{:?}",playback.source.loops)})).into();
                    report["window_chrome"] = serde_json::json!({"custom_frame":true,"height":chrome_ui::HEIGHT,"pin_hit_size":chrome_ui::BUTTON,"pin_glyph_size":16,"document_name":self.document_name(),"file_path":self.current_file_path(),"rename_open":self.rename.is_open(),"rename_busy":self.rename.is_busy()});
                    report["memory_breakdown"] = self.memory_breakdown();
                    report["viewer_ui"] = serde_json::json!({"toolbar_buttons":self.toolbar_layout.as_ref().map(|layout| layout.buttons.map(|r| [r.min.x,r.min.y,r.max.x,r.max.y])),"more_menu_open":self.more_open,"context_menu_open":egui::Popup::is_any_open(ctx),"single_row_toolbar":true,"reference_on_top":self.reference_on_top,"stickers":self.stickers.diagnostics(),"comparison":{"active":self.comparison.active(),"swapped":self.comparison.swapped,"sync":self.comparison.sync,"reference_name":self.comparison.pinned.as_ref().map(|p| &p.name)},"inspector":self.inspector.diagnostics()});
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
                self.rename.cancel_dialog();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.frames += 1;
        if !self.update_health_sent && self.frames >= 2 {
            self.update_health_probe = light_image_tool::updater::launch::start_ui_health(
                self.settings_warning.is_none(),
                self.frames,
            );
            self.update_health_sent = true;
        }
        self.retired_texture_reservations
            .retain(|(frame, _)| *frame >= self.frames);
        self.retired_displays
            .retain(|(frame, _)| *frame >= self.frames);
        self.update_animation_activity(ui.ctx());
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
        self.update_modal_open = ctx.memory(|m| m.top_modal_layer().is_some());
        let colors = app_theme::palette(&ctx);
        self.updater.poll(
            self.start.elapsed().as_secs(),
            self.work.is_some() || self.ai.is_busy() || self.rename.blocks_document(),
        );
        self.model_updater.poll(
            &self.updater,
            &mut self.ai,
            self.start.elapsed().as_secs(),
            self.work.is_some() || self.rename.blocks_document(),
        );
        if let Some(notice) = self.updater.take_completion_notice() {
            self.status = notice;
        }
        if self.updater.is_busy() || self.model_updater.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        } else if self.updater.is_waiting_for_resources()
            || (self.updater.is_configured()
                && (self.updater.preferences.automatic_app_updates
                    || self.updater.preferences.automatic_installed_model_updates))
        {
            ctx.request_repaint_after(std::time::Duration::from_secs(30));
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.rename.blocks_document() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.status = "请先完成或取消重命名，再关闭窗口".into();
        } else if ctx.input(|i| i.viewport().close_requested()) && !self.unsaved() {
            self.update_normal_exit = true;
        }
        if ctx.input(|i| i.viewport().close_requested())
            && !self.rename.blocks_document()
            && !self.close_approved
            && self.unsaved()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_intent(PendingIntent::Close, &ctx);
        }
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
        if !self.rename.blocks_document() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if egui::Popup::is_any_open(&ctx) {
                egui::Popup::close_all(&ctx);
            } else if self.more_open {
                self.more_open = false;
                ctx.memory_mut(|memory| memory.request_focus(viewer_ui::icon_id(Icon::More)));
            } else if self.error.is_some() {
                self.error = None;
            } else if self.ai_view.confirm.is_some() {
                self.ai_view.confirm = None;
            } else if self.extract_dialog {
                self.extract_dialog = false;
            } else if self.pending_intent.is_some() && !self.pending_export {
                self.pending_intent = None;
            } else if self.path_dialog {
                self.path_dialog = false;
            } else if self.export_dialog {
                self.close_export();
                self.pending_intent = None;
                self.pending_export = false;
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
        if self.info_open {
            self.start_provenance(&ctx);
        }
        let root_keyboard =
            ctx.viewport_id() == egui::ViewportId::ROOT && ctx.input(|input| input.raw.focused);
        let viewer_keyboard = root_keyboard
            && (!ctx.egui_wants_keyboard_input() || viewer_ui::command_has_focus(&ctx));
        if root_keyboard
            && !self.rename.blocks_document()
            && (viewer_keyboard || self.more_open || egui::Popup::is_any_open(&ctx))
            && !self.path_dialog
            && !self.export_dialog
            && self.error.is_none()
            && self.pending_intent.is_none()
            && !self.extract_dialog
            && self.ai_view.confirm.is_none()
            && ctx.input(|i| i.key_pressed(egui::Key::F10))
        {
            egui::Popup::close_all(&ctx);
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
            && !self.rename.blocks_document()
            && !egui::Popup::is_any_open(&ctx)
            && !self.more_open
            && !self.path_dialog
            && !self.export_dialog
            && self.error.is_none()
            && self.pending_intent.is_none()
            && !self.extract_dialog
            && self.ai_view.confirm.is_none()
        {
            if self.playback.is_some()
                && ctx.input(|i| i.key_pressed(egui::Key::Space))
                && !ctx.memory(|m| m.focused().is_some())
            {
                self.toggle_playback();
            }
            if !ctx.text_edit_focused() {
                let paste = ctx.input(|i| {
                    (i.modifiers.command && i.key_pressed(egui::Key::V))
                        || i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))
                });
                let copy = ctx.input(|i| {
                    (i.modifiers.command && i.key_pressed(egui::Key::C))
                        || i.events.iter().any(|e| matches!(e, egui::Event::Copy))
                });
                if paste {
                    self.request_intent(PendingIntent::Clipboard, &ctx);
                } else if copy {
                    self.copy_image(&ctx);
                }
            }
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
                if self.long_image {
                    self.long_scroll += self.canvas_viewport.map_or(200.0, |r| r.height() * 0.9);
                } else {
                    self.navigate(1);
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::PageUp)) {
                if self.long_image {
                    self.long_scroll -= self.canvas_viewport.map_or(200.0, |r| r.height() * 0.9);
                } else {
                    self.navigate(-1);
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Home)) {
                if self.long_image && ctx.input(|i| i.modifiers.ctrl) {
                    self.long_scroll = 0.0;
                } else {
                    self.ai_select_boundary(false);
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::End)) {
                if self.long_image && ctx.input(|i| i.modifiers.ctrl) {
                    self.long_to_bottom = true;
                } else {
                    self.ai_select_boundary(true);
                }
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
                self.fit_window();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Num1)) {
                self.actual_pixels();
            }
            if ctx
                .input(|i| i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::S))
            {
                self.begin_export();
            }
            if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Z)) {
                if ctx.input(|i| i.modifiers.shift) {
                    self.redo_edit(&ctx);
                } else {
                    self.undo_edit(&ctx);
                }
            }
            if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Y)) {
                self.redo_edit(&ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
                self.reload_current();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
                self.toggle_fullscreen(&ctx);
            }
        }
        if self.ai_view.confirm.is_none()
            && let Some(path) =
                ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()))
        {
            self.open(path);
        }
        self.update_preview(&ctx);
        let dialog_open = self.path_dialog
            || self.rename.blocks_document()
            || self.export_dialog
            || self.error.is_some()
            || self.pending_intent.is_some()
            || self.extract_dialog
            || self.ai_view.confirm.is_some();
        let now = ctx.input(|i| i.time);
        let viewport_bottom = ctx.viewport_rect().bottom();
        let interacting = ctx.input(|i| {
            i.pointer.delta() != Vec2::ZERO
                || i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))
        }) || ctx.input(|i| {
            i.pointer
                .hover_pos()
                .is_some_and(|p| p.y >= viewport_bottom - 60.0)
        });
        let pinned = dialog_open
            || self.more_open
            || egui::Popup::is_any_open(&ctx)
            || self.editing
            || self.info_open
            || ctx.memory(|m| m.focused().is_some());
        if interacting || pinned || !self.fullscreen {
            self.fullscreen_activity = now;
        }
        self.toolbar_hidden = self.fullscreen && !pinned && now - self.fullscreen_activity >= 1.8;
        if self.fullscreen && !self.toolbar_hidden && !pinned {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                (1.8 - (now - self.fullscreen_activity)).max(0.01),
            ));
        }
        self.window_chrome(ui);
        let width = ctx.viewport_rect().width();
        let panel_open = self.info_open || self.editing;
        let list_visible = self.preferences.list_visible
            && width >= 640.0
            && !self.siblings.is_empty()
            && (!panel_open || width >= 1100.0);
        if list_visible {
            egui::Panel::left("image-list")
                .default_size(240.0)
                .min_size(200.0)
                .max_size(300.0)
                .resizable(true)
                .frame(
                    egui::Frame::NONE
                        .fill(colors.panel)
                        .inner_margin(egui::Margin::same(12))
                        .stroke(egui::Stroke::new(1.0, colors.separator)),
                )
                .show(ui, |ui| {
                    if dialog_open {
                        ui.disable();
                    }
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                    self.image_list(ui);
                });
        } else {
            self.list_viewport_rect = None;
            self.selected_list_row_rect = None;
            self.thumbnail_worker.cancel();
            self.thumbnail_busy = None;
        }
        let bottom_drawer = panel_open && width < 800.0;
        if bottom_drawer {
            self.bottom_controls(ui);
        }
        if panel_open {
            if width < 800.0 {
                let chrome_height = if self.fullscreen {
                    0.0
                } else {
                    chrome_ui::HEIGHT
                };
                let height =
                    (ctx.viewport_rect().height() - 132.0 - chrome_height).clamp(120.0, 260.0);
                egui::Panel::bottom("inspector-drawer")
                    .exact_size(height)
                    .show(ui, |ui| {
                        if dialog_open {
                            ui.disable();
                        }
                        self.inspector_panel(ui);
                    });
            } else {
                egui::Panel::right("inspector")
                    .default_size(264.0)
                    .min_size(248.0)
                    .max_size(320.0)
                    .show(ui, |ui| {
                        if dialog_open {
                            ui.disable();
                        }
                        self.inspector_panel(ui);
                    });
            }
        }
        if !bottom_drawer {
            self.bottom_controls(ui);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let area = ui.available_rect_before_wrap();
                if !self.comparison_canvas(ui) {
                    self.canvas(ui);
                }
                let current_area = self.canvas_viewport.unwrap_or(area);
                self.ai_canvas_overlay(ui, current_area);
                self.reference_badge(ui, area);
                let load_feedback = self.load_feedback(Instant::now());
                let busy = load_feedback.is_some() || !self.loading && self.work.is_some();
                if busy
                    || !self.loading
                        && self
                            .notice_until
                            .is_some_and(|until| Instant::now() < until)
                {
                    let rect = Rect::from_center_size(
                        area.center_bottom() - Vec2::new(0.0, 25.0),
                        Vec2::new(area.width().min(420.0) - 24.0, 34.0),
                    );
                    ui.painter().rect_filled(rect, 7.0, colors.popover);
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
                    let status = load_feedback.as_deref().unwrap_or(&self.status);
                    ui.put(text_rect, egui::Label::new(status).truncate())
                        .on_hover_text(status);
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
        self.unsaved_dialog(&ctx);
        self.rename_dialog(&ctx);
        self.window_edges(&ctx);
        for tile in self.stickers.show(self.frames) {
            self.retired_texture_reservations
                .push((self.frames + 2, tile._reservation));
            self.retired_displays.push((self.frames + 2, tile._display));
        }
        if let Some(error) = self.stickers.take_error() {
            self.error = Some(error);
        }
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
                && self.thumbnail_busy.is_none()
                && (!smoke.alpha_view
                    || self.inspector.alpha_ready()
                    || !self.inspector.diagnostics()["error"].is_null())
                && smoke.feature_stage == 2;
            let sources_ready = sources_ready
                && (self.stickers.smoke_ready() || self.error.is_some())
                && (!smoke.show_context_menu
                    || smoke.context_menu_started.is_some_and(|started| {
                        started.elapsed() >= std::time::Duration::from_millis(250)
                    }));
            let idle_ready = if smoke.idle_seconds > 0
                && !self.loading
                && sources_ready
                && self.frames >= 2
                && (smoke.workflow_image.is_none() || smoke.workflow_result.is_some())
            {
                if smoke.idle_start.is_none() {
                    smoke.idle_start = Some((Instant::now(), self.frames));
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
                let (started, frame) = smoke.idle_start.unwrap();
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
        if self.ai.is_busy() {
            // Commands can start a scan/import during UI, after this frame's logic ran.
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        // No periodic repaint while idle. Poll AI only while a bounded job is active.
    }
    fn on_exit(&mut self) {
        let conditions = light_image_tool::updater::client::ExitConditions {
            unsaved: self.unsaved() && !self.close_approved,
            exporting: self.work.is_some(),
            modal: self.pending_intent.is_some()
                || self.rename.blocks_document()
                || self.export_dialog
                || self.extract_dialog
                || self.ai_view.confirm.is_some()
                || self.update_modal_open,
            workers_active: false,
            other_instances: false,
        };
        self.model_updater.shutdown();
        self.shutdown_background();
        drop(self.update_health_probe.take());
        if self.update_normal_exit
            && self.update_shutdown_observed
            && !light_image_tool::updater::platform::session_ending()
            && let Err(error) = self.updater.record_exit(
                &conditions,
                light_image_tool::updater::client::ExitReason::NormalUserExit,
            )
        {
            log::info!("退出安装暂缓：{error}");
        }
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
    fn startup_collapses_saved_list_layout_without_rewriting_other_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let saved = settings::Preferences {
            background: CanvasBackground::Dark,
            list_visible: true,
            ..Default::default()
        };
        settings::save(&path, &saved).unwrap();
        let original = std::fs::read(&path).unwrap();
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx, "session-layout".into(), None, None);
        app.settings_path = Some(path.clone());
        app.load_session_preferences();
        assert!(!app.preferences.list_visible);
        assert_eq!(app.preferences.background, CanvasBackground::Dark);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        app.toggle_list();
        assert!(app.preferences.list_visible);
        app.load_session_preferences();
        assert!(!app.preferences.list_visible);
        assert!(settings::load(&path).unwrap().list_visible);
    }

    #[test]
    fn minimal_viewer_defaults_and_toolbar_leave_canvas_dominant() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("minimal.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        configure(&ctx);
        assert!(!app.preferences.list_visible && !app.editing && !app.info_open);
        assert_eq!(app.preferences.background, CanvasBackground::Automatic);
        for size in [
            Vec2::new(800.0, 550.0),
            Vec2::new(1180.0, 780.0),
            Vec2::new(400.0, 275.0),
        ] {
            pointer_ui_size(&mut app, &ctx, vec![], size);
            let layout = app.toolbar_layout.as_ref().unwrap();
            for button in layout
                .buttons
                .iter()
                .zip(layout.visible)
                .filter_map(|(r, visible)| visible.then_some(*r))
            {
                assert!(
                    Rect::from_min_size(Pos2::ZERO, size).contains_rect(button),
                    "screen={size:?}, button={button:?}"
                );
            }
            assert!(
                layout
                    .buttons
                    .iter()
                    .zip(layout.visible)
                    .filter_map(|(button, visible)| visible.then_some(button))
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
    fn toolbar_metadata_tracks_the_visible_image_during_navigation() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let c = other.join("c.png");
        for path in [&a, &b, &c] {
            fixture(path, false);
        }
        let (mut app, ctx) = app_with(&a);
        app.directory_path = Some(browsing::directory(&a));
        app.siblings = vec![a.clone(), b.clone()];
        app.open_now(b.clone());
        assert!(app.loading);
        assert_eq!(app.position, 1);
        assert_eq!(app.loaded.as_ref().unwrap().path, a);
        assert_eq!(app.toolbar_metadata(true), "1 / 2");
        assert!(app.toolbar_metadata(false).starts_with("1 / 2 · 4 × 3 · "));
        wait(&mut app, &ctx);
        assert_eq!(app.loaded.as_ref().unwrap().path, b);
        assert_eq!(app.toolbar_metadata(true), "2 / 2");

        app.open_now(c.clone());
        assert!(app.loading);
        assert_eq!(app.loaded.as_ref().unwrap().path, b);
        assert!(!app.toolbar_metadata(true).contains('/'));
        assert!(!app.toolbar_metadata(false).contains('/'));
        wait(&mut app, &ctx);
        assert_eq!(app.loaded.as_ref().unwrap().path, c);
        assert_eq!(app.toolbar_metadata(true), "1 / 1");
    }
    #[test]
    fn ico_size_menu_switches_in_background_and_exports_the_selected_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("sizes.ico");
        let original = include_bytes!("../assets/gloamere-image.ico");
        std::fs::write(&source, original).unwrap();
        let (mut app, ctx) = app_with(&source);
        configure(&ctx);
        app.info_open = true;
        app.evidence_attempted = true;
        let selected = app
            .loaded
            .as_ref()
            .unwrap()
            .icon
            .as_ref()
            .unwrap()
            .selected_index;
        assert_ne!(selected, 0);
        let size = Vec2::new(1180.0, 780.0);
        pointer_ui_size(&mut app, &ctx, vec![], size);
        let trigger = ctx
            .data(|data| data.get_temp::<Rect>(egui::Id::new("icon-size-trigger")))
            .unwrap();
        click_at(&mut app, &ctx, trigger.center(), size);
        pointer_ui_size(&mut app, &ctx, vec![], size);
        let choice = ctx
            .data(|data| data.get_temp::<Rect>(egui::Id::new(("icon-choice", 0usize))))
            .unwrap();
        let previous = app.pixels.clone().unwrap();
        let generation = app.generation;
        click_at(&mut app, &ctx, choice.center(), size);
        assert!(app.generation > generation);
        if app.loading {
            assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &previous));
        }
        wait(&mut app, &ctx);
        let icon = app.loaded.as_ref().unwrap().icon.as_ref().unwrap();
        assert_eq!(icon.selected_index, 0);
        assert_eq!(
            app.dimensions(),
            [icon.entries[0].width, icon.entries[0].height]
        );
        app.begin_export();
        let output = dir.path().join("selected-size.png");
        app.export_name = output.to_string_lossy().into_owned();
        app.start_export(&ctx);
        wait(&mut app, &ctx);
        assert!(app.error.is_none(), "{:?}", app.error);
        let exported = light_image_tool::pipeline::load(&output, &Cancellation::default()).unwrap();
        assert_eq!(
            exported.pixels.as_raw(),
            app.pixels.as_ref().unwrap().as_raw()
        );
        app.reload_current();
        wait(&mut app, &ctx);
        assert_eq!(
            app.loaded
                .as_ref()
                .unwrap()
                .icon
                .as_ref()
                .unwrap()
                .selected_index,
            0
        );
        assert_eq!(std::fs::read(source).unwrap(), original);
    }
    #[test]
    fn ico_size_switch_preserves_unsaved_guard_and_default_cache_identity() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("guard.ico");
        std::fs::write(&source, include_bytes!("../assets/gloamere-image.ico")).unwrap();
        let (mut app, ctx) = app_with(&source);
        app.open_now(source.clone());
        wait(&mut app, &ctx);
        let default_index = app
            .loaded
            .as_ref()
            .unwrap()
            .icon
            .as_ref()
            .unwrap()
            .selected_index;
        app.start_edit(EditOp::Rotate90, &ctx);
        wait(&mut app, &ctx);
        let edited = app.pixels.clone().unwrap();
        app.select_icon_entry(0);
        assert_eq!(
            app.pending_intent,
            Some(PendingIntent::OpenIconEntry(source.clone(), 0))
        );
        assert!(!app.loading);
        app.resolve_pending_choice(3, &ctx);
        assert!(app.pending_intent.is_none());
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &edited));
        app.select_icon_entry(0);
        app.resolve_pending_choice(2, &ctx);
        wait(&mut app, &ctx);
        assert_eq!(
            app.loaded
                .as_ref()
                .unwrap()
                .icon
                .as_ref()
                .unwrap()
                .selected_index,
            0
        );
        app.open_now(source);
        wait(&mut app, &ctx);
        assert_eq!(
            app.loaded
                .as_ref()
                .unwrap()
                .icon
                .as_ref()
                .unwrap()
                .selected_index,
            default_index
        );
        assert!(!app.edited);
    }
    #[test]
    fn appearance_switch_preserves_pixels_textures_view_and_layout() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("appearance.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        configure(&ctx);
        app.preferences.background = CanvasBackground::CheckerDark;
        app.preferences.list_visible = true;
        app.fit = false;
        app.zoom = 1.7;
        app.pan = Vec2::new(14.0, -9.0);
        let pixels = app.pixels.as_ref().unwrap().clone();
        let texture_ids: Vec<_> = app.tiles.iter().map(|tile| tile.texture.id()).collect();
        let generation = app.generation;
        pointer_ui(&mut app, &ctx, vec![]);
        let geometry = app.toolbar_layout.as_ref().unwrap().buttons;
        for mode in [Appearance::Light, Appearance::Dark, Appearance::System] {
            app.set_appearance(&ctx, mode);
            assert_eq!(
                ctx.options(|options| options.theme_preference),
                app_theme::preference(mode)
            );
            assert!(Arc::ptr_eq(&pixels, app.pixels.as_ref().unwrap()));
            assert_eq!(
                texture_ids,
                app.tiles
                    .iter()
                    .map(|tile| tile.texture.id())
                    .collect::<Vec<_>>()
            );
            assert_eq!(app.generation, generation);
            assert!(!app.loading);
            assert_eq!(app.zoom, 1.7);
            assert_eq!(app.pan, Vec2::new(14.0, -9.0));
            assert_eq!(app.preferences.background, CanvasBackground::CheckerDark);
            assert!(app.preferences.list_visible);
            pointer_ui(&mut app, &ctx, vec![]);
            assert_eq!(app.toolbar_layout.as_ref().unwrap().buttons, geometry);
        }
    }
    #[test]
    fn appearance_submenu_selection_outside_click_and_escape_work() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("appearance-menu.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        configure(&ctx);
        let size = Vec2::new(1180.0, 780.0);
        click_icon(&mut app, &ctx, Icon::More, size);
        let trigger = ctx
            .data(|data| data.get_temp::<Rect>(egui::Id::new("appearance-menu-trigger")))
            .unwrap();
        click_at(&mut app, &ctx, trigger.center(), size);
        pointer_ui(&mut app, &ctx, vec![]);
        assert!(app.more_open && egui::Popup::is_any_open(&ctx));
        let dark = ctx
            .data(|data| data.get_temp::<Rect>(egui::Id::new(("appearance-choice", "深色"))))
            .unwrap();
        click_at(&mut app, &ctx, dark.center(), size);
        assert_eq!(app.preferences.appearance, Appearance::Dark);
        assert!(!app.more_open);
        click_icon(&mut app, &ctx, Icon::More, size);
        let trigger = ctx
            .data(|data| data.get_temp::<Rect>(egui::Id::new("appearance-menu-trigger")))
            .unwrap();
        click_at(&mut app, &ctx, trigger.center(), size);
        pointer_ui(&mut app, &ctx, vec![]);
        assert!(egui::Popup::is_any_open(&ctx));
        click_at(&mut app, &ctx, Pos2::new(12.0, 12.0), size);
        assert!(!app.more_open && !egui::Popup::is_any_open(&ctx));
        click_icon(&mut app, &ctx, Icon::More, size);
        let trigger = ctx
            .data(|data| data.get_temp::<Rect>(egui::Id::new("appearance-menu-trigger")))
            .unwrap();
        click_at(&mut app, &ctx, trigger.center(), size);
        pointer_ui(&mut app, &ctx, vec![]);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(app.more_open && !egui::Popup::is_any_open(&ctx));
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!app.more_open);
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

    #[test]
    fn redo_shortcuts_restore_the_edit_instead_of_undoing_again() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("redo-keys.png");
        fixture(&source, false);
        let original_bytes = std::fs::read(&source).unwrap();
        let (mut app, ctx) = app_with(&source);
        raw_ui(&mut app, &ctx, Some(egui::Key::R), egui::Modifiers::NONE);
        wait(&mut app, &ctx);
        let command = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        raw_ui(&mut app, &ctx, Some(egui::Key::Z), command);
        assert_eq!(app.dimensions(), [4, 3]);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Z),
            egui::Modifiers {
                shift: true,
                ..command
            },
        );
        assert_eq!(app.dimensions(), [3, 4]);
        raw_ui(&mut app, &ctx, Some(egui::Key::Z), command);
        raw_ui(&mut app, &ctx, Some(egui::Key::Y), command);
        assert_eq!(app.dimensions(), [3, 4]);
        assert!(app.undo.len() == 1 && app.redo.is_empty());
        assert_eq!(std::fs::read(&source).unwrap(), original_bytes);
    }

    #[test]
    fn f5_reloads_current_file_after_external_changes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("refresh-keys.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        app.open(source.clone());
        wait(&mut app, &ctx);
        image::RgbaImage::from_pixel(7, 5, image::Rgba([30, 40, 50, 200]))
            .save(&source)
            .unwrap();
        raw_ui(&mut app, &ctx, Some(egui::Key::F5), egui::Modifiers::NONE);
        wait(&mut app, &ctx);
        assert!(app.error.is_none());
        assert_eq!(app.dimensions(), [7, 5]);
        assert_eq!(
            app.pixels.as_ref().unwrap().get_pixel(0, 0).0,
            [30, 40, 50, 200]
        );
        assert_eq!(app.loaded.as_ref().unwrap().path, source);
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
    fn final_directory_row_is_fully_visible_when_panels_can_coexist() {
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
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1180.0, 780.0))),
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
        app.prepare_browser(&old);
        app.thumbnail_busy = Some((0, old.clone()));
        app.accept_thumbnail(
            browsing::Response {
                generation: 0,
                path: old.clone(),
                result: Ok(image::RgbaImage::new(8, 8)),
            },
            &ctx,
        );
        assert_eq!(app.thumbnail_cache.len(), 1);
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
    fn same_directory_navigation_keeps_thumbnails_until_explicit_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.png");
        let second = dir.path().join("second.png");
        fixture(&first, false);
        fixture(&second, false);
        let (mut app, ctx) = app_with(&first);
        app.prepare_browser(&first);
        app.thumbnail_busy = Some((1, first.clone()));
        app.accept_thumbnail(
            browsing::Response {
                generation: 1,
                path: first.clone(),
                result: Ok(image::RgbaImage::new(8, 8)),
            },
            &ctx,
        );
        let texture = app.thumbnail_cache[0].texture.as_ref().unwrap().id();
        app.thumbnail_busy = Some((2, second.clone()));
        app.open(second.clone());
        assert!(app.thumbnail_busy.is_none());
        app.accept_thumbnail(
            browsing::Response {
                generation: 2,
                path: second.clone(),
                result: Ok(image::RgbaImage::new(8, 8)),
            },
            &ctx,
        );
        assert_eq!(app.thumbnail_cache.len(), 1);
        assert_eq!(app.thumbnail_cache[0].path, first);
        assert_eq!(
            app.thumbnail_cache[0].texture.as_ref().unwrap().id(),
            texture
        );
        wait(&mut app, &ctx);
        assert_eq!(app.loaded.as_ref().unwrap().path, second);
        assert_eq!(app.thumbnail_cache.len(), 1);

        app.thumbnail_busy = Some((3, second.clone()));
        app.accept_thumbnail(
            browsing::Response {
                generation: 3,
                path: second.clone(),
                result: Ok(image::RgbaImage::new(8, 8)),
            },
            &ctx,
        );
        assert_eq!(app.thumbnail_cache.len(), 2);
        image::RgbaImage::from_pixel(7, 5, image::Rgba([1, 2, 3, 255]))
            .save(&second)
            .unwrap();
        app.open(second.clone());
        assert_eq!(app.thumbnail_cache.len(), 1);
        assert_eq!(app.thumbnail_cache[0].path, first);
        assert_eq!(
            app.thumbnail_cache[0].texture.as_ref().unwrap().id(),
            texture
        );
        wait(&mut app, &ctx);
        assert_eq!(app.loaded.as_ref().unwrap().path, second);
        assert_eq!(app.dimensions(), [7, 5]);

        let generation = app.directory_generation;
        app.refresh_browser();
        assert!(app.thumbnail_cache.is_empty() && app.thumbnail_busy.is_none());
        assert_eq!(app.directory_generation, generation + 1);
        assert!(app.directory_scanning);
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
    fn failed_cross_directory_open_retains_attempted_selection_and_marks_old_canvas() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        let missing = dir.path().join("other/missing.png");
        app.open(missing.clone());
        wait(&mut app, &ctx);
        assert!(app.error.is_some());
        assert_eq!(app.requested_path.as_ref(), Some(&missing));
        assert_eq!(app.directory_path.as_ref(), Some(&dir.path().join("other")));
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
    fn canvas_secondary_drag_pans_without_opening_context_menu_or_changing_crop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secondary-drag.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        app.refresh_texture(&ctx);
        app.editing = true;
        app.crop_selecting = true;
        app.crop = [1, 1, 2, 2];
        let crop = app.crop;
        pointer_ui(&mut app, &ctx, vec![]);
        let start = app.canvas_viewport.unwrap().center();
        drag(
            &mut app,
            &ctx,
            start,
            start + Vec2::new(40.0, 30.0),
            egui::PointerButton::Secondary,
        );
        assert_eq!(app.pan, Vec2::new(40.0, 30.0));
        assert_eq!(app.crop, crop);
        assert!(!egui::Popup::is_any_open(&ctx));
        assert!(!app.edited && app.work.is_none());
    }
    #[test]
    fn canvas_secondary_click_opens_context_menu_and_escape_keeps_crop_tool() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secondary-click.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        app.refresh_texture(&ctx);
        app.editing = true;
        app.crop_selecting = true;
        app.crop = [1, 1, 2, 2];
        pointer_ui(&mut app, &ctx, vec![]);
        let pos = app.canvas_viewport.unwrap().center();
        for pressed in [true, false] {
            pointer_ui(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Secondary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert!(egui::Popup::is_any_open(&ctx));
        assert_eq!(app.pan, Vec2::ZERO);
        assert_eq!(app.crop, [1, 1, 2, 2]);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!egui::Popup::is_any_open(&ctx));
        assert!(app.editing && app.crop_selecting);
    }
    #[test]
    fn unfocused_main_window_does_not_dispatch_edit_shortcuts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unfocused.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        let mut frame = eframe::Frame::_new_kittest();
        let mut output = ctx.run_ui(
            egui::RawInput {
                focused: false,
                events: vec![egui::Event::Key {
                    key: egui::Key::R,
                    physical_key: Some(egui::Key::R),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| eframe::App::ui(&mut app, ui, &mut frame),
        );
        output.textures_delta.clear();
        assert!(app.work.is_none());
        assert!(!app.edited);
        assert_eq!(app.dimensions(), [4, 3]);
    }
    #[test]
    fn title_name_double_click_opens_rename_and_escape_preserves_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("名称.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        app.refresh_texture(&ctx);
        let pixels = app.pixels.clone().unwrap();
        let generation = app.generation;
        let size = Vec2::new(800.0, 550.0);
        pointer_ui_size(&mut app, &ctx, vec![], size);
        let name = app.title_name_rect.unwrap();
        assert!(name.right() < app.sticker_button_rect.unwrap().left());
        click_at(&mut app, &ctx, name.center(), size);
        click_at(&mut app, &ctx, name.center(), size);
        assert!(app.rename.is_open());
        assert!(!app.rename.is_busy());
        app.open(dir.path().join("other.png"));
        app.start_edit(EditOp::Rotate90, &ctx);
        app.begin_export();
        assert!(!app.loading && app.work.is_none() && !app.export_dialog);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!app.rename.blocks_document());
        assert_eq!(app.generation, generation);
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &pixels));
        assert_eq!(app.loaded.as_ref().unwrap().path, path);
        assert!(path.exists());
    }

    #[test]
    fn rename_dialog_cancels_native_close_and_blocks_navigation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("original.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        app.begin_rename();
        assert!(app.rename.is_open());
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 550.0))),
            focused: true,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let mut frame = eframe::Frame::_new_kittest();
        let mut output = ctx.run_ui(input, |ui| eframe::App::ui(&mut app, ui, &mut frame));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::CancelClose))
        );
        output.textures_delta.clear();
        assert!(!app.update_normal_exit && !app.close_approved);
        assert!(app.pending_intent.is_none());
        app.open_now(dir.path().join("other.png"));
        assert!(!app.loading);
        assert_eq!(app.loaded.as_ref().unwrap().path, path);
    }

    #[test]
    fn small_titlebar_fits_narrow_window_and_edges_send_native_resize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("很长的文件名称用于验证窄窗口不会盖住右上角按钮.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        app.refresh_texture(&ctx);
        let size = Vec2::new(360.0, 400.0);
        pointer_ui_size(&mut app, &ctx, vec![], size);
        let name = app.title_name_rect.unwrap();
        let pin = app.sticker_button_rect.unwrap();
        assert!(name.right() < pin.left());
        assert!(pin.right() < size.x - 3.0 * chrome_ui::BUTTON);
        let at = Pos2::new(1.0, 1.0);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            focused: true,
            events: vec![
                egui::Event::PointerMoved(at),
                egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let mut frame = eframe::Frame::_new_kittest();
        let mut output = ctx.run_ui(input, |ui| eframe::App::ui(&mut app, ui, &mut frame));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|c| matches!(
                    c,
                    egui::ViewportCommand::BeginResize(egui::viewport::ResizeDirection::NorthWest)
                ))
        );
        output.textures_delta.clear();
    }

    #[test]
    fn small_pin_button_creates_snapshot_and_keeps_main_navigation_available() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("pin-first.png");
        let second = dir.path().join("pin-second.png");
        fixture(&first, false);
        image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 200, 255]))
            .save(&second)
            .unwrap();
        let (mut app, ctx) = app_with(&first);
        ctx.set_embed_viewports(false);
        app.refresh_texture(&ctx);
        app.siblings = vec![first, second.clone()];
        app.ai_sync_scope();
        let size = Vec2::new(800.0, 550.0);
        pointer_ui_size(&mut app, &ctx, vec![], size);
        let pin = app.sticker_button_rect.unwrap();
        assert_eq!(pin.size(), Vec2::splat(chrome_ui::BUTTON));
        assert!(pin.bottom() <= chrome_ui::HEIGHT);
        assert!(!app.canvas_viewport.unwrap().intersects(pin));
        click_at(&mut app, &ctx, pin.center(), size);
        assert_eq!(app.stickers.count(), 1);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::ArrowRight),
            egui::Modifiers::NONE,
        );
        wait(&mut app, &ctx);
        assert_eq!(app.loaded.as_ref().unwrap().path, second);
        assert_eq!(app.stickers.count(), 1);
    }
    #[test]
    fn open_context_menu_blocks_canvas_drag_and_f10_switches_to_more() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context-overlay.png");
        image::RgbaImage::from_pixel(600, 400, image::Rgba([30, 50, 70, 200]))
            .save(&path)
            .unwrap();
        let (mut app, ctx) = app_with(&path);
        app.refresh_texture(&ctx);
        app.editing = true;
        app.crop_selecting = true;
        app.crop = [20, 20, 200, 150];
        pointer_ui(&mut app, &ctx, vec![]);
        let at = app.canvas_viewport.unwrap().right_top() + Vec2::new(-100.0, 100.0);
        let open = |app: &mut ImageApp| {
            for pressed in [true, false] {
                pointer_ui(
                    app,
                    &ctx,
                    vec![
                        egui::Event::PointerMoved(at),
                        egui::Event::PointerButton {
                            pos: at,
                            button: egui::PointerButton::Secondary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
            assert!(egui::Popup::is_any_open(&ctx));
        };
        open(&mut app);
        let start = app.canvas_image_rect.unwrap().left_top() + Vec2::splat(40.0);
        drag(
            &mut app,
            &ctx,
            start,
            start + Vec2::new(60.0, 40.0),
            egui::PointerButton::Primary,
        );
        assert_eq!(app.crop, [20, 20, 200, 150]);
        assert_eq!(app.pan, Vec2::ZERO);
        assert!(app.crop_anchor.is_none() && app.work.is_none());
        if !egui::Popup::is_any_open(&ctx) {
            open(&mut app);
        }
        raw_ui(&mut app, &ctx, Some(egui::Key::F10), egui::Modifiers::NONE);
        assert!(app.more_open);
        assert!(!egui::Popup::is_any_open(&ctx));
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
        assert!(rect.min.y > 350.0 && rect.max.y < size.y - 8.0, "{rect:?}");
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
        assert_eq!(app.pending_intent, Some(PendingIntent::Open(b.clone())));
        assert!(app.edited && app.crop_selecting);
        app.resolve_pending_choice(2, &ctx);
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
            thread: None,
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
            thread: None,
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
    fn loading_feedback_waits_for_threshold_and_resets_for_latest_request() {
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.png");
        let second = dir.path().join("second.png");
        fixture(&first, false);
        fixture(&second, false);
        let (mut app, ctx) = app_with(&first);
        app.open(second);
        let started = app.load_started.unwrap();
        assert!(
            app.load_feedback(started + Duration::from_millis(199))
                .is_none()
        );
        assert!(
            app.load_feedback(started + Duration::from_millis(200))
                .unwrap()
                .starts_with("画面仍是上一张")
        );
        app.load_started = Some(started - Duration::from_secs(1));
        let previous = app.load_cancel.clone();
        app.open(first.clone());
        let restarted = app.load_started.unwrap();
        assert!(previous.check().is_err());
        assert!(
            app.load_feedback(restarted + Duration::from_millis(199))
                .is_none()
        );
        wait(&mut app, &ctx);
        assert!(
            app.load_started.is_none()
                && app
                    .load_feedback(restarted + Duration::from_secs(1))
                    .is_none()
        );

        let mut empty = ImageApp::from_context(ctx.clone(), "headless".into(), None, None);
        empty.open(first);
        let started = empty.load_started.unwrap();
        assert!(
            !empty
                .load_feedback(started + Duration::from_millis(200))
                .unwrap()
                .contains("上一张")
        );
        empty.load_cancel.cancel();
        empty.loader.cancel();
        wait(&mut empty, &ctx);
        assert!(empty.load_started.is_none());
    }
    #[test]
    fn loading_feedback_uses_one_bottom_notice_and_failure_stays_immediate() {
        fn render(app: &mut ImageApp, ctx: &egui::Context) -> Vec<(String, Pos2)> {
            fn collect(shape: &egui::Shape, found: &mut Vec<(String, Pos2)>) {
                match shape {
                    egui::Shape::Text(text) => {
                        found.push((text.galley.job.text.clone(), text.pos));
                    }
                    egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, found)),
                    _ => {}
                }
            }
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1180.0, 780.0))),
                ..Default::default()
            };
            let mut frame = eframe::Frame::_new_kittest();
            let mut output = ctx.run_ui(input, |ui| eframe::App::ui(app, ui, &mut frame));
            output.textures_delta.clear();
            let mut found = Vec::new();
            for shape in output.shapes {
                collect(&shape.shape, &mut found);
            }
            found
        }
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.png");
        let second = dir.path().join("second.png");
        fixture(&first, false);
        fixture(&second, false);
        let (mut app, ctx) = app_with(&first);
        configure(&ctx);
        app.refresh_texture(&ctx);
        app.open(second);
        app.load_started = Some(Instant::now() + Duration::from_secs(60));
        app.notice_until = Some(Instant::now() + Duration::from_secs(60));
        let early = render(&mut app, &ctx);
        assert!(
            !early
                .iter()
                .any(|(text, _)| text.contains("画面仍是上一张") || text.contains("正在打开"))
        );

        app.load_started = Some(Instant::now() - Duration::from_secs(1));
        let delayed = render(&mut app, &ctx);
        let notices: Vec<_> = delayed
            .iter()
            .filter(|(text, _)| text.contains("画面仍是上一张"))
            .collect();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].1.y > app.canvas_viewport.unwrap().center().y);

        app.apply_load_response(
            light_image_tool::worker::LoadResponse {
                generation: app.generation,
                result: Err(ToolError::Invalid("fixture failure".into())),
                siblings: vec![],
            },
            &ctx,
        );
        assert!(app.error.is_some() && app.load_started.is_none());
        render(&mut app, &ctx);
        let failed = render(&mut app, &ctx);
        let marker = failed
            .iter()
            .find(|(text, _)| text == "打开失败；画面仍是上一张")
            .unwrap();
        assert!(marker.1.y < app.canvas_viewport.unwrap().center().y);
        assert!(failed.iter().any(|(text, _)| text == "fixture failure"));
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
            thread: None,
        });
        let (_tx, receiver) = mpsc::sync_channel(1);
        app.preview_task = Some(PreviewTask {
            generation: app.generation,
            matte: [255; 3],
            cancel: preview.clone(),
            receiver,
            thread: None,
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
                thread: None,
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
            thread: None,
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
    #[test]
    fn dirty_navigation_preserves_first_intent_and_cancel_keeps_current_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        let c = dir.path().join("c.png");
        for path in [&a, &b, &c] {
            fixture(path, false);
        }
        let (mut app, ctx) = app_with(&a);
        app.siblings = vec![a.clone(), b.clone(), c.clone()];
        app.start_edit(EditOp::Rotate90, &ctx);
        wait(&mut app, &ctx);
        let edited = app.pixels.clone().unwrap();
        app.navigate(1);
        app.select_image(2);
        assert_eq!(app.pending_intent, Some(PendingIntent::Open(b)));
        assert_eq!(app.position, 0);
        assert!(!app.loading);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(app.pending_intent.is_none());
        assert!(app.unsaved());
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &edited));
        assert_eq!(app.loaded.as_ref().unwrap().path, a);
    }
    #[test]
    fn export_then_continue_waits_for_publication_and_failure_abandons_intent() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        fixture(&a, false);
        fixture(&b, false);
        let (mut app, ctx) = app_with(&a);
        app.start_edit(EditOp::Rotate90, &ctx);
        wait(&mut app, &ctx);
        app.open(b.clone());
        app.resolve_pending_choice(1, &ctx);
        assert!(app.pending_export && app.export_dialog);
        assert!(!app.loading);
        app.export_name = a.to_string_lossy().into_owned(); // no overwrite, continuation must stop
        app.start_export(&ctx);
        wait(&mut app, &ctx);
        assert!(app.error.is_some() && app.pending_intent.is_none() && !app.pending_export);
        assert_eq!(app.loaded.as_ref().unwrap().path, a);
        assert!(app.unsaved());
        app.error = None;
        app.open(b.clone());
        app.resolve_pending_choice(1, &ctx);
        let out = dir.path().join("safe-copy.png");
        app.export_name = out.to_string_lossy().into_owned();
        app.start_export(&ctx);
        assert_eq!(app.loaded.as_ref().unwrap().path, a);
        wait(&mut app, &ctx);
        assert!(out.is_file());
        assert_eq!(app.loaded.as_ref().unwrap().path, b);
        assert!(app.pending_intent.is_none() && !app.unsaved());
    }
    #[test]
    fn closing_export_confirmation_never_continues_a_dirty_close() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        fixture(&a, false);
        let (mut app, ctx) = app_with(&a);
        app.start_edit(EditOp::Rotate90, &ctx);
        wait(&mut app, &ctx);
        app.request_intent(PendingIntent::Close, &ctx);
        app.resolve_pending_choice(1, &ctx);
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::Escape),
            egui::Modifiers::NONE,
        );
        assert!(!app.close_approved && !app.export_dialog && !app.pending_export);
        assert!(app.pending_intent.is_none() && app.unsaved());
    }
    #[test]
    fn undo_to_exported_pixels_does_not_warn_again_but_later_edit_does() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        fixture(&a, false);
        let (mut app, ctx) = app_with(&a);
        app.start_edit(EditOp::Rotate90, &ctx);
        wait(&mut app, &ctx);
        app.begin_export();
        app.export_name = dir.path().join("copy.png").to_string_lossy().into_owned();
        app.start_export(&ctx);
        wait(&mut app, &ctx);
        assert!(!app.unsaved());
        assert!(app.edited);
        app.start_edit(EditOp::Rotate90, &ctx);
        wait(&mut app, &ctx);
        assert!(app.unsaved());
        app.undo_edit(&ctx);
        assert!(!app.unsaved());
    }
    #[test]
    fn temporary_session_exports_without_claiming_a_file_source() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "test".into(), None, None);
        let pixels = Arc::new(
            light_image_tool::memory::Pixels::from_image(image::RgbaImage::from_pixel(
                4,
                2,
                image::Rgba([40, 70, 90, 128]),
            ))
            .unwrap(),
        );
        app.source = DocumentSource::Clipboard;
        app.temporary_original = Some(pixels.clone());
        app.pixels = Some(pixels.clone());
        app.edited = true;
        assert!(app.loaded.is_none() && app.unsaved());
        app.begin_export();
        assert!(app.export_dialog);
        let output = dir.path().join("clipboard.png");
        app.export_name = output.to_string_lossy().into_owned();
        app.start_export(&ctx);
        wait(&mut app, &ctx);
        assert!(app.loaded.is_none() && !app.unsaved());
        let copy = light_image_tool::pipeline::load(&output, &Cancellation::default()).unwrap();
        assert_eq!(copy.pixels.as_raw(), pixels.as_raw());
    }
    #[test]
    fn all_owned_background_jobs_join_without_egui_lock_inversion() {
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "test".into(), None, None);
        let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (tx, receiver) = mpsc::sync_channel(1);
        let signal = done.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            signal.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            drop(tx);
        });
        app.preview_task = Some(PreviewTask {
            generation: 0,
            matte: [255; 3],
            cancel: Cancellation::default(),
            receiver,
            thread: Some(thread),
        });
        ctx.memory_mut(|_| app.shutdown_background());
        assert_eq!(done.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(app.shutting_down);
        app.shutdown_background();
    }
    fn animation_app() -> (tempfile::TempDir, ImageApp, egui::Context) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("motion.gif");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut encoder =
                gif::Encoder::new(file, 4, 3, &[255, 0, 0, 0, 255, 0, 0, 0, 255]).unwrap();
            encoder.set_repeat(gif::Repeat::Infinite).unwrap();
            for index in 0..3 {
                encoder
                    .write_frame(&gif::Frame {
                        width: 4,
                        height: 3,
                        delay: 10,
                        buffer: std::borrow::Cow::Owned(vec![index; 12]),
                        ..Default::default()
                    })
                    .unwrap();
            }
        }
        let ctx = egui::Context::default();
        configure(&ctx);
        let mut app = ImageApp::from_context(ctx.clone(), "animation-test".into(), None, None);
        let loaded = light_image_tool::pipeline::load(&path, &Cancellation::default()).unwrap();
        app.apply_load_response(
            light_image_tool::worker::LoadResponse {
                generation: 0,
                result: Ok(loaded),
                siblings: vec![],
            },
            &ctx,
        );
        (dir, app, ctx)
    }
    fn pump_animation(app: &mut ImageApp, ctx: &egui::Context, expected: u32) {
        let start = Instant::now();
        loop {
            pointer_ui(app, ctx, vec![]);
            app.poll(ctx);
            let playback = app.playback.as_ref().unwrap();
            assert!(
                playback
                    .warning
                    .as_ref()
                    .is_none_or(|s| !s.contains("最后正确帧")),
                "{:?}",
                playback.warning
            );
            if playback.frame_index == expected {
                break;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(3));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    #[test]
    fn animation_pause_single_step_restart_and_third_frame_are_bounded() {
        let (_dir, mut app, ctx) = animation_app();
        app.toggle_playback(); // user pause
        for expected in [2, 3, 1, 2, 3] {
            app.step_animation();
            pump_animation(&mut app, &ctx, expected);
            assert!(app.playback.as_ref().unwrap().manual_paused);
            assert!(Arc::ptr_eq(
                &app.loaded.as_ref().unwrap().pixels,
                app.pixels.as_ref().unwrap()
            ));
        }
        let before = app.playback.as_ref().unwrap().frame_index;
        for _ in 0..5 {
            pointer_ui(&mut app, &ctx, vec![]);
            app.poll(&ctx);
        }
        assert_eq!(app.playback.as_ref().unwrap().frame_index, before);
        app.restart_animation();
        pump_animation(&mut app, &ctx, 1);
        assert!(!app.playback.as_ref().unwrap().manual_paused);
    }
    #[test]
    fn animation_modal_suspension_does_not_override_manual_pause() {
        let (_dir, mut app, ctx) = animation_app();
        app.toggle_playback();
        app.more_open = true;
        pointer_ui(&mut app, &ctx, vec![]);
        assert!(app.playback.as_ref().unwrap().suspended);
        app.more_open = false;
        pointer_ui(&mut app, &ctx, vec![]);
        assert!(app.playback.as_ref().unwrap().manual_paused);
        assert!(!app.playback.as_ref().unwrap().clock.is_playing());
    }
    #[test]
    fn animation_requires_explicit_static_extraction_before_edit_or_export() {
        let (_dir, mut app, ctx) = animation_app();
        app.start_edit(EditOp::Rotate90, &ctx);
        assert!(app.extract_dialog && app.work.is_none() && !app.edited);
        app.extract_dialog = false;
        app.begin_export();
        assert!(app.extract_dialog && !app.export_dialog);
        app.extract_current_frame(&ctx);
        assert!(app.playback.is_none() && app.loaded.is_none() && app.unsaved() && app.editing);
        assert!(matches!(
            app.source,
            DocumentSource::ExtractedFrame { frame: 1, .. }
        ));
        app.start_edit(EditOp::Rotate90, &ctx);
        wait(&mut app, &ctx);
        assert_eq!(app.dimensions(), [3, 4]);
    }
    #[test]
    fn animation_texture_ids_are_reused_for_same_canvas_frames() {
        let (_dir, mut app, ctx) = animation_app();
        app.toggle_playback();
        let ids: Vec<_> = app.tiles.iter().map(|tile| tile.texture.id()).collect();
        app.step_animation();
        pump_animation(&mut app, &ctx, 2);
        assert_eq!(
            app.tiles
                .iter()
                .map(|tile| tile.texture.id())
                .collect::<Vec<_>>(),
            ids
        );
    }
    #[test]
    fn same_grid_upload_updates_existing_gpu_texture_without_full_reallocation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        let ids: Vec<_> = app.tiles.iter().map(|t| t.texture.id()).collect();
        let mut initial = ctx.run_ui(egui::RawInput::default(), |_| {});
        initial.textures_delta.clear();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.refresh_texture(ui.ctx());
        });
        let texture_updates = std::mem::take(&mut output.textures_delta.set);
        output.textures_delta.clear();
        for id in ids {
            let deltas = texture_updates.get(&id).unwrap();
            assert!(!deltas.is_empty());
            assert!(deltas.iter().all(|delta| delta.pos == Some([0, 0])));
        }
    }
    #[test]
    fn long_image_reaches_both_ends_and_keeps_page_keys_separate_from_navigation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.png");
        image::RgbaImage::from_pixel(480, 30000, image::Rgba([70, 80, 90, 255]))
            .save(&path)
            .unwrap();
        let (mut app, ctx) = app_with(&path);
        configure(&ctx);
        app.enter_long_image();
        pointer_ui_size(&mut app, &ctx, vec![], Vec2::new(400.0, 600.0));
        assert!(app.long_image && app.fit_width && app.long_scroll == 0.0);
        let origin = app.canvas_image_rect.unwrap();
        raw_ui(
            &mut app,
            &ctx,
            Some(egui::Key::PageDown),
            egui::Modifiers::NONE,
        );
        assert!(app.long_scroll > 0.0 && !app.loading);
        raw_ui(&mut app, &ctx, Some(egui::Key::End), egui::Modifiers::CTRL);
        assert!(
            app.canvas_image_rect.unwrap().bottom() <= app.canvas_viewport.unwrap().bottom() + 1.0
        );
        raw_ui(&mut app, &ctx, Some(egui::Key::Home), egui::Modifiers::CTRL);
        assert_eq!(app.long_scroll, 0.0);
        assert!(origin.top() >= 0.0);
        app.actual_pixels();
        assert!(app.long_image && !app.fit_width && app.zoom == 1.0);
        app.fit_window();
        assert!(!app.long_image && app.fit);
    }
    #[test]
    fn narrow_drawer_keeps_toolbar_and_restores_user_list_preference() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.png");
        fixture(&source, false);
        let (mut app, ctx) = app_with(&source);
        configure(&ctx);
        app.preferences.list_visible = true;
        app.siblings = vec![source];
        app.editing = true;
        pointer_ui_size(&mut app, &ctx, vec![], Vec2::new(400.0, 320.0));
        assert!(app.preferences.list_visible && app.list_viewport_rect.is_none());
        assert!(app.canvas_viewport.unwrap().height() > 70.0);
        assert_eq!(
            app.toolbar_layout
                .as_ref()
                .unwrap()
                .visible
                .iter()
                .filter(|v| **v)
                .count(),
            7
        );
        app.editing = false;
        pointer_ui_size(&mut app, &ctx, vec![], Vec2::new(1180.0, 780.0));
        assert!(app.list_viewport_rect.is_some());
        app.toggle_list();
        app.editing = true;
        pointer_ui_size(&mut app, &ctx, vec![], Vec2::new(400.0, 320.0));
        app.editing = false;
        pointer_ui_size(&mut app, &ctx, vec![], Vec2::new(1180.0, 780.0));
        assert!(!app.preferences.list_visible && app.list_viewport_rect.is_none());
    }
    #[test]
    fn clipboard_paste_during_dirty_session_waits_without_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        app.start_edit(EditOp::Rotate90, &ctx);
        wait(&mut app, &ctx);
        raw_ui(&mut app, &ctx, Some(egui::Key::V), egui::Modifiers::COMMAND);
        assert_eq!(app.pending_intent, Some(PendingIntent::Clipboard));
        assert!(app.work.is_none() && app.loaded.as_ref().unwrap().path == path);
        app.resolve_pending_choice(3, &ctx);
        assert!(app.unsaved());
    }
    #[test]
    fn cancelled_load_has_terminal_ui_state_without_a_worker_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        fixture(&path, false);
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "test".into(), None, None);
        app.open(path);
        app.load_cancel.cancel();
        app.loader.cancel();
        wait(&mut app, &ctx);
        assert!(!app.loading && app.load_started.is_none());
        assert!(app.status.contains("取消"));
    }
    #[test]
    fn finite_animation_eof_waits_for_final_frame_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("finite.gif");
        {
            let mut encoder = gif::Encoder::new(
                std::fs::File::create(&path).unwrap(),
                2,
                1,
                &[255, 0, 0, 0, 255, 0],
            )
            .unwrap();
            for index in 0..2 {
                encoder
                    .write_frame(&gif::Frame {
                        width: 2,
                        height: 1,
                        delay: 500,
                        buffer: std::borrow::Cow::Owned(vec![index; 2]),
                        ..Default::default()
                    })
                    .unwrap();
            }
        }
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "finite".into(), None, None);
        app.apply_load_response(
            light_image_tool::worker::LoadResponse {
                generation: 0,
                result: light_image_tool::pipeline::load(&path, &Cancellation::default()),
                siblings: vec![],
            },
            &ctx,
        );
        app.toggle_playback();
        app.step_animation();
        pump_animation(&mut app, &ctx, 2);
        {
            let p = app.playback.as_mut().unwrap();
            p.manual_paused = false;
            p.clock.resume(Instant::now());
        }
        let start = Instant::now();
        while !app.playback.as_ref().unwrap().eof_pending {
            pointer_ui(&mut app, &ctx, vec![]);
            app.poll(&ctx);
            assert!(start.elapsed() < std::time::Duration::from_secs(2));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!app.playback.as_ref().unwrap().ended);
        assert!(app.playback.as_ref().unwrap().clock.is_playing());
        app.playback
            .as_mut()
            .unwrap()
            .clock
            .resume(Instant::now() - std::time::Duration::from_secs(6));
        app.poll(&ctx);
        assert!(app.playback.as_ref().unwrap().ended);
        assert_eq!(
            app.playback.as_ref().unwrap().elapsed,
            std::time::Duration::from_secs(10)
        );
    }
    #[test]
    fn upload_budget_failure_preserves_previous_document_and_canvas() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        fixture(&a, false);
        fixture(&b, false);
        let (mut app, ctx) = app_with(&a);
        app.refresh_texture(&ctx);
        let ids: Vec<_> = app.tiles.iter().map(|t| t.texture.id()).collect();
        assert!(!app.tiles.is_empty());
        let image = light_image_tool::pipeline::load(&b, &Cancellation::default()).unwrap();
        let available =
            light_image_tool::memory::OPERATION_BUDGET - light_image_tool::memory::in_use();
        let pressure = light_image_tool::memory::reserve(available).unwrap();
        app.apply_load_response(
            light_image_tool::worker::LoadResponse {
                generation: app.generation,
                result: Ok(image),
                siblings: vec![],
            },
            &ctx,
        );
        assert!(!app.canvas_unavailable && app.error.is_some());
        assert_eq!(
            app.tiles.iter().map(|t| t.texture.id()).collect::<Vec<_>>(),
            ids
        );
        assert_eq!(app.loaded.as_ref().unwrap().path, a);
        drop(pressure);
        app.error = None;
        app.open(b.clone());
        wait(&mut app, &ctx);
        assert!(!app.canvas_unavailable && !app.tiles.is_empty());
        assert_eq!(app.loaded.as_ref().unwrap().path, b);
    }
    #[test]
    fn editing_evicts_history_for_prepared_display_and_gpu_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        for _ in 0..2 {
            let history = Arc::new(
                light_image_tool::memory::Pixels::from_image(image::RgbaImage::new(4, 3)).unwrap(),
            );
            history.prepare_display(&Cancellation::default()).unwrap();
            app.undo.push(history);
        }
        let available =
            light_image_tool::memory::OPERATION_BUDGET - light_image_tool::memory::in_use();
        let display_peak = 4 * 3 * 4 * 3 + light_image_tool::memory::OVERHEAD;
        let pressure = light_image_tool::memory::reserve(available - display_peak + 1).unwrap();
        app.start_edit(EditOp::Rotate90, &ctx);
        assert_eq!(app.undo.len(), 1);
        drop(pressure);
        wait(&mut app, &ctx);
        assert!(app.error.is_none() && app.edited && !app.canvas_unavailable);
        assert_eq!(app.dimensions(), [3, 4]);
    }
    #[test]
    fn large_image_navigation_retains_canvas_until_success_and_after_failure() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("large.png");
        let b = dir.path().join("next.png");
        image::RgbaImage::from_pixel(2048, 2048, image::Rgba([20, 80, 40, 255]))
            .save(&a)
            .unwrap();
        fixture(&b, false);
        let (mut app, ctx) = app_with(&a);
        app.fit = false;
        app.zoom = 1.25;
        app.pan = Vec2::new(17.0, 25.0);
        let pixels = app.pixels.as_ref().unwrap().clone();
        let ids: Vec<_> = app.tiles.iter().map(|t| t.texture.id()).collect();
        app.open(b.clone());
        assert!(app.loading && !app.canvas_unavailable);
        assert!(Arc::ptr_eq(app.pixels.as_ref().unwrap(), &pixels));
        assert_eq!(
            app.tiles.iter().map(|t| t.texture.id()).collect::<Vec<_>>(),
            ids
        );
        assert_eq!(app.loaded.as_ref().unwrap().path, a);
        assert_eq!(app.zoom, 1.25);
        assert_eq!(app.pan, Vec2::new(17.0, 25.0));
        wait(&mut app, &ctx);
        assert_eq!(app.loaded.as_ref().unwrap().path, b);
        assert_eq!(app.dimensions(), [4, 3]);
        app.open(dir.path().join("missing.png"));
        let ids: Vec<_> = app.tiles.iter().map(|t| t.texture.id()).collect();
        wait(&mut app, &ctx);
        assert!(app.error.is_some());
        assert_eq!(app.loaded.as_ref().unwrap().path, b);
        assert_eq!(
            app.tiles.iter().map(|t| t.texture.id()).collect::<Vec<_>>(),
            ids
        );
        assert!(!app.canvas_unavailable);
    }
    #[test]
    fn native_close_event_is_cancelled_until_dirty_choice_is_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        fixture(&path, false);
        let (mut app, ctx) = app_with(&path);
        app.start_edit(EditOp::Rotate90, &ctx);
        wait(&mut app, &ctx);
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 550.0))),
            focused: true,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let mut frame = eframe::Frame::_new_kittest();
        let mut output = ctx.run_ui(input, |ui| eframe::App::ui(&mut app, ui, &mut frame));
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::CancelClose))
        );
        output.textures_delta.clear();
        assert_eq!(app.pending_intent, Some(PendingIntent::Close));
        assert!(!app.close_approved);
        app.resolve_pending_choice(3, &ctx);
        assert!(app.unsaved() && !app.close_approved);
        app.request_intent(PendingIntent::Close, &ctx);
        app.resolve_pending_choice(2, &ctx);
        assert!(app.close_approved);
    }
    #[test]
    fn text_edit_copy_and_paste_keep_text_semantics_without_clipboard_image_task() {
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "focus".into(), None, None);
        let mut value = String::new();
        let id = egui::Id::new("external-test-text");
        for events in [
            vec![],
            vec![egui::Event::Paste("plain text".into())],
            vec![egui::Event::Copy],
        ] {
            let mut frame = eframe::Frame::_new_kittest();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 550.0))),
                focused: true,
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                ui.add(egui::TextEdit::singleline(&mut value).id(id))
                    .request_focus();
                eframe::App::ui(&mut app, ui, &mut frame);
            });
            output.textures_delta.clear();
            assert!(app.work.is_none() && app.pending_intent.is_none());
        }
        assert_eq!(value, "plain text");
    }
    #[test]
    fn fullscreen_idle_hides_chrome_and_edge_or_modal_restores_it() {
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "fullscreen".into(), None, None);
        app.fullscreen = true;
        let render = |app: &mut ImageApp, time: f64, events: Vec<egui::Event>| {
            let mut frame = eframe::Frame::_new_kittest();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 320.0))),
                    time: Some(time),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| eframe::App::ui(app, ui, &mut frame),
            );
            output.textures_delta.clear();
        };
        render(&mut app, 0.0, vec![]);
        render(&mut app, 2.0, vec![]);
        assert!(app.toolbar_hidden);
        render(
            &mut app,
            2.1,
            vec![egui::Event::PointerMoved(Pos2::new(10.0, 315.0))],
        );
        assert!(!app.toolbar_hidden);
        app.path_dialog = true;
        render(&mut app, 4.2, vec![egui::Event::PointerGone]);
        assert!(!app.toolbar_hidden);
    }
}

#[cfg(test)]
#[path = "app_keyboard_tests.rs"]
mod keyboard_tests;
