use super::*;
impl ImageApp {
    pub(super) fn update_settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("应用与组件更新",|ui|{
            let mut automatic=self.updater.preferences.automatic_app_updates;
            if ui.checkbox(&mut automatic,"自动更新应用").changed(){self.updater.set_automatic_app(automatic);}
            ui.small("开启后允许稳定频道自动检查、下载，并在您正常退出且工作安全保存后静默安装。默认关闭；不会强制关闭窗口或重启系统。");
            ui.small("当前发布策略：HTTPS + TUF 签名验证；EXE 暂无 Authenticode 签名，Windows 可能仍提示未知发布者。不会绕过系统保护。");
            let mut models=self.updater.preferences.automatic_installed_model_updates;
            if ui.checkbox(&mut models,"自动更新已安装模型组件").changed(){self.updater.set_model_updates(models); if !models { self.model_updater.pause(); } else { self.model_updater.request(); }}
            ui.small("模型更新独立默认关闭，不安装新模型，不开启实验筛查，也不授予原图上传权限。");
            if models {
                ui.checkbox(&mut self.model_updater.allow_metered_once,"允许本次在计费或费用未知网络下载模型组件");
                if self.model_updater.is_busy() { if ui.button("暂停模型更新").clicked() { self.model_updater.pause(); } }
                else if ui.button("检查并更新已安装模型组件").clicked() { self.model_updater.request(); }
            }
            ui.small(&self.model_updater.status);
            let mut metered=self.updater.preferences.allow_metered_metadata;
            if ui.checkbox(&mut metered,"计费网络上允许检查小型更新元数据").changed(){self.updater.set_metered_metadata(metered);}
            ui.small("计费网络上的安装包和模型下载仍需另行允许；低电量、节能或处理繁忙时暂停。");
            if ui.add_enabled(!matches!(self.updater.state, light_image_tool::updater::client::State::Ready { .. }), egui::Button::new("检查更新")).on_disabled_hover_text("已有校验完成的更新，请先取消或正常退出安装").clicked(){self.updater.manual_check();}
            if matches!(self.updater.state, light_image_tool::updater::client::State::Available { .. } | light_image_tool::updater::client::State::Paused(_)) {
                ui.checkbox(&mut self.updater.allow_metered_once, "允许本次在计费或费用未知网络下载");
                if ui.button("下载并在正常退出后更新").clicked() {
                    let _ = self.updater.begin_download(light_image_tool::updater::client::Authorization::ExplicitApp, self.work.is_some() || self.ai.is_busy());
                }
            }
            if matches!(self.updater.state, light_image_tool::updater::client::State::Downloading { .. }) && ui.button("暂停下载").clicked() { self.updater.pause(); }
            if matches!(self.updater.state, light_image_tool::updater::client::State::Downloading { .. } | light_image_tool::updater::client::State::Paused(_) | light_image_tool::updater::client::State::Ready { .. }) && ui.button("取消本次更新").clicked() { self.updater.cancel(); }
            ui.label(self.updater.state.text());
            ui.small("手动检查不会自动下载。暂停保留临时包，取消只清理本产品临时下载。安装提交开始后不会杀掉安装器。");
            if let Some(error)=&self.updater.settings_error{ui.colored_label(crate::app_theme::palette(ui.ctx()).error,error);}
        });
    }
}
