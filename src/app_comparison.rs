//! A fixed, immutable reference shares image/texture ownership with the main canvas.
use super::*;

pub(super) struct PinnedImage {
    pub pixels: Arc<light_image_tool::memory::Pixels>,
    pub tiles: Vec<Tile>,
    pub name: String,
}

pub(super) struct Comparison {
    pub pinned: Option<PinnedImage>,
    pub swapped: bool,
    pub sync: bool,
    zoom: f32,
    pan: Vec2,
    fit: bool,
    #[cfg(test)]
    pub reference_rect: Option<Rect>,
}
impl Default for Comparison {
    fn default() -> Self {
        Self {
            pinned: None,
            swapped: false,
            sync: true,
            zoom: 1.0,
            pan: Vec2::ZERO,
            fit: true,
            #[cfg(test)]
            reference_rect: None,
        }
    }
}
impl Comparison {
    pub fn active(&self) -> bool {
        self.pinned.is_some()
    }
    pub fn shares_texture(&self, id: egui::TextureId) -> bool {
        self.pinned
            .as_ref()
            .is_some_and(|p| p.tiles.iter().any(|t| t.texture.id() == id))
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

pub(super) fn pane_rects(area: Rect, swapped: bool) -> (Rect, Rect) {
    let middle = area.center().x;
    let left = Rect::from_min_max(area.min, Pos2::new(middle - 3.0, area.bottom()));
    let right = Rect::from_min_max(Pos2::new(middle + 3.0, area.top()), area.max);
    if swapped {
        (right, left)
    } else {
        (left, right)
    }
}

impl ImageApp {
    pub(super) fn pin_comparison(&mut self) {
        if self.loading || self.work.is_some() || self.playback.is_some() || self.canvas_unavailable
        {
            return;
        }
        let Some(pixels) = self.pixels.clone() else {
            return;
        };
        if self.tiles.is_empty() {
            return;
        }
        self.close_comparison();
        self.comparison = Comparison {
            pinned: Some(PinnedImage {
                pixels,
                tiles: self.tiles.clone(),
                name: self.document_name(),
            }),
            ..Comparison::default()
        };
        self.fit_window();
        self.status = "已固定 A 图；打开或切换另一张作为 B 图".into();
    }

    pub(super) fn close_comparison(&mut self) {
        if let Some(pinned) = self.comparison.pinned.take() {
            // A may be the last owner of a texture; retain accounting through renderer handoff.
            for tile in pinned.tiles {
                self.retired_texture_reservations
                    .push((self.frames + 2, tile._reservation));
                self.retired_displays.push((self.frames + 2, tile._display));
            }
        }
        self.comparison.clear();
        self.fit_window();
    }

    pub(super) fn comparison_menu(&mut self, ui: &mut egui::Ui) -> bool {
        let ready = self.pixels.is_some()
            && !self.loading
            && self.work.is_none()
            && self.playback.is_none()
            && !self.canvas_unavailable;
        let mut close = false;
        if ui
            .add_enabled(
                ready,
                egui::Button::new(if self.comparison.active() {
                    "将当前图片重新固定为 A"
                } else {
                    "固定当前图片为 A，开始对比"
                }),
            )
            .clicked()
        {
            self.pin_comparison();
            close = true;
        }
        if self.comparison.active() {
            if ui.button("交换 A / B 左右位置").clicked() {
                self.comparison.swapped = !self.comparison.swapped;
                close = true;
            }
            ui.checkbox(&mut self.comparison.sync, "同尺寸图片同步缩放 / 平移");
            if ui.button("退出对比并释放 A 图").clicked() {
                self.close_comparison();
                close = true;
            }
        }
        close
    }

    pub(super) fn refresh_pinned_sampler(&mut self) {
        let options = self.inspector.texture_options();
        if let Some(pinned) = &mut self.comparison.pinned {
            for (tile, prepared) in pinned.tiles.iter_mut().zip(
                pinned
                    .pixels
                    .prepare_display(&Cancellation::default())
                    .into_iter()
                    .flat_map(|d| &d.tiles),
            ) {
                tile.texture
                    .set_partial([0, 0], prepared.image.clone(), options);
            }
        }
    }

    pub(super) fn comparison_canvas(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.comparison.active() {
            return false;
        }
        let area = ui.available_rect_before_wrap();
        if area.width() < 240.0 || area.height() < 100.0 {
            ui.small("窗口过小，扩大窗口可查看双图；也可在更多中退出对比");
            return false;
        }
        let (a_rect, b_rect) = pane_rects(area, self.comparison.swapped);
        // Interact with A first so synchronized changes apply to B in this same frame.
        self.paint_reference(ui, a_rect, true, false);
        ui.scope_builder(egui::UiBuilder::new().max_rect(b_rect), |ui| {
            ui.set_clip_rect(b_rect);
            self.canvas(ui);
        });
        self.paint_reference(ui, a_rect, false, true);
        let painter = ui.painter_at(area);
        painter.line_segment(
            [
                Pos2::new(area.center().x, area.top()),
                Pos2::new(area.center().x, area.bottom()),
            ],
            egui::Stroke::new(1.0, crate::app_theme::palette(ui.ctx()).separator),
        );
        self.paint_compare_label(&painter, a_rect, "A · 固定参考", true);
        self.paint_compare_label(&painter, b_rect, "B · 当前图片", false);
        ui.allocate_rect(area, egui::Sense::hover());
        true
    }

    fn paint_compare_label(
        &self,
        painter: &egui::Painter,
        area: Rect,
        label: &str,
        reference: bool,
    ) {
        let colors = crate::app_theme::palette(painter.ctx());
        let rect = Rect::from_min_size(area.min + Vec2::splat(10.0), Vec2::new(110.0, 27.0));
        painter.rect_filled(
            rect,
            5.0,
            if reference {
                colors.selected
            } else {
                colors.popover
            },
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(13.0),
            if reference {
                colors.accent
            } else {
                colors.text
            },
        );
    }

    fn paint_reference(&mut self, ui: &mut egui::Ui, area: Rect, handle_input: bool, draw: bool) {
        #[cfg(test)]
        {
            self.comparison.reference_rect = Some(area);
        }
        let response = (handle_input && !egui::Popup::is_any_open(ui.ctx())).then(|| {
            ui.interact(
                area,
                egui::Id::new("comparison-reference-canvas"),
                egui::Sense::click_and_drag(),
            )
        });
        let pinned = self.comparison.pinned.as_ref().unwrap();
        let dims = pinned.pixels.dimensions();
        let sync = self.comparison.sync
            && self.pixels.as_ref().is_some_and(|p| p.dimensions() == dims)
            && !self.long_image;
        let ppp = ui.ctx().pixels_per_point();
        let base = Vec2::new(dims.0 as f32, dims.1 as f32) / ppp;
        let mut zoom = if sync {
            self.zoom
        } else {
            self.comparison.zoom
        };
        let mut pan = if sync { self.pan } else { self.comparison.pan };
        let mut fit = if sync { self.fit } else { self.comparison.fit };
        if fit {
            zoom = ((area.width() - 40.0) / base.x)
                .min((area.height() - 40.0) / base.y)
                .clamp(0.01, 1.0);
        }
        if response.as_ref().is_some_and(|r| r.dragged()) {
            pan += ui.input(|i| i.pointer.delta());
            fit = false;
        }
        if response.as_ref().is_some_and(|r| r.hovered()) {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.01 {
                let old = zoom;
                zoom = (zoom * (scroll * 0.002).exp()).clamp(0.01, 32.0);
                if let Some(mouse) = ui.input(|i| i.pointer.hover_pos()) {
                    pan = (mouse - area.center()) - (mouse - area.center() - pan) * (zoom / old);
                }
                fit = false;
            }
        }
        if response.as_ref().is_some_and(|r| r.double_clicked()) {
            fit = true;
            pan = Vec2::ZERO;
        }
        if sync {
            self.zoom = zoom;
            self.pan = pan;
            self.fit = fit;
        } else {
            self.comparison.zoom = zoom;
            self.comparison.pan = pan;
            self.comparison.fit = fit;
        }
        if !draw {
            return;
        }
        let painter = ui.painter_at(area);
        let (background, checker) = self.canvas_colors(ui.ctx());
        painter.rect_filled(
            area,
            0.0,
            Color32::from_rgb(background[0], background[1], background[2]),
        );
        if let Some(colors) = checker {
            viewer_ui::paint_checker(&painter, area, colors);
        }
        let top = area.center() + pan - base * zoom / 2.0;
        for tile in &pinned.tiles {
            let dest = Rect::from_min_size(
                top + Vec2::new(tile.x as f32, tile.y as f32) * zoom / ppp,
                Vec2::new(tile.w as f32, tile.h as f32) * zoom / ppp,
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
        let caption = Rect::from_min_size(
            area.left_bottom() + Vec2::new(8.0, -30.0),
            Vec2::new(area.width() - 16.0, 25.0),
        );
        ui.put(caption, egui::Label::new(&pinned.name).truncate())
            .on_hover_text(&pinned.name);
    }

    pub(super) fn set_reference_on_top(&mut self, on_top: bool, ctx: &egui::Context) {
        self.reference_on_top = on_top;
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        }));
    }

    pub(super) fn reference_badge(&mut self, ui: &mut egui::Ui, area: Rect) {
        if !self.reference_on_top {
            return;
        }
        let rect = Rect::from_min_size(
            area.min + Vec2::new(12.0, if self.comparison.active() { 44.0 } else { 12.0 }),
            Vec2::new(176.0, 30.0),
        );
        if ui
            .put(
                rect,
                egui::Button::new("当前窗口置顶 · 点击取消")
                    .fill(crate::app_theme::palette(ui.ctx()).selected),
            )
            .clicked()
        {
            self.set_reference_on_top(false, ui.ctx());
        }
    }

    pub(super) fn prepare_feature_smoke(&mut self, ctx: &egui::Context) {
        let Some(smoke) = &self.smoke else {
            return;
        };
        if smoke.feature_stage == 2 || self.loading || self.work.is_some() {
            return;
        }
        let stage = smoke.feature_stage;
        let compare = smoke.compare_image.clone();
        let inspect = smoke.inspect_pixels || smoke.alpha_view;
        let alpha = smoke.alpha_view;
        let nearest = smoke.nearest_pixels;
        let reference = smoke.reference_on_top;
        let show_sticker = smoke.show_sticker || smoke.sticker_image.is_some();
        let sticker_image = smoke.sticker_image.clone();
        let sticker_directory = smoke.directory.clone();
        let zoom_smoke = smoke.sticker_zoom_smoke;
        let show_rename = smoke.show_rename;
        if stage == 0 && self.loaded.is_some() && show_sticker {
            self.stickers.configure_smoke(sticker_directory);
            self.stickers.configure_zoom_smoke(zoom_smoke);
            self.create_sticker(ctx);
            if let Some(path) = sticker_image {
                self.smoke.as_mut().unwrap().feature_stage = 1;
                self.open(path);
                ctx.request_repaint();
                return;
            }
        }
        if stage == 0
            && self.loaded.is_some()
            && let Some(path) = compare
        {
            self.pin_comparison();
            self.smoke.as_mut().unwrap().feature_stage = 1;
            self.open(path);
            ctx.request_repaint();
            return;
        }
        self.inspector.configure_smoke(inspect, nearest, alpha);
        if nearest && self.pixels.is_some() {
            self.refresh_texture(ctx);
            self.refresh_pinned_sampler();
        }
        if reference {
            self.set_reference_on_top(true, ctx);
        }
        if show_rename && self.loaded.is_some() {
            self.begin_rename();
        }
        self.smoke.as_mut().unwrap().feature_stage = 2;
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn loaded_app() -> (tempfile::TempDir, ImageApp, egui::Context, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a.png");
        let second = dir.path().join("b.png");
        image::RgbaImage::from_pixel(40, 30, image::Rgba([200, 30, 60, 255]))
            .save(&first)
            .unwrap();
        image::RgbaImage::from_pixel(40, 30, image::Rgba([20, 70, 190, 255]))
            .save(&second)
            .unwrap();
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "comparison-test".into(), None, None);
        app.apply_load_response(
            light_image_tool::worker::LoadResponse {
                generation: 0,
                siblings: vec![first.clone(), second.clone()],
                result: light_image_tool::pipeline::load(&first, &Cancellation::default()),
            },
            &ctx,
        );
        (dir, app, ctx, second)
    }
    #[test]
    fn pinned_a_cannot_be_overwritten_by_same_grid_b_upload() {
        let (_dir, mut app, ctx, second) = loaded_app();
        app.pin_comparison();
        let a_ids: Vec<_> = app
            .comparison
            .pinned
            .as_ref()
            .unwrap()
            .tiles
            .iter()
            .map(|t| t.texture.id())
            .collect();
        app.apply_load_response(
            light_image_tool::worker::LoadResponse {
                generation: 0,
                siblings: vec![],
                result: light_image_tool::pipeline::load(&second, &Cancellation::default()),
            },
            &ctx,
        );
        assert_eq!(
            app.comparison
                .pinned
                .as_ref()
                .unwrap()
                .pixels
                .get_pixel(0, 0)
                .0,
            [200, 30, 60, 255]
        );
        assert_eq!(
            app.pixels.as_ref().unwrap().get_pixel(0, 0).0,
            [20, 70, 190, 255]
        );
        assert!(app.tiles.iter().all(|t| !a_ids.contains(&t.texture.id())));
        assert!(app.loaded.as_ref().is_some_and(|i| i.path == second));
    }
    #[test]
    fn repinning_keeps_previous_a_budget_until_renderer_handoff() {
        let (_dir, mut app, ctx, second) = loaded_app();
        app.pin_comparison();
        app.apply_load_response(
            light_image_tool::worker::LoadResponse {
                generation: 0,
                siblings: vec![],
                result: light_image_tool::pipeline::load(&second, &Cancellation::default()),
            },
            &ctx,
        );
        app.retired_texture_reservations.clear();
        app.retired_displays.clear();
        let old = Arc::downgrade(&app.comparison.pinned.as_ref().unwrap().tiles[0]._reservation);
        app.pin_comparison();
        assert!(old.upgrade().is_some());
        app.retired_texture_reservations.clear();
        app.retired_displays.clear();
        assert!(old.upgrade().is_none());
    }
    #[test]
    fn swapped_panes_remain_disjoint_and_preserve_current_b_geometry() {
        let area = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(800.0, 600.0));
        let (a, b) = pane_rects(area, false);
        let (swapped_a, swapped_b) = pane_rects(area, true);
        assert!(!a.intersects(b));
        assert_eq!(a, swapped_b);
        assert_eq!(b, swapped_a);
        assert_eq!(a.size(), b.size());
    }
    #[test]
    fn reference_toggle_only_changes_window_level_and_is_reversible() {
        let (_dir, mut app, ctx, _second) = loaded_app();
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        for (on_top, level) in [
            (true, egui::WindowLevel::AlwaysOnTop),
            (false, egui::WindowLevel::Normal),
        ] {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                app.set_reference_on_top(on_top, ui.ctx())
            });
            output.textures_delta.clear();
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            assert!(commands.iter().any(|command| matches!(command, egui::ViewportCommand::WindowLevel(actual) if *actual == level)));
            assert!(!commands.iter().any(|command| matches!(
                command,
                egui::ViewportCommand::OuterPosition(_) | egui::ViewportCommand::InnerSize(_)
            )));
            assert_eq!(app.reference_on_top, on_top);
        }
    }
    #[test]
    fn scrolling_b_draws_a_at_the_same_scale_in_the_same_frame() {
        let (_dir, mut app, ctx, second) = loaded_app();
        app.pin_comparison();
        app.apply_load_response(
            light_image_tool::worker::LoadResponse {
                generation: 0,
                siblings: vec![],
                result: light_image_tool::pipeline::load(&second, &Cancellation::default()),
            },
            &ctx,
        );
        app.fit = false;
        app.zoom = 4.0;
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 600.0));
        let raw = || egui::RawInput {
            screen_rect: Some(screen),
            focused: true,
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw(), |ui| {
            app.comparison_canvas(ui);
        });
        output.textures_delta.clear();
        let b = app.canvas_viewport.unwrap().center();
        let mut input = raw();
        input.events.push(egui::Event::PointerMoved(b));
        input.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, 100.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        let mut output = ctx.run_ui(input, |ui| {
            app.comparison_canvas(ui);
        });
        output.textures_delta.clear();
        assert!(app.zoom > 4.0);
        let a_id = app.comparison.pinned.as_ref().unwrap().tiles[0]
            .texture
            .id();
        let b_id = app.tiles[0].texture.id();
        let width = |id| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Mesh(mesh) if mesh.texture_id == id => {
                        Some(mesh.calc_bounds().width())
                    }
                    _ => None,
                })
                .unwrap()
        };
        assert!((width(a_id) - width(b_id)).abs() < 0.01);
    }
}
