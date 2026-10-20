//! View-only inspection. The original pixels remain the input for editing, copying and export.
use super::*;
use light_image_tool::memory::{self, Pixels, PreparedDisplay, Reservation};
use std::{sync::Weak, thread::JoinHandle, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PixelSample {
    pub coordinate: [u32; 2],
    pub rgba: [u8; 4],
}
impl PixelSample {
    pub fn hex(&self) -> String {
        let [r, g, b, a] = self.rgba;
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

fn image_coordinate(rect: Rect, clip: Rect, dimensions: [u32; 2], point: Pos2) -> Option<[u32; 2]> {
    if dimensions.contains(&0)
        || !rect.is_finite()
        || !clip.is_finite()
        || rect.width() <= 0.0
        || rect.height() <= 0.0
        || !point.x.is_finite()
        || !point.y.is_finite()
        || point.x < rect.left().max(clip.left())
        || point.x >= rect.right().min(clip.right())
        || point.y < rect.top().max(clip.top())
        || point.y >= rect.bottom().min(clip.bottom())
    {
        return None;
    }
    Some([
        (((point.x - rect.left()) / rect.width()) * dimensions[0] as f32).floor() as u32,
        (((point.y - rect.top()) / rect.height()) * dimensions[1] as f32).floor() as u32,
    ])
    .filter(|[x, y]| *x < dimensions[0] && *y < dimensions[1])
}

fn sample_pixel(pixels: &Pixels, rect: Rect, clip: Rect, point: Pos2) -> Option<PixelSample> {
    let coordinate = image_coordinate(rect, clip, [pixels.width(), pixels.height()], point)?;
    Some(PixelSample {
        coordinate,
        rgba: pixels.get_pixel(coordinate[0], coordinate[1]).0,
    })
}

fn derive_alpha(source: &Pixels, cancel: &Cancellation) -> light_image_tool::Result<Arc<Pixels>> {
    cancel.check()?;
    if source.width() == 0 || source.height() == 0 {
        return Err(ToolError::Invalid("当前图片没有可查看的像素".into()));
    }
    let bytes = memory::rgba_bytes(source.width(), source.height());
    let mut reservation = memory::reserve(bytes.saturating_add(memory::OVERHEAD))?;
    let mut image = image::RgbaImage::new(source.width(), source.height());
    let stride = source.width() as usize * 4;
    for (src, dst) in source
        .as_raw()
        .chunks_exact(stride)
        .zip(image.as_flat_samples_mut().samples.chunks_exact_mut(stride))
    {
        cancel.check()?;
        for (src, dst) in src
            .as_chunks::<4>()
            .0
            .iter()
            .zip(dst.as_chunks_mut::<4>().0.iter_mut())
        {
            *dst = [src[3], src[3], src[3], 255];
        }
    }
    cancel.check()?;
    let pixels = Arc::new(Pixels::from_reserved(image, &mut reservation)?);
    drop(reservation);
    pixels.prepare_display_with_alpha(cancel, Some(false))?;
    cancel.check()?;
    Ok(pixels)
}

struct AlphaTask {
    generation: u64,
    cancel: Cancellation,
    receiver: mpsc::Receiver<light_image_tool::Result<Arc<Pixels>>>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for AlphaTask {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ColorCopyTask {
    receiver: mpsc::Receiver<Result<String, String>>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for ColorCopyTask {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct AlphaTile {
    texture: egui::TextureHandle,
    _reservation: Reservation,
    offset: [u32; 2],
    size: [u32; 2],
}
struct AlphaView {
    source: Weak<Pixels>,
    _display: Arc<PreparedDisplay>,
    tiles: Vec<AlphaTile>,
}

pub(super) struct Inspector {
    enabled: bool,
    nearest: bool,
    alpha_requested: bool,
    alpha_available: bool,
    source: Weak<Pixels>,
    generation: u64,
    attempted: Option<u64>,
    task: Option<AlphaTask>,
    alpha: Option<AlphaView>,
    retired: Vec<(u64, AlphaView)>,
    upload_reservations: Vec<(u64, Reservation)>,
    frame: u64,
    error: Option<String>,
    copy_task: Option<ColorCopyTask>,
    copy_notice: Option<(String, Instant)>,
}
impl Default for Inspector {
    fn default() -> Self {
        Self {
            enabled: false,
            nearest: false,
            alpha_requested: false,
            alpha_available: true,
            source: Weak::new(),
            generation: 0,
            attempted: None,
            task: None,
            alpha: None,
            retired: Vec::new(),
            upload_reservations: Vec::new(),
            frame: 0,
            error: None,
            copy_task: None,
            copy_notice: None,
        }
    }
}

impl Inspector {
    pub(super) fn configure_smoke(&mut self, enabled: bool, nearest: bool, alpha: bool) {
        self.enabled = enabled;
        self.nearest = nearest;
        self.alpha_requested = enabled && alpha;
        self.invalidate();
    }
    pub(super) fn alpha_ready(&self) -> bool {
        !self.alpha_requested
            || self.enabled
                && self.alpha_available
                && self.error.is_none()
                && self.source.strong_count() > 0
                && self
                    .alpha
                    .as_ref()
                    .is_some_and(|view| view.source.ptr_eq(&self.source))
    }
    pub(super) fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "nearest": self.nearest,
            "alpha_requested": self.alpha_requested,
            "alpha_available": self.alpha_available,
            "alpha_ready": self.alpha_ready(),
            "error": self.error,
        })
    }
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
    pub fn texture_options(&self) -> egui::TextureOptions {
        if self.nearest {
            egui::TextureOptions::NEAREST
        } else {
            egui::TextureOptions::LINEAR
        }
    }
    /// Returns true only when existing normal/comparison textures need their sampler updated.
    pub fn menu(
        &mut self,
        ui: &mut egui::Ui,
        pixels: Option<&Arc<Pixels>>,
        alpha_available: bool,
    ) -> bool {
        self.set_alpha_available(alpha_available);
        self.on_pixels_changed(pixels);
        let mut options_changed = false;
        ui.collapsing("像素与透明度检查", |ui| {
            if ui.checkbox(&mut self.enabled, "开启像素检查器").changed() && !self.enabled {
                self.alpha_requested = false;
                self.invalidate();
            }
            ui.small("显示当前 sRGB / 8 位像素；坐标从 0 开始。悬停查看，单击图片复制 #RRGGBBAA。");
            ui.horizontal(|ui| {
                ui.label("显示采样");
                options_changed |= ui
                    .selectable_value(&mut self.nearest, false, "平滑")
                    .changed();
                options_changed |= ui
                    .selectable_value(&mut self.nearest, true, "锐利 / 像素")
                    .changed();
            });
            let response = ui.add_enabled(
                self.enabled && pixels.is_some() && self.alpha_available,
                egui::Checkbox::new(&mut self.alpha_requested, "查看 Alpha 通道"),
            );
            if response.changed() {
                self.invalidate();
            }
            ui.small("Alpha：黑色为透明，白色为不透明。仅改变查看，不改变原像素、复制图像或导出。");
            if !self.alpha_available {
                ui.small("切图、动画及双图比较期间暂用正常画面；返回静态单图后恢复 Alpha 查看。");
            }
            if let Some(error) = &self.error {
                ui.colored_label(Color32::from_rgb(166, 68, 30), error);
                if self.alpha_requested && ui.button("重试 Alpha 查看").clicked() {
                    self.attempted = None;
                    self.error = None;
                }
            }
        });
        if options_changed && let Some(view) = &mut self.alpha {
            let options = if self.nearest {
                egui::TextureOptions::NEAREST
            } else {
                egui::TextureOptions::LINEAR
            };
            for (tile, prepared) in view.tiles.iter_mut().zip(&view._display.tiles) {
                tile.texture
                    .set_partial([0, 0], prepared.image.clone(), options);
            }
        }
        self.start_if_needed();
        if self.is_busy() {
            ui.ctx().request_repaint_after(Duration::from_millis(8));
        }
        options_changed
    }

    pub fn set_alpha_available(&mut self, available: bool) {
        if self.alpha_available != available {
            self.alpha_available = available;
            self.invalidate();
        }
    }
    pub fn on_pixels_changed(&mut self, pixels: Option<&Arc<Pixels>>) {
        let next = pixels.map(Arc::downgrade).unwrap_or_default();
        let same = self.source.ptr_eq(&next);
        if !same {
            self.invalidate();
            self.source = next;
        }
    }
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.attempted = None;
        self.error = None;
        if let Some(task) = &self.task {
            task.cancel.cancel();
        }
        if let Some(view) = self.alpha.take() {
            self.retired.push((self.frame + 2, view));
        }
    }
    pub fn cancel(&mut self) {
        self.invalidate();
        self.source = Weak::new();
    }
    pub fn shutdown(&mut self) {
        self.alpha_requested = false;
        self.cancel();
        drop(self.task.take());
        drop(self.copy_task.take());
        self.retired.clear();
        self.upload_reservations.clear();
    }
    pub fn is_busy(&self) -> bool {
        self.task.is_some() || self.copy_task.is_some()
    }
    fn start_if_needed(&mut self) {
        if !self.enabled
            || !self.alpha_requested
            || !self.alpha_available
            || self.task.is_some()
            || self.alpha.is_some()
            || self.attempted == Some(self.generation)
        {
            return;
        }
        let Some(source) = self.source.upgrade() else {
            return;
        };
        self.attempted = Some(self.generation);
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        // No egui Context is captured. Cancellation/join never needs the UI's context lock.
        let thread = std::thread::spawn(move || {
            let result = protected(
                || derive_alpha(&source, &token),
                "Alpha 显示准备异常；正常图片仍可查看",
            );
            let _ = sender.try_send(result);
        });
        self.task = Some(AlphaTask {
            generation: self.generation,
            cancel,
            receiver,
            thread: Some(thread),
        });
    }

    pub fn poll(&mut self, ctx: &egui::Context, pixels: Option<&Arc<Pixels>>, frame: u64) {
        self.frame = frame;
        self.retired.retain(|(until, _)| *until >= frame);
        self.upload_reservations
            .retain(|(until, _)| *until >= frame);
        self.on_pixels_changed(pixels);
        let done = self
            .task
            .as_ref()
            .and_then(|task| match task.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(ToolError::Invalid(
                    "Alpha 后台任务已退出；正常图片仍可查看".into(),
                ))),
                Err(mpsc::TryRecvError::Empty) => None,
            });
        if let Some(result) = done {
            let task = self.task.take().unwrap();
            if task.generation == self.generation
                && task.cancel.check().is_ok()
                && self.enabled
                && self.alpha_requested
                && self.alpha_available
            {
                match result.and_then(|derived| self.upload_alpha(ctx, &derived, &task.cancel)) {
                    Ok(view) => self.alpha = Some(view),
                    Err(ToolError::Cancelled) => {}
                    Err(error) => {
                        self.error = Some(format!("Alpha 查看未完成：{error}；继续显示正常图片"))
                    }
                }
            }
            drop(task);
        }
        let copied = self
            .copy_task
            .as_ref()
            .and_then(|task| match task.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("颜色复制任务已退出，请重试".into()))
                }
                Err(mpsc::TryRecvError::Empty) => None,
            });
        if let Some(result) = copied {
            self.copy_notice = Some((
                match result {
                    Ok(hex) => format!("已复制 {hex}"),
                    Err(error) => format!("颜色未复制：{error}"),
                },
                Instant::now() + Duration::from_secs(3),
            ));
            drop(self.copy_task.take());
            ctx.request_repaint();
            ctx.request_repaint_after(Duration::from_secs(3));
        }
        self.start_if_needed();
        if self.is_busy() {
            ctx.request_repaint_after(Duration::from_millis(8));
        }
    }
    fn upload_alpha(
        &mut self,
        ctx: &egui::Context,
        pixels: &Arc<Pixels>,
        cancel: &Cancellation,
    ) -> light_image_tool::Result<AlphaView> {
        let display = pixels.prepare_display(cancel)?.clone();
        let mut reservation = memory::reserve(
            memory::rgba_bytes(pixels.width(), pixels.height()).saturating_add(memory::OVERHEAD),
        )?;
        cancel.check()?;
        let mut tiles = Vec::with_capacity(display.tiles.len());
        for prepared in &display.tiles {
            let [w, h] = prepared.image.size;
            tiles.push(AlphaTile {
                texture: ctx.load_texture(
                    format!("alpha-{}-{}-{}", self.generation, prepared.x, prepared.y),
                    prepared.image.clone(),
                    self.texture_options(),
                ),
                _reservation: reservation.split((w * h * 4) as u64)?,
                offset: [prepared.x, prepared.y],
                size: [w as u32, h as u32],
            });
        }
        self.upload_reservations.push((self.frame + 2, reservation));
        Ok(AlphaView {
            source: self.source.clone(),
            _display: display,
            tiles,
        })
    }

    pub fn draw_alpha(
        &self,
        painter: &egui::Painter,
        image_rect: Rect,
        clip: Rect,
        pixels: &Arc<Pixels>,
    ) -> bool {
        if !self.enabled || !self.alpha_requested || !self.alpha_available {
            return false;
        }
        let Some(view) = self
            .alpha
            .as_ref()
            .filter(|view| view.source.ptr_eq(&Arc::downgrade(pixels)))
        else {
            return false;
        };
        let scale = Vec2::new(
            image_rect.width() / pixels.width() as f32,
            image_rect.height() / pixels.height() as f32,
        );
        let painter = painter.with_clip_rect(clip);
        for tile in &view.tiles {
            let rect = Rect::from_min_size(
                image_rect.min
                    + Vec2::new(
                        tile.offset[0] as f32 * scale.x,
                        tile.offset[1] as f32 * scale.y,
                    ),
                Vec2::new(tile.size[0] as f32 * scale.x, tile.size[1] as f32 * scale.y),
            );
            if rect.intersects(clip) {
                painter.image(
                    tile.texture.id(),
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }
        true
    }

    /// Call only for the current/B canvas. Pointer coordinates are already logical egui points.
    pub fn overlay(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        image_rect: Rect,
        clip: Rect,
        pixels: &Arc<Pixels>,
        sampling_available: bool,
    ) -> bool {
        if !self.enabled {
            return false;
        }
        let sample = if sampling_available && response.hovered() {
            ui.input(|input| input.pointer.hover_pos())
                .and_then(|point| sample_pixel(pixels, image_rect, clip, point))
        } else {
            None
        };
        let copied = sample.as_ref().is_some_and(|sample| {
            if response.clicked() && self.copy_task.is_none() {
                let hex = sample.hex();
                let (sender, receiver) = mpsc::sync_channel(1);
                let thread = std::thread::spawn(move || {
                    let result = light_image_tool::clipboard::ClipboardAdapter::new()
                        .copy_text(&hex)
                        .map(|()| hex)
                        .map_err(|error| error.to_string());
                    let _ = sender.try_send(result);
                });
                self.copy_task = Some(ColorCopyTask {
                    receiver,
                    thread: Some(thread),
                });
                ui.ctx().request_repaint_after(Duration::from_millis(8));
                true
            } else {
                false
            }
        });
        let mut lines = Vec::new();
        if let Some(sample) = sample {
            lines.push(format!(
                "({}, {})  RGBA {:?}  {}",
                sample.coordinate[0],
                sample.coordinate[1],
                sample.rgba,
                sample.hex()
            ));
        } else {
            lines.push("像素检查 · 在当前图片内悬停取色".into());
        }
        lines.push("当前 sRGB / 8 位像素 · 单击复制颜色".into());
        if self.alpha_requested {
            if !self.alpha_available {
                lines.push("Alpha 暂停 · 当前显示正常图片".into());
            } else if let Some(error) = &self.error {
                lines.push(error.clone());
            } else if !self
                .alpha
                .as_ref()
                .is_some_and(|view| view.source.ptr_eq(&Arc::downgrade(pixels)))
            {
                lines.push("正在后台准备 Alpha · 当前仍是正常图片".into());
            } else {
                lines.push("Alpha 通道 · 黑色透明，白色不透明".into());
            }
        }
        if self.copy_task.is_some() {
            lines.push("正在复制颜色…".into());
        }
        if let Some((notice, until)) = &self.copy_notice
            && *until > Instant::now()
        {
            lines.push(notice.clone());
        }
        let painter = ui.painter().with_clip_rect(clip);
        let width = (clip.width() - 16.0).clamp(1.0, 510.0);
        let galleys: Vec<_> = lines
            .into_iter()
            .map(|line| {
                painter.layout(
                    line,
                    egui::FontId::proportional(12.0),
                    Color32::from_rgb(35, 54, 47),
                    (width - 12.0).max(1.0),
                )
            })
            .collect();
        let height = galleys
            .iter()
            .map(|galley| galley.size().y + 3.0)
            .sum::<f32>()
            + 12.0;
        let rect = Rect::from_min_size(
            Pos2::new(
                clip.left() + 8.0,
                (clip.bottom() - height - 8.0).max(clip.top()),
            ),
            Vec2::new(width, height),
        );
        painter.rect_filled(
            rect,
            5.0,
            Color32::from_rgba_unmultiplied(245, 248, 247, 242),
        );
        let mut y = 6.0;
        for galley in galleys {
            let height = galley.size().y;
            painter.galley(
                rect.min + Vec2::new(6.0, y),
                galley,
                Color32::from_rgb(35, 54, 47),
            );
            y += height + 3.0;
        }
        copied
    }
}
impl Drop for Inspector {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source(color: [u8; 4]) -> Arc<Pixels> {
        Arc::new(
            Pixels::from_image(image::RgbaImage::from_pixel(9, 7, image::Rgba(color))).unwrap(),
        )
    }
    #[test]
    fn inspector_coordinates_respect_zoom_pan_clip_and_dpi() {
        for ppp in [1.0, 1.25, 1.5, 2.0] {
            for zoom in [0.25, 1.0, 3.0] {
                let rect = Rect::from_min_size(
                    Pos2::new(-13.0, 17.0),
                    Vec2::new(400.0, 200.0) * (zoom / ppp),
                );
                let point = rect.min + rect.size() * Vec2::new(0.501, 0.251);
                assert_eq!(
                    image_coordinate(rect, rect, [400, 200], point),
                    Some([200, 50])
                );
                assert_eq!(image_coordinate(rect, rect, [400, 200], rect.max), None);
                assert_eq!(
                    image_coordinate(rect, rect, [400, 200], rect.min - Vec2::splat(0.01)),
                    None
                );
                let clipped = Rect::from_min_max(rect.center(), rect.max);
                assert_eq!(image_coordinate(rect, clipped, [400, 200], point), None);
            }
        }
    }
    #[test]
    fn inspector_samples_uncomposited_rgba_including_hidden_rgb() {
        let pixels = source([201, 90, 7, 0]);
        let rect = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(90.0, 70.0));
        let sample = sample_pixel(&pixels, rect, rect, Pos2::new(25.0, 35.0)).unwrap();
        assert_eq!(sample.coordinate, [1, 2]);
        assert_eq!(sample.rgba, [201, 90, 7, 0]);
        assert_eq!(sample.hex(), "#C95A0700");
        assert!(sample_pixel(&pixels, rect, rect, Pos2::new(5.0, 5.0)).is_none());
    }
    #[test]
    fn alpha_view_derivation_preserves_source_and_has_owned_display() {
        let pixels = source([200, 70, 8, 123]);
        let original = pixels.as_raw().clone();
        let derived = derive_alpha(&pixels, &Cancellation::default()).unwrap();
        assert_eq!(pixels.as_raw(), &original);
        assert_eq!(derived.get_pixel(0, 0).0, [123, 123, 123, 255]);
        let display = derived.prepare_display(&Cancellation::default()).unwrap();
        assert!(!display.has_alpha);
        assert_eq!(display.tiles[0].image[(0, 0)], Color32::from_gray(123));
    }
    #[test]
    fn cancelled_alpha_derivation_does_not_change_source() {
        let pixels = source([20, 30, 40, 50]);
        let original = pixels.as_raw().clone();
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(matches!(
            derive_alpha(&pixels, &cancel),
            Err(ToolError::Cancelled)
        ));
        assert_eq!(pixels.as_raw(), &original);
    }
    #[test]
    fn alpha_preparation_failure_keeps_normal_source_and_does_not_retry_implicitly() {
        let pixels = source([10, 20, 30, 40]);
        let original = pixels.as_raw().clone();
        let ctx = egui::Context::default();
        let mut inspector = Inspector::default();
        inspector.enabled = true;
        inspector.alpha_requested = true;
        inspector.on_pixels_changed(Some(&pixels));
        inspector.attempted = Some(inspector.generation);
        let (sender, receiver) = mpsc::sync_channel(1);
        sender
            .send(Err(ToolError::ResourceBusy("synthetic pressure".into())))
            .unwrap();
        inspector.task = Some(AlphaTask {
            generation: inspector.generation,
            cancel: Cancellation::default(),
            receiver,
            thread: None,
        });
        inspector.poll(&ctx, Some(&pixels), 1);
        assert!(inspector.error.as_ref().unwrap().contains("正常图片"));
        assert!(inspector.alpha.is_none() && inspector.task.is_none());
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(90.0, 70.0));
            assert!(!inspector.draw_alpha(ui.painter(), rect, rect, &pixels));
        });
        assert_eq!(pixels.as_raw(), &original);
    }
    #[test]
    fn expired_source_is_distinct_from_no_source_when_cancelling() {
        let pixels = source([1, 2, 3, 4]);
        let mut inspector = Inspector::default();
        inspector.on_pixels_changed(Some(&pixels));
        let generation = inspector.generation;
        drop(pixels);
        inspector.on_pixels_changed(None);
        assert_ne!(inspector.generation, generation);
        assert!(inspector.source.ptr_eq(&Weak::new()));
    }
    #[test]
    fn alpha_switch_cancels_old_generation_and_restores_after_comparison() {
        let a = source([255, 0, 0, 10]);
        let b = source([0, 255, 0, 200]);
        let ctx = egui::Context::default();
        let mut inspector = Inspector::default();
        inspector.enabled = true;
        inspector.alpha_requested = true;
        inspector.poll(&ctx, Some(&a), 1);
        inspector.on_pixels_changed(Some(&b));
        assert!(inspector.alpha.is_none());
        let started = Instant::now();
        let mut frame = 2;
        while inspector.alpha.is_none() {
            inspector.poll(&ctx, Some(&b), frame);
            frame += 1;
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "{:?}",
                inspector.error
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            inspector
                .alpha
                .as_ref()
                .unwrap()
                .source
                .ptr_eq(&Arc::downgrade(&b))
        );
        inspector.set_alpha_available(false);
        assert!(inspector.alpha.is_none() && inspector.alpha_requested);
        inspector.poll(&ctx, Some(&b), frame);
        assert!(inspector.task.is_none());
        inspector.set_alpha_available(true);
        inspector.poll(&ctx, Some(&b), frame + 1);
        assert!(inspector.task.is_some());
        inspector.shutdown();
        assert!(!inspector.is_busy() && inspector.alpha.is_none());
    }
}
