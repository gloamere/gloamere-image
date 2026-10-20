//! Independent, view-only snapshots. Deferred callbacks never own the image resources.
use super::*;
#[path = "app_sticker_region.rs"]
mod native_region;
#[path = "app_sticker_zoom.rs"]
mod smooth_zoom;
use light_image_tool::memory::{self, Reservation};
use std::sync::{
    Mutex, MutexGuard, Weak,
    atomic::{AtomicBool, Ordering},
};

const MAX_STICKERS: usize = 3;
const WINDOW_BYTES: u64 = 16 * 1024 * 1024;

struct Snapshot {
    name: String,
    dimensions: [u32; 2],
    tiles: Vec<Tile>,
    background: CanvasBackground,
    canvas: Rect,
    initial_image: Rect,
    window_title: String,
    _window_reservation: Mutex<Reservation>,
}

#[derive(Clone, Copy)]
struct View {
    fit: bool,
    zoom: f32,
    pan: Vec2,
    on_top: bool,
}
impl Default for View {
    fn default() -> Self {
        Self {
            fit: true,
            zoom: 1.0,
            pan: Vec2::ZERO,
            on_top: true,
        }
    }
}

struct SmokeCapture {
    directory: PathBuf,
    started: Instant,
    requested: bool,
    saved: bool,
    error: Option<String>,
    motion: [MotionCapture; 2],
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct ScreenshotToken(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MotionStage {
    ZoomIn,
    ZoomOut,
}
impl MotionStage {
    fn index(self) -> usize {
        match self {
            Self::ZoomIn => 0,
            Self::ZoomOut => 1,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct MotionScreenshotToken {
    serial: u64,
    stage: MotionStage,
}
#[derive(Default)]
struct MotionCapture {
    requested_on_draw: Option<u64>,
    report: Option<serde_json::Value>,
    saved: bool,
}

#[derive(Default)]
struct Controls {
    view: View,
    draws: u64,
    native: bool,
    pixels_per_point: Option<f32>,
    canvas: Option<Rect>,
    image_rect: Option<Rect>,
    context_menu_open: bool,
    move_requests: u64,
    resize_requests: u64,
    zoom_state: Option<smooth_zoom::Zoom>,
    region: native_region::Region,
    region_updates: u64,
    native_error: Option<String>,
    animation_frames: u64,
    animation_active: bool,
    current_size: Vec2,
    target_size: Vec2,
    current_scale: f64,
    resize_error: Option<String>,
    zoom_smoke: bool,
    zoom_smoke_started: bool,
    zoom_smoke_completed: bool,
    zoom_smoke_goal: Option<f64>,
    zoom_smoke_inputs: u8,
    zoom_smoke_time: Option<f64>,
    zoom_smoke_out_time: Option<f64>,
    zoom_trace: Vec<[f64; 7]>,
    smoke: Option<SmokeCapture>,
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|error| error.into_inner())
}

struct Sticker {
    serial: u64,
    id: egui::ViewportId,
    snapshot: Arc<Snapshot>,
    controls: Arc<Mutex<Controls>>,
    closed: Arc<AtomicBool>,
}

pub(super) struct Stickers {
    ctx: egui::Context,
    stickers: Vec<Sticker>,
    retired: Vec<(u64, Arc<Snapshot>)>,
    next_serial: u64,
    smoke_directory: Option<PathBuf>,
    smoke_error: Option<String>,
    zoom_smoke: bool,
    shutting_down: bool,
    native_error: Option<String>,
}

impl Stickers {
    pub(super) fn new(ctx: egui::Context) -> Self {
        Self {
            ctx,
            stickers: Vec::new(),
            retired: Vec::new(),
            next_serial: 1,
            smoke_directory: None,
            smoke_error: None,
            zoom_smoke: false,
            shutting_down: false,
            native_error: None,
        }
    }

    pub(super) fn push(
        &mut self,
        name: String,
        dimensions: [u32; 2],
        tiles: Vec<Tile>,
        background: CanvasBackground,
    ) -> light_image_tool::Result<()> {
        let result = self.push_inner(name, dimensions, tiles, background);
        if self.smoke_directory.is_some()
            && let Err(error) = &result
        {
            self.smoke_error = Some(error.to_string());
        }
        result
    }

    fn push_inner(
        &mut self,
        name: String,
        dimensions: [u32; 2],
        tiles: Vec<Tile>,
        background: CanvasBackground,
    ) -> light_image_tool::Result<()> {
        if self.shutting_down {
            return Err(ToolError::Cancelled);
        }
        if self.ctx.embed_viewports() {
            return Err(ToolError::Invalid("当前显示后端不支持独立贴图窗口".into()));
        }
        if self.stickers.len() >= MAX_STICKERS {
            return Err(ToolError::ResourceBusy(
                "最多同时保留 3 张贴图，请关闭一张后重试".into(),
            ));
        }
        if dimensions.contains(&0) || tiles.is_empty() {
            return Err(ToolError::Invalid("当前图片没有可固定的显示内容".into()));
        }
        let ppp = self.ctx.pixels_per_point();
        let parent = self.ctx.input(|input| input.viewport().outer_rect);
        let canvas = native_work_area(parent, ppp)
            .unwrap_or_else(|| Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 1024.0)));
        let mut size = initial_size(dimensions);
        size *= (canvas.width() * 0.9 / size.x)
            .min(canvas.height() * 0.9 / size.y)
            .min(1.0);
        let center = parent.map_or(canvas.center(), |rect| rect.center());
        let origin = reachable_position(center - size / 2.0, size, canvas);
        let initial_image = Rect::from_min_size(origin - canvas.min.to_vec2(), size);
        let reservation = memory::reserve(window_bytes_for_size(canvas.size(), ppp))?;
        let serial = self.next_serial;
        self.next_serial = self
            .next_serial
            .checked_add(1)
            .ok_or_else(|| ToolError::Invalid("贴图窗口标识已耗尽".into()))?;
        let id = egui::ViewportId::from_hash_of(("gloamere-sticker", serial));
        self.stickers.push(Sticker {
            serial,
            id,
            snapshot: Arc::new(Snapshot {
                window_title: format!("贴图 · {name} · {serial}"),
                name,
                dimensions,
                tiles,
                background,
                canvas,
                initial_image,
                _window_reservation: Mutex::new(reservation),
            }),
            controls: Arc::new(Mutex::new(Controls {
                zoom_smoke: self.zoom_smoke,
                smoke: self.smoke_directory.as_ref().map(|directory| SmokeCapture {
                    directory: directory.clone(),
                    started: Instant::now(),
                    requested: false,
                    saved: false,
                    error: None,
                    motion: Default::default(),
                }),
                ..Controls::default()
            })),
            closed: Arc::new(AtomicBool::new(false)),
        });
        if self.zoom_smoke {
            self.ctx
                .send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
        }
        self.ctx.request_repaint_of(egui::ViewportId::ROOT);
        Ok(())
    }

    pub(super) fn count(&self) -> usize {
        self.stickers
            .iter()
            .filter(|sticker| !sticker.closed.load(Ordering::Acquire))
            .count()
    }
    pub(super) fn window_bytes(&self) -> u64 {
        self.stickers
            .iter()
            .map(|sticker| &sticker.snapshot)
            .chain(self.retired.iter().map(|(_, snapshot)| snapshot))
            .map(|snapshot| lock(&snapshot._window_reservation).bytes())
            .sum()
    }

    pub(super) fn tiles(&self) -> impl Iterator<Item = &Tile> {
        self.stickers
            .iter()
            .map(|sticker| &sticker.snapshot)
            .chain(self.retired.iter().map(|(_, snapshot)| snapshot))
            .flat_map(|snapshot| snapshot.tiles.iter())
    }

    pub(super) fn shares_texture(&self, id: egui::TextureId) -> bool {
        self.tiles().any(|tile| tile.texture.id() == id)
    }

    /// Root owns returned tiles through its usual renderer handoff. Window reservations
    /// stay here for three root frames, including the pass that removes the viewport.
    pub(super) fn show(&mut self, frame: u64) -> Vec<Tile> {
        self.retired.retain(|(until, _)| *until > frame);
        let mut released = Vec::new();
        let mut index = 0;
        while index < self.stickers.len() {
            if self.stickers[index].closed.load(Ordering::Acquire) {
                let sticker = self.stickers.remove(index);
                if let Some(error) = &lock(&sticker.controls).native_error {
                    self.native_error = Some(error.clone());
                }
                if let Some(smoke) = &lock(&sticker.controls).smoke {
                    if let Some(error) = &smoke.error {
                        self.smoke_error = Some(error.clone());
                    } else if !smoke.saved {
                        self.smoke_error = Some("贴图窗口在截图完成前已关闭".into());
                    }
                }
                released.extend(sticker.snapshot.tiles.iter().cloned());
                self.retired
                    .push((frame.saturating_add(3), sticker.snapshot));
            } else {
                index += 1;
            }
        }
        if !self.retired.is_empty() {
            self.ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
        for sticker in &self.stickers {
            let weak = Arc::downgrade(&sticker.snapshot);
            let controls = sticker.controls.clone();
            let closed = sticker.closed.clone();
            let id = sticker.id;
            let serial = sticker.serial;
            let on_top = lock(&controls).view.on_top;
            self.ctx.show_viewport_deferred(
                id,
                viewport_builder(&sticker.snapshot, on_top),
                move |ui, class| {
                    draw_sticker(ui, class, id, serial, &weak, &controls, &closed);
                },
            );
        }
        released
    }

    pub(super) fn shutdown(&mut self) {
        if self.shutting_down {
            return;
        }
        self.shutting_down = true;
        for sticker in self.stickers.drain(..) {
            sticker.closed.store(true, Ordering::Release);
            if let Some(zoom) = &mut lock(&sticker.controls).zoom_state {
                zoom.close();
            }
            self.ctx
                .send_viewport_cmd_to(sticker.id, egui::ViewportCommand::Close);
        }
        // Only Weak<Snapshot> is captured by egui/eframe, so their live callbacks
        // cannot retain either image owners or native-window reservations.
        self.retired.clear();
    }

    pub(super) fn take_error(&mut self) -> Option<String> {
        self.native_error.take()
    }

    pub(super) fn configure_smoke(&mut self, directory: PathBuf) {
        self.smoke_directory = Some(directory.clone());
        self.smoke_error = None;
        for sticker in &self.stickers {
            lock(&sticker.controls).smoke = Some(SmokeCapture {
                directory: directory.clone(),
                started: Instant::now(),
                requested: false,
                saved: false,
                error: None,
                motion: Default::default(),
            });
            self.ctx.request_repaint_of(sticker.id);
        }
    }

    pub(super) fn configure_zoom_smoke(&mut self, enabled: bool) {
        self.zoom_smoke = enabled;
        for sticker in &self.stickers {
            {
                let mut controls = lock(&sticker.controls);
                controls.zoom_smoke = enabled;
                controls.zoom_smoke_started = false;
                controls.zoom_smoke_completed = false;
                controls.zoom_smoke_goal = None;
                controls.zoom_smoke_inputs = 0;
                controls.zoom_smoke_time = None;
                controls.zoom_smoke_out_time = None;
                if let Some(smoke) = &mut controls.smoke {
                    smoke.motion = Default::default();
                }
            }
            if enabled {
                self.ctx
                    .send_viewport_cmd_to(sticker.id, egui::ViewportCommand::Focus);
            }
            self.ctx.request_repaint_of(sticker.id);
        }
    }

    /// GPU screenshots can arrive in any viewport's next raw input. Root forwards
    /// them here; a child also handles events delivered to its own callback.
    pub(super) fn handle_smoke_screenshots(&self, events: &[egui::Event]) {
        for sticker in &self.stickers {
            if save_smoke_screenshot(sticker.id, sticker.serial, &sticker.controls, events) {
                self.ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
        }
    }

    pub(super) fn smoke_ready(&self) -> bool {
        self.smoke_directory.is_none()
            || self.smoke_error.is_some()
            || self.stickers.iter().any(|sticker| {
                lock(&sticker.controls)
                    .smoke
                    .as_ref()
                    .is_some_and(|smoke| smoke.saved || smoke.error.is_some())
            })
    }

    pub(super) fn diagnostics(&self) -> serde_json::Value {
        let windows: Vec<_> = self
            .stickers
            .iter()
            .map(|sticker| {
                let controls = lock(&sticker.controls);
                let mut window = serde_json::json!({
                    "id": sticker.serial,
                    "viewport_id": format!("{:?}", sticker.id),
                    "name": sticker.snapshot.name,
                    "native_title": sticker.snapshot.window_title,
                    "dimensions": sticker.snapshot.dimensions,
                    "closed": sticker.closed.load(Ordering::Acquire),
                    "on_top": controls.view.on_top,
                    "fit": controls.view.fit,
                    "zoom": controls.view.zoom,
                    "pan": [controls.view.pan.x, controls.view.pan.y],
                    "native": controls.native,
                    "draws": controls.draws,
                    "pixels_per_point": controls.pixels_per_point,
                    "canvas": controls.canvas.map(|rect| [rect.min.x, rect.min.y, rect.max.x, rect.max.y]),
                    "image_rect": controls.image_rect.map(|rect| [rect.min.x, rect.min.y, rect.max.x, rect.max.y]),
                    "borderless": true,
                    "resizable": false,
                    "toolbar": false,
                    "content_only": true,
                    "context_menu_open": controls.context_menu_open,
                    "move_requests": controls.move_requests,
                    "resize_requests": controls.resize_requests,
                    "region_updates": controls.region.update_count(),
                    "region_ready": controls.region.is_ready(),
                    "gpu_zoom": true,
                    "fixed_surface": true,
                    "window_budget_bytes": lock(&sticker.snapshot._window_reservation).bytes(),
                    "resize_current": [controls.current_size.x, controls.current_size.y],
                    "current_scale": controls.current_scale,
                    "resize_target": [controls.target_size.x, controls.target_size.y],
                    "animation_active": controls.animation_active,
                    "animation_frames": controls.animation_frames,
                    "resize_error": controls.resize_error,
                    "zoom_smoke_started": controls.zoom_smoke_started,
                    "zoom_smoke_completed": controls.zoom_smoke_completed,
                    "zoom_smoke_goal_scale": controls.zoom_smoke_goal,
                    "zoom_trace": controls.zoom_trace,
                    "screenshot_requested": controls.smoke.as_ref().is_some_and(|smoke| smoke.requested),
                    "screenshot_ready": controls.smoke.as_ref().is_some_and(|smoke| smoke.saved),
                    "screenshot": controls.smoke.as_ref().filter(|smoke| smoke.saved)
                        .map(|smoke| smoke.directory.join(format!("sticker-{}.png", sticker.serial))),
                    "error": controls.native_error.as_ref().or_else(|| controls.smoke.as_ref().and_then(|smoke| smoke.error.as_ref())),
                });
                window["zoom_smoke_inputs"] = controls.zoom_smoke_inputs.into();
                let [count, total_us, max_us] = controls.region.presentation_waits();
                window["presentation_waits"] = serde_json::json!({"count":count,"total_us":total_us,"max_us":max_us});
                window["motion_screenshots_ready"] = controls
                    .smoke
                    .as_ref()
                    .is_some_and(|smoke| smoke.motion.iter().all(|capture| capture.saved))
                    .into();
                window
            })
            .collect();
        let error = self.smoke_error.clone().or_else(|| {
            windows
                .iter()
                .find_map(|window| window["error"].as_str().map(str::to_owned))
        });
        serde_json::json!({
            "count": self.count(),
            "limit": MAX_STICKERS,
            "retired": self.retired.len(),
            "window_budget_bytes": self.window_bytes(),
            "animation_active": windows.iter().any(|window| window["animation_active"] == true),
            "animation_frames": windows.iter().filter_map(|window| window["animation_frames"].as_u64()).sum::<u64>(),
            "zoom_smoke": self.zoom_smoke,
            "zoom_smoke_ready": !self.zoom_smoke || windows.iter().any(|window| window["zoom_smoke_completed"] == true),
            "smoke_configured": self.smoke_directory.is_some(),
            "smoke_ready": self.smoke_ready(),
            "screenshot_ready": windows.iter().any(|window| window["screenshot_ready"] == true),
            "error": error,
            "windows": windows,
        })
    }
}

impl Drop for Stickers {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn viewport_builder(snapshot: &Snapshot, on_top: bool) -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title(snapshot.window_title.clone())
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false)
        .with_active(false)
        .with_inner_size(snapshot.canvas.size())
        .with_position(snapshot.canvas.min)
        .with_clamp_size_to_monitor_size(true)
        .with_window_level(if on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        })
}

fn initial_size(dimensions: [u32; 2]) -> Vec2 {
    let size = Vec2::new(dimensions[0] as f32, dimensions[1] as f32);
    let scale = (640.0 / size.x)
        .min(480.0 / size.y)
        .min(1.0)
        .max(24.0 / size.x.max(size.y));
    (size * scale).max(Vec2::splat(1.0))
}

fn wheel_points(events: &[egui::Event], page_height: f32) -> f64 {
    events
        .iter()
        .filter_map(|event| {
            if let egui::Event::MouseWheel { unit, delta, .. } = event {
                let unit = match unit {
                    egui::MouseWheelUnit::Point => 1.0,
                    egui::MouseWheelUnit::Line => 40.0,
                    egui::MouseWheelUnit::Page => f64::from(page_height),
                };
                let points = f64::from(delta.y) * unit;
                points.is_finite().then_some(points)
            } else {
                None
            }
        })
        .sum()
}

fn window_bytes_for_size(size: Vec2, ppp: f32) -> u64 {
    // Keep a high-water reservation for three native color buffers. Drivers can
    // allocate more; this remains an application estimate rather than a GPU cap.
    let width = (f64::from(size.x) * f64::from(ppp)).ceil().max(1.0);
    let height = (f64::from(size.y) * f64::from(ppp)).ceil().max(1.0);
    ((width * height * 12.0) as u64).max(WINDOW_BYTES)
}

fn reachable_position(origin: Pos2, size: Vec2, work: Rect) -> Pos2 {
    Pos2::new(
        origin
            .x
            .clamp(work.left(), (work.right() - size.x).max(work.left())),
        origin
            .y
            .clamp(work.top(), (work.bottom() - size.y).max(work.top())),
    )
}

#[cfg(windows)]
fn native_work_area(outer: Option<Rect>, ppp: f32) -> Option<Rect> {
    use windows_sys::Win32::Foundation::{POINT, RECT};
    #[repr(C)]
    struct MonitorInfo {
        size: u32,
        monitor: RECT,
        work: RECT,
        flags: u32,
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        #[link_name = "MonitorFromPoint"]
        fn monitor_from_point(point: POINT, flags: u32) -> *mut std::ffi::c_void;
        #[link_name = "GetMonitorInfoW"]
        fn get_monitor_info(monitor: *mut std::ffi::c_void, info: *mut MonitorInfo) -> i32;
    }
    let center = outer?.center() * ppp;
    let monitor = unsafe {
        monitor_from_point(
            POINT {
                x: center.x.round() as i32,
                y: center.y.round() as i32,
            },
            2,
        )
    };
    if monitor.is_null() {
        return None;
    }
    let zero = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let mut info = MonitorInfo {
        size: std::mem::size_of::<MonitorInfo>() as u32,
        monitor: zero,
        work: zero,
        flags: 0,
    };
    if unsafe { get_monitor_info(monitor, &mut info) } == 0 {
        return None;
    }
    let rect = Rect::from_min_max(
        Pos2::new(info.work.left as f32, info.work.top as f32) / ppp,
        Pos2::new(info.work.right as f32, info.work.bottom as f32) / ppp,
    );
    (rect.is_finite() && rect.is_positive()).then_some(rect)
}

#[cfg(not(windows))]
fn native_work_area(_outer: Option<Rect>, _ppp: f32) -> Option<Rect> {
    None
}

fn close_sticker(
    ctx: &egui::Context,
    id: egui::ViewportId,
    closed: &AtomicBool,
    controls: &Mutex<Controls>,
) {
    closed.store(true, Ordering::Release);
    {
        let mut controls = lock(controls);
        if let Some(zoom) = &mut controls.zoom_state {
            zoom.close();
        }
        controls.animation_active = false;
    }
    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Close);
    ctx.request_repaint_of(egui::ViewportId::ROOT);
}

fn zoom_smoke_wheel(
    start: &mut Option<f64>,
    out_start: &mut Option<f64>,
    inputs: &mut u8,
    now: f64,
    animation_active: bool,
) -> Option<f32> {
    let started = *start.get_or_insert(now);
    let due = if *inputs < 3 {
        started + 0.6 + f64::from(*inputs) * 0.07
    } else if *inputs < 6 {
        if *inputs == 3 && !animation_active {
            out_start.get_or_insert(now + 0.12);
        }
        (*out_start)? + f64::from(*inputs - 3) * 0.07
    } else {
        return None;
    };
    if now < due {
        return None;
    }
    let direction = if *inputs < 3 { 1.0 } else { -1.0 };
    *inputs += 1;
    Some(direction)
}

fn smoke_frame_report(controls: &Controls) -> serde_json::Value {
    let [count, total_us, max_us] = controls.region.presentation_waits();
    serde_json::json!({
        "draw": controls.draws,
        "native": controls.native, "borderless": true, "resizable": false,
        "toolbar": false, "content_only": true,
        "canvas": controls.canvas.map(|rect| [rect.min.x, rect.min.y, rect.max.x, rect.max.y]),
        "image_rect": controls.image_rect.map(|rect| [rect.min.x, rect.min.y, rect.max.x, rect.max.y]),
        "pixels_per_point": controls.pixels_per_point,
        "resize_current_scale": controls.current_scale,
        "resize_target_scale": controls.zoom_state.as_ref().map_or(controls.current_scale, |zoom| zoom.target),
        "animation_active": controls.animation_active, "animation_frames": controls.animation_frames,
        "zoom_smoke_started": controls.zoom_smoke_started,
        "zoom_smoke_completed": controls.zoom_smoke_completed,
        "zoom_smoke_inputs": controls.zoom_smoke_inputs,
        "presentation_waits": {"count":count,"total_us":total_us,"max_us":max_us},
    })
}

fn draw_sticker(
    ui: &mut egui::Ui,
    class: egui::ViewportClass,
    id: egui::ViewportId,
    serial: u64,
    weak: &Weak<Snapshot>,
    controls: &Arc<Mutex<Controls>>,
    closed: &AtomicBool,
) {
    let ctx = ui.ctx().clone();
    let events = ctx.input(|input| input.events.clone());
    if save_smoke_screenshot(id, serial, controls, &events) {
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
    if closed.load(Ordering::Acquire) {
        return;
    }
    if class == egui::ViewportClass::EmbeddedWindow || ctx.viewport_id() != id {
        if let Some(smoke) = &mut lock(controls).smoke {
            smoke.error = Some("当前后端未提供独立贴图原生窗口".into());
        }
        close_sticker(&ctx, id, closed, controls);
        return;
    }
    let Some(snapshot) = weak.upgrade() else {
        close_sticker(&ctx, id, closed, controls);
        return;
    };
    if ctx.input(|input| input.viewport().close_requested())
        || ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        close_sticker(&ctx, id, closed, controls);
        return;
    }
    let area = ui.max_rect();
    let (now, actual, pointer) = ctx.input(|input| {
        (
            input.time,
            input.viewport().inner_rect.unwrap_or_else(|| {
                Rect::from_min_size(
                    input
                        .viewport()
                        .outer_rect
                        .map_or(Pos2::ZERO, |rect| rect.min),
                    area.size(),
                )
            }),
            input.pointer.hover_pos(),
        )
    });
    let ppp = ctx.pixels_per_point();
    let (
        mut view,
        state,
        mut resize_error,
        zoom_smoke,
        mut smoke_time,
        mut smoke_out_time,
        mut smoke_inputs,
        draws,
        completed,
    ) = {
        let mut controls = lock(controls);
        (
            controls.view,
            controls.zoom_state.take(),
            controls.resize_error.clone(),
            controls.zoom_smoke,
            controls.zoom_smoke_time,
            controls.zoom_smoke_out_time,
            controls.zoom_smoke_inputs,
            controls.draws,
            controls.zoom_smoke_completed,
        )
    };
    let Some(mut zoom) = state.or_else(|| {
        let mut size = snapshot.initial_image.size();
        size *= (actual.width() * 0.9 / size.x)
            .min(actual.height() * 0.9 / size.y)
            .min(1.0);
        let origin = reachable_position(
            actual.min + snapshot.initial_image.min.to_vec2(),
            size,
            actual,
        );
        smooth_zoom::Zoom::new(Rect::from_min_size(origin, size), actual)
    }) else {
        lock(controls).native_error = Some("无法建立贴图缩放画布".into());
        close_sticker(&ctx, id, closed, controls);
        return;
    };
    zoom.set_canvas(actual);
    zoom.tick(now);
    let local = |rect: Rect| rect.translate(area.min.to_vec2() - actual.min.to_vec2());
    let response = ui.interact(
        local(zoom.image_world()),
        egui::Id::new(("sticker-canvas", serial)),
        egui::Sense::click_and_drag(),
    );
    let menu_was_open = response.context_menu_opened();
    let was_on_top = view.on_top;
    let mut close = false;
    let mut restore = false;
    let menu = response.context_menu(|ui| {
        ui.label("冻结快照 · 主窗口可继续看图");
        ui.separator();
        if ui.checkbox(&mut view.on_top, "窗口置顶").changed() {
            ui.close();
        }
        if ui.button("恢复初始大小").clicked() {
            restore = true;
            ui.close();
        }
        if let Some(error) = &resize_error {
            ui.small(error);
        }
        if ui.button("关闭这张贴图").clicked() {
            close = true;
            ui.close();
        }
    });
    let menu_open = response.context_menu_opened();
    let move_window =
        !menu_was_open && !menu_open && response.drag_started_by(egui::PointerButton::Primary);
    if move_window || menu_open && !restore {
        zoom.lose_focus(now);
    }
    if restore {
        zoom.reset(now);
    }
    // Windows can deliver wheel events to the hovered inactive window. Focus is
    // deliberately not an admission condition, and leaving the image never snaps a pending zoom.
    if !menu_was_open
        && !menu_open
        && !move_window
        && response.hovered()
        && let Some(pointer) = pointer
    {
        let points = wheel_points(&events, area.height());
        zoom.wheel(
            points,
            pointer + actual.min.to_vec2() - area.min.to_vec2(),
            now,
        );
    }
    if zoom_smoke && !completed && draws >= 1 {
        let active = zoom.tick(now).active;
        if let Some(direction) = zoom_smoke_wheel(
            &mut smoke_time,
            &mut smoke_out_time,
            &mut smoke_inputs,
            now,
            active,
        ) {
            let wheel = egui::Event::MouseWheel {
                phase: egui::TouchPhase::Move,
                unit: egui::MouseWheelUnit::Line,
                delta: Vec2::new(0.0, direction),
                modifiers: egui::Modifiers::NONE,
            };
            let anchor = zoom.tick(now).image_world.center();
            zoom.wheel(wheel_points(&[wheel], area.height()), anchor, now);
        }
    }
    let plan = zoom.tick(now);
    let image_rect = plan
        .image_world
        .translate(area.min.to_vec2() - plan.canvas_world.min.to_vec2());
    view.fit = true;
    view.pan = Vec2::ZERO;
    view.zoom = image_rect.width() * ppp / snapshot.dimensions[0] as f32;
    let painter = ui.painter_at(image_rect.intersect(area));
    let (background, checker) = snapshot.background.colors();
    painter.rect_filled(
        image_rect,
        0.0,
        Color32::from_rgb(background[0], background[1], background[2]),
    );
    if let Some(colors) = checker {
        viewer_ui::paint_checker(&painter, image_rect, colors);
    }
    let scale =
        image_rect.size() / Vec2::new(snapshot.dimensions[0] as f32, snapshot.dimensions[1] as f32);
    for tile in &snapshot.tiles {
        let rect = Rect::from_min_size(
            image_rect.min + Vec2::new(tile.x as f32, tile.y as f32) * scale,
            Vec2::new(tile.w as f32, tile.h as f32) * scale,
        );
        if rect.intersects(area) {
            painter.image(
                tile.texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }
    if move_window {
        ctx.send_viewport_cmd_to(id, egui::ViewportCommand::StartDrag);
    }
    if was_on_top != view.on_top {
        ctx.send_viewport_cmd_to(
            id,
            egui::ViewportCommand::WindowLevel(if view.on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }),
        );
    }
    if plan.closed {
        close = true;
    }
    if plan.repaint
        && let Some(delay) = plan.repaint_after
    {
        ctx.request_repaint_after(delay);
    } else if zoom_smoke && !completed {
        ctx.request_repaint_after(std::time::Duration::from_millis(8));
    }
    let (capture, motion_capture, failed, region_ready) = {
        let mut controls = lock(controls);
        let waits_before = controls.region.presentation_waits()[0];
        match controls.region.update(
            &snapshot.window_title,
            image_rect,
            menu.as_ref().map(|menu| menu.response.rect),
            ppp,
            ctx.cumulative_frame_nr(),
        ) {
            Ok(changed) => controls.region_updates += u64::from(changed),
            Err(error) => {
                let message = format!("贴图窗口区域设置失败：{error}");
                controls.native_error = Some(message.clone());
                resize_error = Some(message);
                close = true;
            }
        }
        let region_ready = cfg!(test) || controls.region.is_ready();
        if controls.region.presentation_waits()[0] != waits_before
            && (plan.active || controls.region.needs_repaint())
        {
            // The native presentation barrier already paced this frame to DWM.
            ctx.request_repaint();
        }
        if controls.region.needs_repaint() {
            ctx.request_repaint_after(std::time::Duration::from_millis(8));
        }
        if !region_ready && draws >= 30 {
            controls.native_error = Some("无法初始化贴图窗口区域，请重试".into());
            close = true;
        }
        controls.view = view;
        controls.native = true;
        controls.draws += 1;
        controls.pixels_per_point = Some(ppp);
        controls.canvas = Some(area);
        controls.image_rect = Some(image_rect);
        controls.context_menu_open = menu_open;
        controls.move_requests += u64::from(move_window);
        controls.animation_frames = plan.animation_frames;
        if zoom_smoke && !completed && controls.zoom_trace.len() < 512 {
            controls.zoom_trace.push([
                now,
                f64::from(image_rect.min.x),
                f64::from(image_rect.min.y),
                f64::from(image_rect.width()),
                f64::from(image_rect.height()),
                f64::from(area.width()),
                f64::from(area.height()),
            ]);
        }
        controls.animation_active = plan.active;
        controls.current_scale = plan.current;
        controls.current_size = plan.image_world.size();
        controls.target_size = plan.target_world.size();
        controls.zoom_smoke_started = smoke_time.is_some();
        controls.zoom_smoke_time = smoke_time;
        controls.zoom_smoke_out_time = smoke_out_time;
        controls.zoom_smoke_inputs = smoke_inputs;
        controls.zoom_smoke_goal = (smoke_inputs == 6).then_some(plan.target);
        controls.zoom_smoke_completed =
            completed || smoke_inputs == 6 && !plan.active && plan.animation_frames > 0;
        controls.resize_error = resize_error;
        controls.zoom_state = Some(zoom);
        let draw = controls.draws;
        // eframe consumes Screenshot on the paint after the command was queued.
        // Freeze that following draw's geometry, rather than the requesting pose.
        if zoom_smoke
            && controls.smoke.as_ref().is_some_and(|smoke| {
                smoke.motion.iter().any(|capture| {
                    capture.report.is_none()
                        && capture
                            .requested_on_draw
                            .is_some_and(|requested| requested < draw)
                })
            })
        {
            let mut report = smoke_frame_report(&controls);
            report["time"] = now.into();
            if let Some(smoke) = &mut controls.smoke {
                for capture in &mut smoke.motion {
                    if capture.report.is_none()
                        && capture
                            .requested_on_draw
                            .is_some_and(|requested| requested < draw)
                    {
                        capture.report = Some(report.clone());
                    }
                }
            }
        }
        let motion_capture =
            if zoom_smoke && region_ready && plan.active && matches!(smoke_inputs, 2 | 3 | 5 | 6) {
                let stage = if smoke_inputs <= 3 {
                    MotionStage::ZoomIn
                } else {
                    MotionStage::ZoomOut
                };
                controls.smoke.as_mut().and_then(|smoke| {
                    let capture = &mut smoke.motion[stage.index()];
                    if smoke.error.is_none() && capture.requested_on_draw.is_none() {
                        capture.requested_on_draw = Some(draw);
                        Some(stage)
                    } else {
                        None
                    }
                })
            } else {
                None
            };
        let motion_ready = !zoom_smoke
            || controls
                .smoke
                .as_ref()
                .is_none_or(|smoke| smoke.motion.iter().all(|capture| capture.saved));
        let ready = region_ready
            && !controls.region.needs_repaint()
            && controls.draws >= 2
            && !plan.active
            && (!zoom_smoke || controls.zoom_smoke_completed)
            && motion_ready;
        let mut failed = false;
        let capture = controls.smoke.as_mut().is_some_and(|smoke| {
            if zoom_smoke
                && !smoke.saved
                && smoke.error.is_none()
                && smoke_time.is_some_and(|start| now - start >= 3.8)
            {
                smoke.error = Some("贴图双向缩放烟测未能在 4 秒内完成截图".into());
                failed = true;
            }
            if !smoke.saved
                && smoke.error.is_none()
                && smoke.started.elapsed() >= std::time::Duration::from_secs(10)
            {
                smoke.error = Some("贴图原生窗口截图等待超过 10 秒".into());
                failed = true;
            }
            if ready && !smoke.requested && smoke.error.is_none() {
                smoke.requested = true;
                true
            } else {
                false
            }
        });
        (capture, motion_capture, failed, region_ready)
    };
    if let Some(stage) = motion_capture {
        ctx.send_viewport_cmd_to(
            id,
            egui::ViewportCommand::Screenshot(egui::UserData::new(MotionScreenshotToken {
                serial,
                stage,
            })),
        );
    }
    if capture {
        ctx.send_viewport_cmd_to(
            id,
            egui::ViewportCommand::Screenshot(egui::UserData::new(ScreenshotToken(serial))),
        );
    }
    if failed {
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
    if !region_ready
        || lock(controls)
            .smoke
            .as_ref()
            .is_some_and(|smoke| !smoke.saved && smoke.error.is_none())
    {
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
    }
    if close {
        close_sticker(&ctx, id, closed, controls);
    }
}
fn save_smoke_screenshot(
    id: egui::ViewportId,
    serial: u64,
    controls: &Mutex<Controls>,
    events: &[egui::Event],
) -> bool {
    let mut handled = false;
    for event in events {
        let egui::Event::Screenshot {
            viewport_id,
            user_data,
            image,
        } = event
        else {
            continue;
        };
        if *viewport_id != id {
            continue;
        }
        let final_capture = user_data
            .data
            .as_ref()
            .and_then(|data| data.downcast_ref::<ScreenshotToken>())
            == Some(&ScreenshotToken(serial));
        let motion_stage = user_data
            .data
            .as_ref()
            .and_then(|data| data.downcast_ref::<MotionScreenshotToken>())
            .filter(|token| token.serial == serial)
            .map(|token| token.stage);
        if !final_capture && motion_stage.is_none() {
            continue;
        }
        let (directory, mut report) = {
            let controls = lock(controls);
            let Some(smoke) = &controls.smoke else {
                continue;
            };
            if smoke.saved || smoke.error.is_some() {
                continue;
            }
            let report = if let Some(stage) = motion_stage {
                if !controls.zoom_smoke || smoke.motion[stage.index()].saved {
                    continue;
                }
                let Some(report) = &smoke.motion[stage.index()].report else {
                    continue;
                };
                report.clone()
            } else {
                smoke_frame_report(&controls)
            };
            (smoke.directory.clone(), report)
        };
        let stem = motion_stage.map_or_else(
            || format!("sticker-{serial}"),
            |stage| format!("sticker-{serial}-motion-{}", stage.label()),
        );
        report["viewport_id"] = format!("{id:?}").into();
        report["id"] = serial.into();
        report["size"] = serde_json::json!(image.size);
        report["screenshot_saved"] = true.into();
        report["ui_input_tested"] = false.into();
        if let Some(stage) = motion_stage {
            report["motion_stage"] = stage.label().into();
        }
        let result = (|| -> Result<(), String> {
            std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            let bytes: Vec<u8> = image
                .pixels
                .iter()
                .flat_map(|pixel| pixel.to_array())
                .collect();
            image::save_buffer(
                directory.join(format!("{stem}.png")),
                &bytes,
                image.size[0] as u32,
                image.size[1] as u32,
                image::ColorType::Rgba8,
            )
            .map_err(|error| error.to_string())?;
            std::fs::write(
                directory.join(format!("{stem}.json")),
                serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            Ok(())
        })();
        if let Some(smoke) = &mut lock(controls).smoke {
            handled = true;
            match result {
                Ok(()) => {
                    if let Some(stage) = motion_stage {
                        smoke.motion[stage.index()].saved = true;
                    } else {
                        smoke.saved = true;
                    }
                }
                Err(error) => smoke.error = Some(format!("贴图窗口截图失败：{error}")),
            }
        }
    }
    handled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(ctx: &egui::Context) -> (Tile, Arc<memory::Budget>) {
        let budget = memory::Budget::new(1024);
        let pixels = memory::Pixels::from_image(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([20, 80, 40, 255]),
        ))
        .unwrap();
        let display = pixels
            .prepare_display(&Cancellation::default())
            .unwrap()
            .clone();
        let tile = Tile {
            _reservation: Arc::new(budget.reserve(16).unwrap()),
            _display: display.clone(),
            texture: ctx.load_texture(
                "sticker-test",
                display.tiles[0].image.clone(),
                egui::TextureOptions::LINEAR,
            ),
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        };
        (tile, budget)
    }
    fn setup() -> (egui::Context, Stickers, Tile, Arc<memory::Budget>) {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let (tile, budget) = tile(&ctx);
        let stickers = Stickers::new(ctx.clone());
        (ctx, stickers, tile, budget)
    }
    fn push(stickers: &mut Stickers, tile: &Tile) {
        stickers
            .push(
                "frozen.png".into(),
                [2, 2],
                vec![tile.clone()],
                CanvasBackground::CheckerLight,
            )
            .unwrap();
        Arc::get_mut(&mut stickers.stickers.last_mut().unwrap().snapshot)
            .unwrap()
            .initial_image = Rect::from_min_size(Pos2::new(40.0, 30.0), Vec2::splat(240.0));
    }
    fn root_pass(ctx: &egui::Context, stickers: &mut Stickers, frame: u64) -> egui::FullOutput {
        ctx.run_ui(egui::RawInput::default(), |_ui| {
            drop(stickers.show(frame));
        })
    }
    fn child_pass(
        ctx: &egui::Context,
        output: &egui::FullOutput,
        id: egui::ViewportId,
        events: Vec<egui::Event>,
        close: bool,
    ) -> egui::FullOutput {
        child_pass_with(
            ctx,
            output,
            id,
            events,
            close,
            Vec2::new(400.0, 300.0),
            true,
        )
    }
    fn child_pass_with(
        ctx: &egui::Context,
        output: &egui::FullOutput,
        id: egui::ViewportId,
        events: Vec<egui::Event>,
        close: bool,
        size: Vec2,
        focused: bool,
    ) -> egui::FullOutput {
        let callback = output.viewport_output[&id].viewport_ui_cb.clone().unwrap();
        let mut input = egui::RawInput {
            viewport_id: id,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            events,
            focused,
            ..Default::default()
        };
        let info = input.viewports.entry(id).or_default();
        info.parent = Some(egui::ViewportId::ROOT);
        if close {
            info.events.push(egui::ViewportEvent::Close);
        }
        ctx.run_ui(input, |ui| callback(ui))
    }

    #[test]
    fn three_stickers_share_texture_owners_without_rebilling_image_bytes() {
        let (_ctx, mut stickers, tile, budget) = setup();
        for _ in 0..MAX_STICKERS {
            push(&mut stickers, &tile);
        }
        assert_eq!(stickers.count(), 3);
        assert_eq!(budget.in_use(), 16);
        assert!(
            stickers
                .tiles()
                .all(|shared| Arc::ptr_eq(&shared._reservation, &tile._reservation))
        );
        assert!(stickers.shares_texture(tile.texture.id()));
        assert!(
            stickers
                .push(
                    "fourth".into(),
                    [2, 2],
                    vec![tile.clone()],
                    CanvasBackground::Light
                )
                .is_err()
        );
        assert_eq!(stickers.count(), 3);
    }

    #[test]
    fn deferred_viewports_are_borderless_on_top_and_keep_small_image_ratio() {
        let (ctx, mut stickers, tile, _) = setup();
        push(&mut stickers, &tile);
        let id = stickers.stickers[0].id;
        let output = root_pass(&ctx, &mut stickers, 1);
        let viewport = &output.viewport_output[&id];
        assert!(viewport.class == egui::ViewportClass::Deferred);
        assert_eq!(viewport.parent, egui::ViewportId::ROOT);
        assert_eq!(viewport.builder.decorations, Some(false));
        assert_eq!(
            viewport.builder.inner_size,
            Some(stickers.stickers[0].snapshot.canvas.size())
        );
        assert_eq!(viewport.builder.transparent, Some(true));
        assert_eq!(viewport.builder.min_inner_size, None);
        assert_eq!(viewport.builder.resizable, Some(false));
        assert_eq!(
            viewport.builder.window_level,
            Some(egui::WindowLevel::AlwaysOnTop)
        );
        assert!(viewport.viewport_ui_cb.is_some());
    }

    #[test]
    fn image_has_no_chrome_and_envelope_background_is_not_painted() {
        let (ctx, mut stickers, tile, _) = setup();
        push(&mut stickers, &tile);
        let id = stickers.stickers[0].id;
        let output = root_pass(&ctx, &mut stickers, 1);
        let output = child_pass_with(&ctx, &output, id, vec![], false, Vec2::splat(360.0), true);
        let controls = lock(&stickers.stickers[0].controls);
        let full = Rect::from_min_size(Pos2::ZERO, Vec2::splat(360.0));
        let image = stickers.stickers[0].snapshot.initial_image;
        assert_eq!(controls.canvas, Some(full));
        assert_eq!(controls.image_rect, Some(image));
        assert!(
            output
                .shapes
                .iter()
                .all(|shape| !matches!(&shape.shape, egui::Shape::Text(_)))
        );
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Mesh(mesh) if mesh.texture_id == tile.texture.id() && mesh.calc_bounds() == image)));
        assert!(output.shapes.iter().all(|shape| match &shape.shape {
            egui::Shape::Rect(rect) => image.contains_rect(rect.rect),
            _ => true,
        }));
    }

    #[test]
    fn reachable_window_bounds_support_negative_monitor_coordinates() {
        let work = Rect::from_min_max(Pos2::new(-1920.0, 0.0), Pos2::new(0.0, 1080.0));
        let size = Vec2::new(300.0, 200.0);
        assert_eq!(
            reachable_position(Pos2::new(-10.0, 1050.0), size, work),
            Pos2::new(-300.0, 880.0)
        );
        assert_eq!(
            reachable_position(Pos2::new(-2200.0, -100.0), size, work),
            work.min
        );
    }

    #[test]
    fn wheel_bursts_move_gpu_rect_without_any_native_resize_commands() {
        let (ctx, mut stickers, tile, _) = setup();
        stickers.configure_zoom_smoke(true);
        push(&mut stickers, &tile);
        let id = stickers.stickers[0].id;
        let root = root_pass(&ctx, &mut stickers, 1);
        let size = Vec2::new(800.0, 600.0);
        let mut widths = Vec::new();
        for _ in 0..150 {
            let output = child_pass_with(&ctx, &root, id, vec![], false, size, false);
            assert!(
                !output.viewport_output[&id]
                    .commands
                    .iter()
                    .any(|command| matches!(
                        command,
                        egui::ViewportCommand::InnerSize(_)
                            | egui::ViewportCommand::OuterPosition(_)
                            | egui::ViewportCommand::Screenshot(_)
                    ))
            );
            let controls = lock(&stickers.stickers[0].controls);
            widths.push(controls.image_rect.unwrap().width());
            if controls.zoom_smoke_completed {
                break;
            }
        }
        let controls = lock(&stickers.stickers[0].controls);
        assert!(controls.zoom_smoke_completed && controls.animation_frames > 3);
        assert_eq!(controls.resize_requests, 0);
        assert_eq!(controls.zoom_smoke_inputs, 6);
        assert!(controls.zoom_trace.last().unwrap()[0] - controls.zoom_smoke_time.unwrap() < 4.0);
        assert!(widths.windows(2).filter(|pair| pair[1] > pair[0]).count() > 5);
        assert!(widths.windows(2).filter(|pair| pair[1] < pair[0]).count() > 5);
        assert!(widths.iter().copied().fold(0.0_f32, f32::max) > widths[0] * 1.06);
        assert!((widths.last().unwrap() - widths[0]).abs() < 0.1);
        let settled = controls.image_rect;
        drop(controls);
        child_pass_with(&ctx, &root, id, vec![], false, size, false);
        assert_eq!(lock(&stickers.stickers[0].controls).image_rect, settled);
    }

    #[test]
    fn zoom_smoke_waits_for_warmup_and_settle_before_reverse_burst() {
        let mut start = None;
        let mut out_start = None;
        let mut inputs = 0;
        let mut wheel =
            |now, active| zoom_smoke_wheel(&mut start, &mut out_start, &mut inputs, now, active);
        assert_eq!(wheel(10.0, false), None);
        assert_eq!(wheel(10.599, false), None);
        assert_eq!(wheel(10.601, false), Some(1.0));
        assert_eq!(wheel(10.671, true), Some(1.0));
        assert_eq!(wheel(10.741, true), Some(1.0));
        assert_eq!(wheel(11.0, true), None);
        assert_eq!(wheel(11.1, false), None);
        assert_eq!(wheel(11.219, false), None);
        assert_eq!(wheel(11.221, false), Some(-1.0));
        assert_eq!(wheel(11.291, true), Some(-1.0));
        assert_eq!(wheel(11.361, true), Some(-1.0));
        assert_eq!(wheel(11.9, false), None);
        assert_eq!(inputs, 6);
    }

    #[test]
    fn hovered_inactive_wheel_is_accepted_and_leaving_does_not_snap_animation() {
        let (ctx, mut stickers, tile, _) = setup();
        push(&mut stickers, &tile);
        let id = stickers.stickers[0].id;
        let root = root_pass(&ctx, &mut stickers, 1);
        let size = Vec2::new(800.0, 600.0);
        child_pass_with(&ctx, &root, id, vec![], false, size, false);
        let original = lock(&stickers.stickers[0].controls).image_rect.unwrap();
        let at = original.center();
        child_pass_with(
            &ctx,
            &root,
            id,
            vec![egui::Event::PointerMoved(at)],
            false,
            size,
            false,
        );
        let output = child_pass_with(
            &ctx,
            &root,
            id,
            vec![egui::Event::MouseWheel {
                phase: egui::TouchPhase::Move,
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0.0, 120.0),
                modifiers: egui::Modifiers::NONE,
            }],
            false,
            size,
            false,
        );
        assert!(lock(&stickers.stickers[0].controls).animation_active);
        assert!(
            !output.viewport_output[&id]
                .commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::InnerSize(_)))
        );
        for _ in 0..80 {
            child_pass_with(
                &ctx,
                &root,
                id,
                vec![egui::Event::PointerMoved(Pos2::new(700.0, 500.0))],
                false,
                size,
                false,
            );
        }
        let controls = lock(&stickers.stickers[0].controls);
        assert!(!controls.animation_active);
        assert!(controls.image_rect.unwrap().width() > original.width() * 1.07);
    }

    #[test]
    fn escape_closes_only_child_while_gpu_zoom_is_active() {
        let (ctx, mut stickers, tile, _) = setup();
        push(&mut stickers, &tile);
        let id = stickers.stickers[0].id;
        let root = root_pass(&ctx, &mut stickers, 1);
        child_pass(&ctx, &root, id, vec![], false);
        {
            let mut controls = lock(&stickers.stickers[0].controls);
            let zoom = controls.zoom_state.as_mut().unwrap();
            zoom.wheel(120.0, zoom.image_world().center(), 1.0);
        }
        let output = child_pass(
            &ctx,
            &root,
            id,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        );
        assert!(stickers.stickers[0].closed.load(Ordering::Acquire));
        assert!(!lock(&stickers.stickers[0].controls).animation_active);
        assert!(
            output.viewport_output[&id]
                .commands
                .contains(&egui::ViewportCommand::Close)
        );
        assert!(
            !output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::Close)
        );
    }

    #[test]
    fn primary_drag_moves_native_window_but_secondary_drag_does_not() {
        for button in [egui::PointerButton::Primary, egui::PointerButton::Secondary] {
            let (ctx, mut stickers, tile, _) = setup();
            push(&mut stickers, &tile);
            let id = stickers.stickers[0].id;
            let output = root_pass(&ctx, &mut stickers, 1);
            child_pass(&ctx, &output, id, vec![], false);
            let start = Pos2::new(100.0, 100.0);
            child_pass(
                &ctx,
                &output,
                id,
                vec![
                    egui::Event::PointerMoved(start),
                    egui::Event::PointerButton {
                        pos: start,
                        button,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                false,
            );
            let moved = child_pass(
                &ctx,
                &output,
                id,
                vec![egui::Event::PointerMoved(Pos2::new(140.0, 140.0))],
                false,
            );
            assert_eq!(
                moved.viewport_output[&id]
                    .commands
                    .contains(&egui::ViewportCommand::StartDrag),
                button == egui::PointerButton::Primary
            );
            assert_eq!(lock(&stickers.stickers[0].controls).view.pan, Vec2::ZERO);
            assert!(
                !moved.viewport_output[&egui::ViewportId::ROOT]
                    .commands
                    .contains(&egui::ViewportCommand::StartDrag)
            );
        }
    }

    #[test]
    fn right_click_opens_close_menu_without_starting_window_move() {
        fn find(shape: &egui::Shape, label: &str) -> Option<Pos2> {
            match shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() / 2.0)
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, label)),
                _ => None,
            }
        }
        let (ctx, mut stickers, tile, _) = setup();
        push(&mut stickers, &tile);
        let id = stickers.stickers[0].id;
        let output = root_pass(&ctx, &mut stickers, 1);
        child_pass(&ctx, &output, id, vec![], false);
        let at = Pos2::new(100.0, 100.0);
        let pointer = |button, pressed, pos| egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        child_pass(
            &ctx,
            &output,
            id,
            vec![
                egui::Event::PointerMoved(at),
                pointer(egui::PointerButton::Secondary, true, at),
            ],
            false,
        );
        child_pass(
            &ctx,
            &output,
            id,
            vec![pointer(egui::PointerButton::Secondary, false, at)],
            false,
        );
        let menu = child_pass(&ctx, &output, id, vec![], false);
        assert!(lock(&stickers.stickers[0].controls).context_menu_open);
        let close = menu
            .shapes
            .iter()
            .find_map(|shape| find(&shape.shape, "关闭这张贴图"))
            .expect("right click should expose sticker close action");
        child_pass(
            &ctx,
            &output,
            id,
            vec![
                egui::Event::PointerMoved(close),
                pointer(egui::PointerButton::Primary, true, close),
            ],
            false,
        );
        let closed = child_pass(
            &ctx,
            &output,
            id,
            vec![pointer(egui::PointerButton::Primary, false, close)],
            false,
        );
        assert_eq!(stickers.count(), 0);
        assert_eq!(lock(&stickers.stickers[0].controls).move_requests, 0);
        assert!(
            closed.viewport_output[&id]
                .commands
                .contains(&egui::ViewportCommand::Close)
        );
        assert!(
            !closed.viewport_output[&id]
                .commands
                .contains(&egui::ViewportCommand::StartDrag)
        );
        assert!(
            !closed.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::Close)
        );
    }

    #[test]
    fn child_close_and_escape_only_close_the_child_then_retire_for_three_root_frames() {
        for native_close in [true, false] {
            let (ctx, mut stickers, tile, _) = setup();
            push(&mut stickers, &tile);
            let id = stickers.stickers[0].id;
            let weak = Arc::downgrade(&stickers.stickers[0].snapshot);
            let output = root_pass(&ctx, &mut stickers, 1);
            let events = if native_close {
                vec![]
            } else {
                vec![
                    egui::Event::PointerMoved(Pos2::new(100.0, 100.0)),
                    egui::Event::PointerButton {
                        pos: Pos2::new(100.0, 100.0),
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            };
            let closed = child_pass(&ctx, &output, id, events, native_close);
            assert_eq!(stickers.count(), 0);
            assert!(
                closed.viewport_output[&id]
                    .commands
                    .contains(&egui::ViewportCommand::Close)
            );
            assert!(
                !closed.viewport_output[&id]
                    .commands
                    .contains(&egui::ViewportCommand::StartDrag)
            );
            assert!(
                !closed.viewport_output[&egui::ViewportId::ROOT]
                    .commands
                    .contains(&egui::ViewportCommand::Close)
            );
            let mut released = Vec::new();
            let removed = ctx.run_ui(egui::RawInput::default(), |_ui| {
                released = stickers.show(2);
            });
            assert_eq!(released.len(), 1);
            assert!(!removed.viewport_output.contains_key(&id));
            assert!(weak.upgrade().is_some());
            root_pass(&ctx, &mut stickers, 4);
            assert!(weak.upgrade().is_some());
            root_pass(&ctx, &mut stickers, 5);
            assert!(weak.upgrade().is_none());
        }
    }

    #[test]
    fn context_retained_callbacks_cannot_keep_snapshots_alive_after_shutdown() {
        let (ctx, mut stickers, tile, budget) = setup();
        push(&mut stickers, &tile);
        let weak = Arc::downgrade(&stickers.stickers[0].snapshot);
        let output = root_pass(&ctx, &mut stickers, 1);
        assert!(
            output
                .viewport_output
                .values()
                .any(|viewport| viewport.viewport_ui_cb.is_some())
        );
        stickers.shutdown();
        assert!(weak.upgrade().is_none());
        assert_eq!(stickers.count(), 0);
        assert!(stickers.tiles().next().is_none());
        drop(tile);
        assert_eq!(budget.in_use(), 0);
        drop(output);
        let _still_live = ctx.run_ui(egui::RawInput::default(), |_ui| {});
    }

    #[test]
    fn frozen_raster_survives_same_grid_main_and_animation_style_uploads() {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let mut app = ImageApp::from_context(ctx.clone(), "sticker-freeze-test".into(), None, None);
        let pixels = |rgba| {
            Arc::new(
                memory::Pixels::from_image(image::RgbaImage::from_pixel(2, 2, image::Rgba(rgba)))
                    .unwrap(),
            )
        };
        let first = pixels([210, 20, 40, 255]);
        app.upload_pixels(&first, &ctx, &Cancellation::default())
            .unwrap();
        app.pixels = Some(first.clone());
        app.stickers
            .push(
                "frozen".into(),
                [2, 2],
                app.tiles.clone(),
                CanvasBackground::CheckerLight,
            )
            .unwrap();
        let frozen_id = app.tiles[0].texture.id();
        let first_weak = Arc::downgrade(&first);
        root_pass(&ctx, &mut app.stickers, 1);

        let second = pixels([20, 40, 210, 255]);
        app.upload_pixels(&second, &ctx, &Cancellation::default())
            .unwrap();
        app.pixels = Some(second);
        drop(first);
        assert!(
            first_weak.upgrade().is_none(),
            "a sticker must not retain raw RGBA"
        );
        let second_id = app.tiles[0].texture.id();
        assert_ne!(second_id, frozen_id);
        let uploaded = root_pass(&ctx, &mut app.stickers, 2);
        assert!(!uploaded.textures_delta.set.contains_key(&frozen_id));

        // Subsequent same-grid frames can reuse B; only the frozen handle is protected.
        let third = pixels([20, 210, 40, 255]);
        app.upload_pixels(&third, &ctx, &Cancellation::default())
            .unwrap();
        app.pixels = Some(third);
        assert_eq!(app.tiles[0].texture.id(), second_id);
        let uploaded = root_pass(&ctx, &mut app.stickers, 3);
        assert!(!uploaded.textures_delta.set.contains_key(&frozen_id));
        let frozen = &app.stickers.stickers[0].snapshot.tiles[0];
        assert_eq!(
            frozen._display.tiles[0].image.pixels[0].to_array(),
            [210, 20, 40, 255]
        );
        assert_eq!(
            app.tiles[0]._display.tiles[0].image.pixels[0].to_array(),
            [20, 210, 40, 255]
        );
    }

    #[test]
    fn child_navigation_keys_do_not_create_root_commands_or_edit_snapshot() {
        let (ctx, mut stickers, tile, _) = setup();
        push(&mut stickers, &tile);
        let id = stickers.stickers[0].id;
        let output = root_pass(&ctx, &mut stickers, 1);
        let events = [
            egui::Key::R,
            egui::Key::ArrowRight,
            egui::Key::F5,
            egui::Key::Z,
        ]
        .into_iter()
        .map(|key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::CTRL,
        })
        .collect();
        let child = child_pass(&ctx, &output, id, events, false);
        assert_eq!(stickers.count(), 1);
        assert_eq!(stickers.stickers[0].snapshot.dimensions, [2, 2]);
        assert_eq!(
            stickers.stickers[0].snapshot.tiles[0].texture.id(),
            tile.texture.id()
        );
        assert!(
            child.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .is_empty()
        );
    }

    #[test]
    fn zoom_smoke_motion_captures_keep_paint_metadata_and_final_readiness_separate() {
        let (ctx, mut stickers, tile, _) = setup();
        let directory = tempfile::tempdir().unwrap();
        stickers.configure_smoke(directory.path().to_path_buf());
        stickers.configure_zoom_smoke(true);
        push(&mut stickers, &tile);
        let id = stickers.stickers[0].id;
        let root = root_pass(&ctx, &mut stickers, 1);
        for _ in 0..180 {
            child_pass_with(
                &ctx,
                &root,
                id,
                vec![],
                false,
                Vec2::new(800.0, 600.0),
                false,
            );
            if lock(&stickers.stickers[0].controls).zoom_smoke_completed {
                break;
            }
        }
        let reports = {
            let controls = lock(&stickers.stickers[0].controls);
            assert!(controls.zoom_smoke_completed);
            assert_eq!(controls.zoom_smoke_inputs, 6);
            let smoke = controls.smoke.as_ref().unwrap();
            assert!(!smoke.saved && !smoke.requested && smoke.error.is_none());
            smoke
                .motion
                .iter()
                .map(|capture| {
                    let report = capture.report.as_ref().unwrap();
                    assert_eq!(
                        report["draw"].as_u64(),
                        capture.requested_on_draw.map(|draw| draw + 1)
                    );
                    assert_eq!(report["animation_active"], true);
                    assert_ne!(
                        report["image_rect"],
                        smoke_frame_report(&controls)["image_rect"]
                    );
                    report.clone()
                })
                .collect::<Vec<_>>()
        };
        let screenshot = Arc::new(egui::ColorImage::new([2, 2], vec![Color32::WHITE; 4]));
        for stage in [MotionStage::ZoomIn, MotionStage::ZoomOut] {
            let event = |viewport_id, serial| egui::Event::Screenshot {
                viewport_id,
                user_data: egui::UserData::new(MotionScreenshotToken { serial, stage }),
                image: screenshot.clone(),
            };
            stickers.handle_smoke_screenshots(&[event(egui::ViewportId::ROOT, 1)]);
            stickers.handle_smoke_screenshots(&[event(id, 2)]);
            let stem = format!("sticker-1-motion-{}", stage.label());
            assert!(!directory.path().join(format!("{stem}.png")).exists());
            stickers.handle_smoke_screenshots(&[event(id, 1)]);
            assert!(directory.path().join(format!("{stem}.png")).exists());
            let report: serde_json::Value = serde_json::from_slice(
                &std::fs::read(directory.path().join(format!("{stem}.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(report["image_rect"], reports[stage.index()]["image_rect"]);
            assert_eq!(
                report["pixels_per_point"],
                reports[stage.index()]["pixels_per_point"]
            );
            assert_eq!(report["motion_stage"], stage.label());
            assert!(!stickers.smoke_ready());
            assert!(!directory.path().join("sticker-1.png").exists());
        }
        child_pass_with(
            &ctx,
            &root,
            id,
            vec![],
            false,
            Vec2::new(800.0, 600.0),
            false,
        );
        assert!(
            lock(&stickers.stickers[0].controls)
                .smoke
                .as_ref()
                .unwrap()
                .requested
        );
        stickers.handle_smoke_screenshots(&[egui::Event::Screenshot {
            viewport_id: id,
            user_data: egui::UserData::new(ScreenshotToken(1)),
            image: screenshot,
        }]);
        assert!(stickers.smoke_ready());
        assert!(directory.path().join("sticker-1.png").exists());
    }

    #[test]
    fn smoke_routes_by_both_viewport_and_typed_userdata() {
        let (ctx, mut stickers, tile, _) = setup();
        let directory = tempfile::tempdir().unwrap();
        assert!(stickers.smoke_ready());
        stickers.configure_smoke(directory.path().to_path_buf());
        push(&mut stickers, &tile);
        assert!(!stickers.smoke_ready());
        let sticker = &stickers.stickers[0];
        let screenshot = Arc::new(egui::ColorImage::new([2, 2], vec![Color32::WHITE; 4]));
        let event = |id, token| egui::Event::Screenshot {
            viewport_id: id,
            user_data: egui::UserData::new(ScreenshotToken(token)),
            image: screenshot.clone(),
        };
        stickers.handle_smoke_screenshots(&[event(egui::ViewportId::ROOT, sticker.serial)]);
        stickers.handle_smoke_screenshots(&[event(sticker.id, sticker.serial + 1)]);
        assert!(!stickers.smoke_ready());
        stickers.handle_smoke_screenshots(&[event(sticker.id, sticker.serial)]);
        assert!(stickers.smoke_ready());
        assert!(directory.path().join("sticker-1.png").exists());
        assert_eq!(stickers.diagnostics()["screenshot_ready"], true);
        let _root = ctx;
    }

    #[test]
    fn smoke_creation_failure_is_ready_with_an_explicit_error() {
        let ctx = egui::Context::default();
        let (tile, _) = tile(&ctx);
        let mut stickers = Stickers::new(ctx);
        stickers.configure_smoke(PathBuf::from("unused-sticker-smoke"));
        assert!(
            stickers
                .push(
                    "embedded".into(),
                    [2, 2],
                    vec![tile],
                    CanvasBackground::Light
                )
                .is_err()
        );
        assert!(stickers.smoke_ready());
        assert!(
            stickers.diagnostics()["error"]
                .as_str()
                .is_some_and(|error| error.contains("独立贴图"))
        );
        assert_eq!(stickers.count(), 0);
    }
}
