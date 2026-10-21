use super::*;
use light_image_tool::settings::Appearance;

impl ImageApp {
    pub(super) fn more_menu_contents(
        &mut self,
        ui: &mut egui::Ui,
        ready: bool,
        newly_opened: bool,
        ctx: &egui::Context,
    ) -> bool {
        let colors = crate::app_theme::palette(ui.ctx());
        let mut close = false;
        ui.horizontal(|ui| {
            ui.add(egui::Image::new(&self.brand_texture).fit_to_exact_size(Vec2::splat(22.0)));
            ui.strong(viewer_ui::PRODUCT_NAME);
        });
        close |= self.ai_detection_menu_entry(ui);
        let path = ui.add(ActionButton::new(Icon::Open, "通过路径打开…").menu());
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
            ActionButton::new(
                Icon::Crop,
                if self.playback.is_some() {
                    "提取当前帧为静态副本"
                } else {
                    "裁剪 / 缩放"
                },
            )
            .selected(self.editing)
            .menu(),
        );
        #[cfg(test)]
        {
            self.menu_edit_rect = Some(edit.rect);
        }
        if edit.clicked() {
            if self.playback.is_some() {
                self.extract_dialog = true;
            } else {
                self.editing = !self.editing;
            }
            self.info_open = false;
            close = true;
        }
        ui.separator();
        ui.collapsing("文件", |ui| {
            let file = self.current_file_path().is_some() && self.work.is_none();
            if ui
                .add_enabled(
                    file,
                    ActionButton::new(Icon::Refresh, "重新载入当前图片 · F5").menu(),
                )
                .clicked()
            {
                self.reload_current();
                close = true;
            }
            if ui
                .add_enabled(file, ActionButton::new(Icon::Open, "在文件夹中显示").menu())
                .clicked()
            {
                self.show_current_in_folder(ctx);
                close = true;
            }
            if ui
                .add_enabled(file, ActionButton::new(Icon::Copy, "复制文件路径").menu())
                .clicked()
            {
                self.copy_path(ctx);
                close = true;
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
                self.copy_image(ctx);
                close = true;
            }
            if ui
                .add_enabled(
                    !self.loading && self.work.is_none(),
                    ActionButton::new(Icon::Paste, "从剪贴板打开 · Ctrl+V").menu(),
                )
                .clicked()
            {
                self.request_intent(PendingIntent::Clipboard, ctx);
                close = true;
            }
            if ui
                .add_enabled(
                    ready,
                    ActionButton::new(Icon::Export, "导出新副本 · Ctrl+Shift+S").menu(),
                )
                .clicked()
            {
                self.begin_export();
                close = true;
            }
        });
        ui.collapsing("查看", |ui| {
            ui.collapsing("素材排序与筛选", |ui| {
                self.browser_sort_filter_ui(ui)
            });
            let alpha = !self.comparison.active()
                && self.playback.is_none()
                && !self.loading
                && self.work.is_none();
            if self.inspector.menu(ui, self.pixels.as_ref(), alpha) {
                self.refresh_texture(ctx);
                self.refresh_pinned_sampler();
            }
            ui.collapsing("双图 A/B 对比", |ui| {
                close |= self.comparison_menu(ui);
            });
            if ui
                .add_enabled(
                    self.canvas_actions_ready(),
                    ActionButton::new(Icon::Pin, "贴图置顶（独立参考窗）").menu(),
                )
                .on_hover_text(if self.playback.is_some() {
                    "冻结当前动画帧为独立参考窗，原动画继续播放。"
                } else {
                    "创建独立置顶参考窗，继续在主窗口浏览其他图片。"
                })
                .clicked()
            {
                self.create_sticker(ctx);
                close = true;
            }
            let mut top = self.reference_on_top;
            if ui.checkbox(&mut top, "当前窗口置顶").changed() {
                self.set_reference_on_top(top, ctx);
                close = true;
            }
            ui.small("当前窗口置顶保留窗口大小和位置，不创建独立贴图。");
            if ui
                .add_enabled(
                    ready,
                    ActionButton::new(Icon::FitWidth, "长图适应宽度")
                        .selected(self.long_image)
                        .menu(),
                )
                .clicked()
            {
                self.enter_long_image();
                close = true;
            }
            if ui
                .add_enabled(
                    ready,
                    ActionButton::new(Icon::Actual, "实际像素 1:1 · 1").menu(),
                )
                .clicked()
            {
                self.actual_pixels();
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
                self.toggle_fullscreen(ctx);
                close = true;
            }
        });
        ui.collapsing("编辑", |ui| {
            if ui
                .add_enabled(
                    ready && !self.undo.is_empty(),
                    ActionButton::new(Icon::Undo, "撤销 · Ctrl+Z").menu(),
                )
                .clicked()
            {
                self.undo_edit(ctx);
                close = true;
            }
            if ui
                .add_enabled(
                    ready && !self.redo.is_empty(),
                    egui::Button::new("重做 · Ctrl+Y / Ctrl+Shift+Z"),
                )
                .clicked()
            {
                self.redo_edit(ctx);
                close = true;
            }
            if ui
                .add_enabled(
                    ready,
                    ActionButton::new(Icon::Rotate, "右转 90° · R").menu(),
                )
                .clicked()
            {
                self.start_edit(EditOp::Rotate90, ctx);
                close = true;
            }
            if ui
                .add_enabled(
                    ready,
                    ActionButton::new(Icon::RotateLeft, "左转 90° · Shift+R").menu(),
                )
                .clicked()
            {
                self.start_edit(EditOp::Rotate270, ctx);
                close = true;
            }
        });
        let appearance_menu = ui.menu_button("外观", |ui| {
            for (mode, label) in [
                (Appearance::System, "跟随系统"),
                (Appearance::Light, "浅色"),
                (Appearance::Dark, "深色"),
            ] {
                let choice = ui.selectable_label(self.preferences.appearance == mode, label);
                #[cfg(test)]
                ui.ctx().data_mut(|data| {
                    data.insert_temp(egui::Id::new(("appearance-choice", label)), choice.rect)
                });
                if choice.clicked() {
                    self.set_appearance(ui.ctx(), mode);
                    close = true;
                    ui.close();
                }
            }
        });
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("appearance-menu-trigger"),
                appearance_menu.response.rect,
            )
        });
        appearance_menu
            .response
            .on_hover_text("跟随系统，或为查看器单独选择浅色、深色");
        ui.collapsing("高级与设置", |ui| {
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
            ui.collapsing("AI 筛选与本地检查", |ui| {
                self.ai_filter_choices(ui);
                ui.separator();
                self.ai_settings_ui(ui);
            });
            ui.collapsing("应用与组件更新", |ui| self.update_settings_ui(ui));
            ui.label("画布背景");
            egui::Grid::new("canvas-background-menu")
                .num_columns(2)
                .spacing(Vec2::new(8.0, 4.0))
                .show(ui, |ui| {
                    for (index, background) in CanvasBackground::ALL.into_iter().enumerate() {
                        if ui
                            .add_sized(
                                [124.0, 28.0],
                                egui::Button::new(background.label())
                                    .frame(false)
                                    .selected(self.preferences.background == background),
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
            ui.small("背景、通道查看和取色工具均不改变导出像素。");
        });
        ui.separator();
        ui.small(&self.status);
        if let Some(warning) = &self.settings_warning {
            ui.colored_label(colors.warning, warning);
        }
        ui.small("原图只读 · 仅本地处理");
        ui.add_space(8.0);
        close
    }
}
