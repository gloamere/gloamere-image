use super::*;

impl ImageApp {
    pub(super) fn icon_size_selector(&mut self, ui: &mut egui::Ui) {
        let Some(icon) = self.loaded.as_ref().and_then(|image| image.icon.clone()) else {
            return;
        };
        ui.label(format!("图标尺寸 · {} 种", icon.entries.len()));
        let mut selected = icon.selected_index;
        let label = |index: usize| {
            let entry = &icon.entries[index];
            format!(
                "{} × {} · {} 位",
                entry.width, entry.height, entry.bit_depth
            )
        };
        ui.add_enabled_ui(!self.loading && self.work.is_none(), |ui| {
            let menu = egui::ComboBox::from_id_salt("icon-size")
                .selected_text(label(selected))
                .show_ui(ui, |ui| {
                    for entry in &icon.entries {
                        let choice = ui.selectable_value(
                            &mut selected,
                            entry.index,
                            format!("{} · #{}", label(entry.index), entry.index + 1),
                        );
                        #[cfg(test)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new(("icon-choice", entry.index)),
                                choice.rect,
                            )
                        });
                        #[cfg(not(test))]
                        let _ = choice;
                    }
                });
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(egui::Id::new("icon-size-trigger"), menu.response.rect)
            });
            menu.response.on_hover_text("选择图标文件中保存的尺寸");
        });
        ui.small("复制和导出使用当前选中的图标尺寸");
        if selected != icon.selected_index {
            self.select_icon_entry(selected);
        }
    }
}
