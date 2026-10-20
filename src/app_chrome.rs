//! The main frame keeps document actions outside the image surface.
use super::*;

pub(super) const HEIGHT: f32 = 36.0;
pub(super) const BUTTON: f32 = 28.0;
const LIST_LEFT: f32 = 40.0;
const TITLE_LEFT: f32 = LIST_LEFT + BUTTON + 8.0;

#[derive(Clone, Copy)]
enum ChromeIcon {
    List(bool),
    Pin,
    Minimize,
    Maximize(bool),
    Close,
}

fn resize_direction(rect: Rect, pos: Pos2) -> Option<egui::viewport::ResizeDirection> {
    if !rect.contains(pos) {
        return None;
    }
    let left = pos.x < rect.left() + 4.0;
    let right = pos.x >= rect.right() - 4.0;
    let top = pos.y < rect.top() + 4.0;
    let bottom = pos.y >= rect.bottom() - 4.0;
    use egui::viewport::ResizeDirection as D;
    Some(match (left, right, top, bottom) {
        (true, _, true, _) => D::NorthWest,
        (_, true, true, _) => D::NorthEast,
        (true, _, _, true) => D::SouthWest,
        (_, true, _, true) => D::SouthEast,
        (true, _, _, _) => D::West,
        (_, true, _, _) => D::East,
        (_, _, true, _) => D::North,
        (_, _, _, true) => D::South,
        _ => return None,
    })
}

fn button(ui: &mut egui::Ui, rect: Rect, icon: ChromeIcon, label: &str) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.make_persistent_id(if matches!(icon, ChromeIcon::List(_)) {
            "window-image-list-toggle"
        } else {
            label
        }),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        if let ChromeIcon::List(selected) = icon {
            egui::WidgetInfo::selected(egui::WidgetType::Button, ui.is_enabled(), selected, label)
        } else {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
        }
    });
    if response.has_focus() {
        ui.ctx()
            .data_mut(|data| data.insert_temp(egui::Id::new("viewer-command-focus"), response.id));
    }
    let close_hover = matches!(icon, ChromeIcon::Close) && response.hovered();
    let green = Color32::from_rgb(43, 103, 78);
    let color = if !ui.is_enabled() {
        Color32::from_gray(176)
    } else if close_hover {
        Color32::WHITE
    } else if matches!(icon, ChromeIcon::Pin | ChromeIcon::List(true)) {
        green
    } else {
        Color32::from_rgb(94, 108, 100)
    };
    if response.hovered() || response.has_focus() || matches!(icon, ChromeIcon::List(true)) {
        ui.painter().rect_filled(
            rect,
            5.0,
            if close_hover {
                Color32::from_rgb(190, 64, 64)
            } else {
                Color32::from_rgb(228, 237, 231)
            },
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            5.0,
            egui::Stroke::new(1.0, green),
            egui::StrokeKind::Inside,
        );
    }
    let p = |x, y| rect.center() + Vec2::new(x, y);
    let stroke = egui::Stroke::new(1.25, color);
    let line = |points: &[(f32, f32)]| {
        ui.painter().add(egui::Shape::line(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    match icon {
        ChromeIcon::List(_) => {
            let outline = Rect::from_center_size(rect.center(), Vec2::splat(16.0));
            ui.painter()
                .rect_stroke(outline, 2.0, stroke, egui::StrokeKind::Inside);
            line(&[(-3.0, -6.75), (-3.0, 6.75)]);
        }
        ChromeIcon::Pin => viewer_ui::paint_pin(ui.painter(), rect.center(), 16.0, color),
        ChromeIcon::Minimize => line(&[(-4.5, 0.0), (4.5, 0.0)]),
        ChromeIcon::Maximize(maximized) => {
            if maximized {
                line(&[(-2.0, -4.5), (4.5, -4.5), (4.5, 2.0)]);
            }
            ui.painter().rect_stroke(
                Rect::from_center_size(
                    p(
                        if maximized { -1.0 } else { 0.0 },
                        if maximized { 1.0 } else { 0.0 },
                    ),
                    Vec2::splat(8.0),
                ),
                0.5,
                stroke,
                egui::StrokeKind::Inside,
            );
        }
        ChromeIcon::Close => {
            line(&[(-4.0, -4.0), (4.0, 4.0)]);
            line(&[(4.0, -4.0), (-4.0, 4.0)]);
        }
    }
    response.on_hover_text(label)
}

impl ImageApp {
    pub(super) fn window_chrome(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.fullscreen {
            return;
        }
        let maximized = ctx.input(|input| input.viewport().maximized == Some(true));
        egui::Panel::top("viewer-window-chrome")
            .exact_size(HEIGHT)
            .frame(egui::Frame::NONE.fill(Color32::from_rgb(249, 251, 249)))
            .show(ui, |ui| {
                let area = ui.max_rect();
                let control_x = area.right() - BUTTON * 4.0 - 16.0;
                let y = area.center().y - BUTTON / 2.0;
                let rect = |index| {
                    Rect::from_min_size(
                        Pos2::new(control_x + index as f32 * BUTTON, y),
                        Vec2::splat(BUTTON),
                    )
                };
                let logo = Rect::from_center_size(
                    Pos2::new(area.left() + 20.0, area.center().y),
                    Vec2::splat(20.0),
                );
                ui.painter().image(
                    self.brand_texture.id(),
                    logo,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                let list_rect =
                    Rect::from_min_size(Pos2::new(area.left() + LIST_LEFT, y), Vec2::splat(BUTTON));
                let list_enabled = !self.path_dialog
                    && !self.export_dialog
                    && self.error.is_none()
                    && self.pending_intent.is_none()
                    && !self.extract_dialog
                    && self.ai_view.confirm.is_none()
                    && !self.rename.blocks_document();
                if ui
                    .add_enabled_ui(list_enabled, |ui| {
                        button(
                            ui,
                            list_rect,
                            ChromeIcon::List(self.preferences.list_visible),
                            if self.preferences.list_visible {
                                "隐藏图片列表"
                            } else {
                                "显示图片列表"
                            },
                        )
                    })
                    .inner
                    .clicked()
                {
                    self.toggle_list();
                }
                let title = format!(
                    "{}{}",
                    self.document_name(),
                    if self.unsaved() { " *" } else { "" }
                );
                let width = (control_x - area.left() - TITLE_LEFT - 12.0).max(16.0);
                let galley = ui.painter().layout_no_wrap(
                    title.clone(),
                    egui::FontId::proportional(13.0),
                    Color32::from_rgb(58, 73, 65),
                );
                let title_rect = Rect::from_min_size(
                    Pos2::new(area.left() + TITLE_LEFT, area.top() + 4.0),
                    Vec2::new(galley.size().x.min(width) + 4.0, BUTTON),
                );
                #[cfg(test)]
                {
                    self.title_name_rect = Some(title_rect);
                }
                let response = ui.put(
                    title_rect,
                    egui::Label::new(
                        egui::RichText::new(title)
                            .size(13.0)
                            .color(Color32::from_rgb(58, 73, 65)),
                    )
                    .truncate()
                    .sense(egui::Sense::click_and_drag()),
                );
                if response.has_focus() {
                    ctx.data_mut(|data| {
                        data.insert_temp(egui::Id::new("viewer-command-focus"), response.id)
                    });
                }
                if response.double_clicked() && !self.rename.blocks_document() {
                    self.begin_rename();
                } else if response.drag_started_by(egui::PointerButton::Primary)
                    && !self.rename.blocks_document()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                response.on_hover_text(format!(
                    "{}\n双击修改文件名 · 拖动移动窗口",
                    self.document_name()
                ));
                let drag_rect = Rect::from_min_max(
                    Pos2::new(title_rect.right(), area.top() + 4.0),
                    Pos2::new(control_x - 8.0, area.bottom() - 4.0),
                );
                if drag_rect.is_positive() {
                    let drag = ui.interact(
                        drag_rect,
                        ui.id().with("window-drag"),
                        egui::Sense::click_and_drag(),
                    );
                    if drag.double_clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                    } else if drag.drag_started_by(egui::PointerButton::Primary) {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                }
                #[cfg(test)]
                {
                    self.sticker_button_rect = Some(rect(0));
                }
                let ready = self.canvas_actions_ready() && !self.tiles.is_empty();
                if ui
                    .add_enabled_ui(ready, |ui| button(ui, rect(0), ChromeIcon::Pin, "贴图置顶"))
                    .inner
                    .clicked()
                {
                    self.create_sticker(&ctx);
                }
                ui.painter().line_segment(
                    [
                        Pos2::new(control_x + BUTTON - 1.0, y + 7.0),
                        Pos2::new(control_x + BUTTON - 1.0, y + BUTTON - 7.0),
                    ],
                    egui::Stroke::new(1.0, Color32::from_rgb(222, 230, 224)),
                );
                if button(ui, rect(1), ChromeIcon::Minimize, "最小化").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
                if button(
                    ui,
                    rect(2),
                    ChromeIcon::Maximize(maximized),
                    if maximized {
                        "还原窗口"
                    } else {
                        "最大化"
                    },
                )
                .clicked()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }
                if button(ui, rect(3), ChromeIcon::Close, "关闭窗口").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                ui.painter().hline(
                    area.x_range(),
                    area.bottom() - 0.5,
                    egui::Stroke::new(1.0, Color32::from_rgb(225, 232, 227)),
                );
            });
    }

    pub(super) fn window_edges(&self, ctx: &egui::Context) {
        if self.fullscreen || ctx.input(|i| i.viewport().maximized == Some(true)) {
            return;
        }
        let rect = ctx.viewport_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("window-outline"),
        ));
        painter.rect_stroke(
            rect.shrink(0.5),
            0.0,
            egui::Stroke::new(1.0, Color32::from_rgb(202, 214, 206)),
            egui::StrokeKind::Inside,
        );
        use egui::viewport::ResizeDirection as D;
        if let Some(direction) = ctx
            .input(|i| i.pointer.hover_pos())
            .and_then(|pos| resize_direction(rect, pos))
        {
            let cursor = match direction {
                D::NorthWest | D::SouthEast => egui::CursorIcon::ResizeNwSe,
                D::NorthEast | D::SouthWest => egui::CursorIcon::ResizeNeSw,
                D::West | D::East => egui::CursorIcon::ResizeHorizontal,
                _ => egui::CursorIcon::ResizeVertical,
            };
            ctx.set_cursor_icon(cursor);
        }
        if let Some(direction) = ctx
            .input(|i| {
                i.pointer
                    .button_pressed(egui::PointerButton::Primary)
                    .then(|| i.pointer.press_origin())
                    .flatten()
            })
            .and_then(|pos| resize_direction(rect, pos))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome_frame(
        app: &mut ImageApp,
        ctx: &egui::Context,
        size: Vec2,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                focused: true,
                events,
                ..Default::default()
            },
            |ui| app.window_chrome(ui),
        );
        output.textures_delta.clear();
        output
    }

    fn click_list(app: &mut ImageApp, ctx: &egui::Context, size: Vec2) {
        let position = Pos2::new(LIST_LEFT + BUTTON / 2.0, HEIGHT / 2.0);
        for pressed in [true, false] {
            chrome_frame(
                app,
                ctx,
                size,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        chrome_frame(app, ctx, size, vec![]);
    }

    fn accessible_button<'a>(
        output: &'a egui::FullOutput,
        label: &str,
    ) -> &'a egui::accesskit::Node {
        &output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.label() == Some(label)
            })
            .unwrap_or_else(|| panic!("missing chrome button: {label}"))
            .1
    }

    #[test]
    fn title_list_button_toggles_repeatedly_without_an_open_document() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = ImageApp::from_context(ctx.clone(), "chrome-test".into(), None, None);
        let size = Vec2::new(360.0, 320.0);
        let output = chrome_frame(&mut app, &ctx, size, vec![]);
        assert!(!accessible_button(&output, "显示图片列表").is_disabled());
        assert!(!app.preferences.list_visible);
        app.locate_current = false;
        for expected in [true, false, true, false] {
            click_list(&mut app, &ctx, size);
            assert_eq!(app.preferences.list_visible, expected);
            if expected {
                assert!(app.locate_current);
                app.locate_current = false;
            } else {
                assert!(app.thumbnail_busy.is_none());
            }
        }
    }

    #[test]
    fn document_modals_disable_the_list_button_and_keep_window_controls_enabled() {
        for modal in ["path", "export", "error", "pending", "extract", "ai"] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut app = ImageApp::from_context(ctx.clone(), "chrome-test".into(), None, None);
            match modal {
                "path" => app.path_dialog = true,
                "export" => app.export_dialog = true,
                "error" => app.error = Some("test".into()),
                "pending" => app.pending_intent = Some(PendingIntent::Close),
                "extract" => app.extract_dialog = true,
                "ai" => app.ai_view.confirm = Some(ai_ui::ClearRecords::Automatic),
                _ => unreachable!(),
            }
            let size = Vec2::new(800.0, 550.0);
            let output = chrome_frame(&mut app, &ctx, size, vec![]);
            assert!(
                accessible_button(&output, "显示图片列表").is_disabled(),
                "{modal}"
            );
            for label in ["最小化", "最大化", "关闭窗口"] {
                assert!(
                    !accessible_button(&output, label).is_disabled(),
                    "{modal}: {label}"
                );
            }
            click_list(&mut app, &ctx, size);
            assert!(!app.preferences.list_visible, "{modal}");
        }
    }

    #[test]
    fn rename_dialog_disables_the_list_button_until_cancelled() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("rename.png");
        image::RgbaImage::from_pixel(4, 3, image::Rgba([50, 70, 60, 255]))
            .save(&source)
            .unwrap();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = ImageApp::from_context(ctx.clone(), "chrome-test".into(), None, None);
        let loaded = light_image_tool::pipeline::load(&source, &Cancellation::default()).unwrap();
        app.pixels = Some(loaded.pixels.clone());
        app.loaded = Some(loaded);
        app.requested_path = Some(source);
        app.begin_rename();
        assert!(app.rename.blocks_document());
        let size = Vec2::new(800.0, 550.0);
        let output = chrome_frame(&mut app, &ctx, size, vec![]);
        assert!(accessible_button(&output, "显示图片列表").is_disabled());
        click_list(&mut app, &ctx, size);
        assert!(!app.preferences.list_visible);
        assert!(app.rename.cancel_dialog());
        chrome_frame(&mut app, &ctx, size, vec![]);
        click_list(&mut app, &ctx, size);
        assert!(app.preferences.list_visible);
    }

    #[test]
    fn title_list_toggle_and_file_name_fit_before_the_existing_pin_controls() {
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "chrome-test".into(), None, None);
        for width in [360.0, 640.0, 1180.0] {
            chrome_frame(&mut app, &ctx, Vec2::new(width, 550.0), vec![]);
            let list = Rect::from_min_size(
                Pos2::new(LIST_LEFT, (HEIGHT - BUTTON) / 2.0),
                Vec2::splat(BUTTON),
            );
            let title = app.title_name_rect.unwrap();
            let pin = app.sticker_button_rect.unwrap();
            let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, HEIGHT));
            assert!(viewport.contains_rect(list));
            assert!(viewport.contains_rect(title));
            assert!(viewport.contains_rect(pin));
            assert!(list.right() + 8.0 <= title.left());
            assert!(title.right() + 8.0 <= pin.left());
            assert_eq!(pin.left(), width - BUTTON * 4.0 - 16.0);
        }
    }

    #[test]
    fn list_glyph_is_an_upright_16_point_split_panel_inside_a_28_point_button() {
        for selected in [false, true] {
            let ctx = egui::Context::default();
            let rect = Rect::from_center_size(Pos2::new(50.0, 50.0), Vec2::splat(BUTTON));
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                assert_eq!(
                    button(ui, rect, ChromeIcon::List(selected), "图片列表").rect,
                    rect
                );
            });
            let outlines: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rectangle) if rectangle.stroke.width > 0.0 => Some(rectangle),
                    _ => None,
                })
                .collect();
            assert_eq!(outlines.len(), 1);
            let outline = outlines[0].rect;
            assert_eq!(outline.size(), Vec2::splat(16.0));
            assert_eq!(outline.center(), rect.center());
            assert!(rect.contains_rect(outline));
            assert!(output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Path(path)
                    if path.points.len() == 2
                        && path.points[0].x == path.points[1].x
                        && path.points[0].y < path.points[1].y
                        && outline.contains(path.points[0])
                        && outline.contains(path.points[1]))
            }));
        }
    }

    #[test]
    fn title_pin_keeps_28_point_target_with_a_centered_16_point_glyph() {
        let ctx = egui::Context::default();
        let rect = Rect::from_center_size(Pos2::new(50.0, 50.0), Vec2::splat(BUTTON));
        let mut target = None;
        let output = ctx.run_ui(egui::RawInput::default(), |ui| {
            target = Some(button(ui, rect, ChromeIcon::Pin, "贴图置顶").rect);
        });
        assert_eq!(target.unwrap().size(), Vec2::splat(28.0));
        let bounds = output
            .shapes
            .iter()
            .filter(|shape| {
                matches!(
                    shape.shape,
                    egui::Shape::Path(_) | egui::Shape::LineSegment { .. }
                )
            })
            .fold(Rect::NOTHING, |bounds, shape| {
                bounds.union(shape.shape.visual_bounding_rect())
            });
        assert!(bounds.center().distance(rect.center()) < 0.001);
        assert!((14.0..=16.0).contains(&bounds.height()));
        assert!(bounds.width() < bounds.height());
        assert!(rect.contains_rect(bounds));
    }

    #[test]
    fn resize_uses_press_origin_when_pointer_events_share_a_frame() {
        for (origin, destination, expected) in [
            (Pos2::new(200.0, 200.0), Pos2::new(1.0, 1.0), false),
            (Pos2::new(1.0, 1.0), Pos2::new(200.0, 200.0), true),
        ] {
            let ctx = egui::Context::default();
            let app = ImageApp::from_context(ctx.clone(), "chrome-test".into(), None, None);
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 550.0))),
                    focused: true,
                    events: vec![
                        egui::Event::PointerMoved(origin),
                        egui::Event::PointerButton {
                            pos: origin,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            modifiers: egui::Modifiers::NONE,
                        },
                        egui::Event::PointerMoved(destination),
                    ],
                    ..Default::default()
                },
                |ui| app.window_edges(ui.ctx()),
            );
            assert_eq!(
                output.viewport_output[&egui::ViewportId::ROOT]
                    .commands
                    .iter()
                    .any(|c| matches!(c, egui::ViewportCommand::BeginResize(_))),
                expected
            );
            output.textures_delta.clear();
        }
    }
}
