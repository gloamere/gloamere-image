//! Canvas shortcuts invoke the same document actions as the toolbar and menus.
use super::*;

impl ImageApp {
    fn canvas_actions_idle(&self) -> bool {
        !self.loading
            && !self.rename.blocks_document()
            && self.work.is_none()
            && !self.shutting_down
            && self.pending_intent.is_none()
            && !self.path_dialog
            && !self.export_dialog
            && !self.extract_dialog
            && self.ai_view.confirm.is_none()
            && self.error.is_none()
    }

    pub(super) fn canvas_actions_ready(&self) -> bool {
        self.canvas_actions_idle() && self.pixels.is_some() && !self.canvas_unavailable
    }

    pub(super) fn canvas_context_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let idle = self.canvas_actions_idle();
        let ready = self.canvas_actions_ready();
        let file = idle && self.current_file_path().is_some();
        let static_image = ready && self.playback.is_none();
        ui.set_width(280.0_f32.min((ctx.viewport_rect().width() - 24.0).max(1.0)));
        ui.spacing_mut().interact_size.y = 34.0;

        if self.pixels.is_none() {
            if ui
                .add_enabled(
                    idle,
                    ActionButton::new(Icon::Open, "打开图片… · Ctrl+O").menu(),
                )
                .clicked()
            {
                ui.close();
                self.open_dialog();
            }
            if ui
                .add_enabled(
                    idle,
                    ActionButton::new(Icon::Paste, "从剪贴板打开 · Ctrl+V").menu(),
                )
                .clicked()
            {
                ui.close();
                self.request_intent(PendingIntent::Clipboard, &ctx);
            }
            return;
        }

        if ui
            .add_enabled(
                ready,
                ActionButton::new(
                    Icon::Pin,
                    if self.playback.is_some() {
                        "贴图置顶（当前帧）"
                    } else {
                        "贴图置顶（独立参考窗）"
                    },
                )
                .menu(),
            )
            .clicked()
        {
            ui.close();
            self.create_sticker(&ctx);
        }
        if ui
            .add_enabled(
                ready,
                ActionButton::new(
                    Icon::Copy,
                    if self.playback.is_some() {
                        "复制当前帧 · Ctrl+C"
                    } else {
                        "复制图像 · Ctrl+C"
                    },
                )
                .menu(),
            )
            .clicked()
        {
            ui.close();
            self.copy_image(&ctx);
        }
        if ui
            .add_enabled(
                ready,
                ActionButton::new(
                    Icon::Export,
                    if self.playback.is_some() {
                        "提取当前帧为静态副本…"
                    } else {
                        "导出新副本 · Ctrl+Shift+S"
                    },
                )
                .menu(),
            )
            .clicked()
        {
            ui.close();
            self.begin_export();
        }
        ui.separator();
        if ui
            .add_enabled(ready, ActionButton::new(Icon::Fit, "适应窗口 · F").menu())
            .clicked()
        {
            ui.close();
            self.fit_window();
        }
        if ui
            .add_enabled(
                ready,
                ActionButton::new(Icon::Actual, "实际像素 1:1 · 1").menu(),
            )
            .clicked()
        {
            ui.close();
            self.actual_pixels();
        }
        ui.separator();
        ui.menu_button("编辑", |ui| {
            if ui
                .add_enabled(
                    static_image,
                    ActionButton::new(Icon::Rotate, "右转 90° · R").menu(),
                )
                .clicked()
            {
                ui.close();
                self.start_edit(EditOp::Rotate90, &ctx);
            }
            if ui
                .add_enabled(
                    static_image,
                    ActionButton::new(Icon::RotateLeft, "左转 90° · Shift+R").menu(),
                )
                .clicked()
            {
                ui.close();
                self.start_edit(EditOp::Rotate270, &ctx);
            }
            if ui
                .add_enabled(
                    static_image && !self.undo.is_empty(),
                    ActionButton::new(Icon::Undo, "撤销 · Ctrl+Z").menu(),
                )
                .clicked()
            {
                ui.close();
                self.undo_edit(&ctx);
            }
            if ui
                .add_enabled(
                    static_image && !self.redo.is_empty(),
                    ActionButton::new(Icon::Restore, "重做 · Ctrl+Y / Ctrl+Shift+Z").menu(),
                )
                .clicked()
            {
                ui.close();
                self.redo_edit(&ctx);
            }
            if self.playback.is_some() {
                ui.small("先提取当前帧，再编辑静态副本。");
            }
        });
        ui.menu_button("文件", |ui| {
            if ui
                .add_enabled(idle, ActionButton::new(Icon::Open, "打开其他图片…").menu())
                .clicked()
            {
                ui.close();
                self.open_dialog();
            }
            if ui
                .add_enabled(
                    idle,
                    ActionButton::new(Icon::Paste, "从剪贴板打开 · Ctrl+V").menu(),
                )
                .clicked()
            {
                ui.close();
                self.request_intent(PendingIntent::Clipboard, &ctx);
            }
            ui.separator();
            if ui
                .add_enabled(
                    file,
                    ActionButton::new(Icon::Refresh, "重新载入当前图片 · F5").menu(),
                )
                .clicked()
            {
                ui.close();
                self.reload_current();
            }
            if ui
                .add_enabled(file, ActionButton::new(Icon::Open, "在文件夹中显示").menu())
                .clicked()
            {
                ui.close();
                self.show_current_in_folder(&ctx);
            }
            if ui
                .add_enabled(file, ActionButton::new(Icon::Copy, "复制文件路径").menu())
                .clicked()
            {
                ui.close();
                self.copy_path(&ctx);
            }
        });
        ui.separator();
        if ui
            .add_enabled(
                ready && file,
                ActionButton::new(
                    Icon::AiDetect,
                    if self.edited {
                        "AI 检测原文件"
                    } else {
                        "AI 检测当前图片"
                    },
                )
                .menu(),
            )
            .clicked()
        {
            ui.close();
            self.ai_detect_current();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(with_pixels: bool) -> (ImageApp, egui::Context) {
        let ctx = egui::Context::default();
        configure(&ctx);
        let mut app = ImageApp::from_context(ctx.clone(), "context-menu-test".into(), None, None);
        if with_pixels {
            app.pixels = Some(Arc::new(
                light_image_tool::memory::Pixels::from_image(image::RgbaImage::from_pixel(
                    4,
                    3,
                    image::Rgba([20, 40, 60, 255]),
                ))
                .unwrap(),
            ));
            app.source = DocumentSource::Clipboard;
        }
        (app, ctx)
    }

    fn pointer(position: Pos2, button: egui::PointerButton, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn draw(app: &mut ImageApp, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 720.0))),
                events,
                ..Default::default()
            },
            |ui| {
                let (_, response) =
                    ui.allocate_exact_size(Vec2::new(780.0, 700.0), egui::Sense::click_and_drag());
                response.context_menu(|ui| app.canvas_context_menu(ui));
            },
        )
    }

    fn open_menu(app: &mut ImageApp, ctx: &egui::Context) -> egui::FullOutput {
        draw(app, ctx, vec![]);
        for pressed in [true, false] {
            draw(
                app,
                ctx,
                pointer(
                    Pos2::new(40.0, 40.0),
                    egui::PointerButton::Secondary,
                    pressed,
                ),
            );
        }
        let output = draw(app, ctx, vec![]);
        assert!(egui::Popup::is_any_open(ctx));
        output
    }

    fn label_position(output: &egui::FullOutput, label: &str) -> Option<Pos2> {
        fn find(shape: &egui::Shape, label: &str) -> Option<Pos2> {
            match shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() / 2.0)
                }
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, label)),
                _ => None,
            }
        }
        output
            .shapes
            .iter()
            .find_map(|shape| find(&shape.shape, label))
    }

    fn click(app: &mut ImageApp, ctx: &egui::Context, position: Pos2) {
        for pressed in [true, false] {
            draw(
                app,
                ctx,
                pointer(position, egui::PointerButton::Primary, pressed),
            );
        }
    }

    #[test]
    fn empty_canvas_right_click_offers_open_and_paste_only() {
        let (mut app, ctx) = app(false);
        let output = open_menu(&mut app, &ctx);
        assert!(label_position(&output, "打开图片… · Ctrl+O").is_some());
        assert!(label_position(&output, "从剪贴板打开 · Ctrl+V").is_some());
        assert!(label_position(&output, "复制图像 · Ctrl+C").is_none());
        assert!(label_position(&output, "贴图置顶（独立参考窗）").is_none());
        assert!(app.work.is_none() && app.pixels.is_none());
    }

    #[test]
    fn right_click_fit_invokes_existing_action_and_closes_popup() {
        let (mut app, ctx) = app(true);
        app.fit = false;
        app.zoom = 2.0;
        app.pan = Vec2::new(25.0, 35.0);
        let output = open_menu(&mut app, &ctx);
        assert!(label_position(&output, "贴图置顶（独立参考窗）").is_some());
        let position = label_position(&output, "适应窗口 · F").unwrap();
        click(&mut app, &ctx, position);
        assert!(app.fit);
        assert_eq!(app.pan, Vec2::ZERO);
        assert!(!egui::Popup::is_any_open(&ctx));
    }

    #[test]
    fn loading_and_pending_confirmation_disable_canvas_actions() {
        let (mut app, ctx) = app(true);
        for pending in [false, true] {
            egui::Popup::close_all(&ctx);
            app.loading = !pending;
            app.pending_intent = pending.then_some(PendingIntent::Close);
            app.fit = false;
            app.pan = Vec2::new(25.0, 35.0);
            let output = open_menu(&mut app, &ctx);
            let position = label_position(&output, "适应窗口 · F").unwrap();
            click(&mut app, &ctx, position);
            assert!(!app.fit);
            assert_eq!(app.pan, Vec2::new(25.0, 35.0));
            assert!(!app.export_dialog && app.work.is_none());
        }
    }

    #[test]
    fn right_click_export_opens_existing_dialog_and_closes_popup() {
        let (mut app, ctx) = app(true);
        let output = open_menu(&mut app, &ctx);
        let position = label_position(&output, "导出新副本 · Ctrl+Shift+S").unwrap();
        click(&mut app, &ctx, position);
        assert!(app.export_dialog);
        assert_eq!(app.export_generation, Some(app.generation));
        assert!(!egui::Popup::is_any_open(&ctx));
        assert!(app.work.is_none());
    }
}
