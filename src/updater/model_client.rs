//! Independent model-update consent and scheduling. No model is installed merely
//! because the user enabled the application updater.
use super::{
    client::Client,
    model_update::{ModelJob, PreparedModelUpdate},
};
use crate::ai::Controller;
use std::path::PathBuf;

pub struct ModelClient {
    job: Option<ModelJob>,
    prepared: Option<PreparedModelUpdate>,
    pub status: String,
    pub allow_metered_once: bool,
    next_check: u64,
    failures: u8,
    paused: bool,
    cancelled: bool,
    requested: bool,
    active_metered: bool,
    resource_checked: Option<u64>,
}
impl Default for ModelClient {
    fn default() -> Self {
        Self {
            job: None,
            prepared: None,
            status: "模型自动更新已关闭".into(),
            allow_metered_once: false,
            next_check: 60,
            failures: 0,
            paused: false,
            cancelled: false,
            requested: false,
            active_metered: false,
            resource_checked: None,
        }
    }
}
impl ModelClient {
    pub fn is_busy(&self) -> bool {
        self.job.is_some()
    }
    pub fn request(&mut self) {
        self.paused = false;
        self.requested = true;
    }
    pub fn pause(&mut self) {
        if let Some(job) = &self.job {
            job.pause();
        }
        if self.prepared.take().is_some() {
            self.job.take();
        }
        self.paused = true;
        self.cancelled = true;
        self.status = "模型更新已暂停，保留经验证绑定的下载片段；旧组件仍可使用".into();
    }
    pub fn shutdown(&mut self) {
        if let Some(job) = &self.job {
            job.cancel();
        }
        self.prepared.take();
        self.job.take();
    }
    pub fn poll(&mut self, client: &Client, ai: &mut Controller, elapsed: u64, heavy: bool) {
        let allowed = client.preferences.automatic_installed_model_updates;
        if allowed
            && self.job.is_some()
            && self.prepared.is_none()
            && self.resource_checked != Some(elapsed)
        {
            self.resource_checked = Some(elapsed);
            if let Some((_, root)) = client.model_context() {
                let policy = super::platform::download_resources(&root, heavy || ai.is_busy())
                    .and_then(|r| r.transfer_permission(self.active_metered));
                if let Err(error) = policy {
                    if let Some(job) = &self.job {
                        job.pause();
                    }
                    self.cancelled = true;
                    self.next_check = elapsed.saturating_add(30);
                    self.status = format!("模型更新等待资源恢复：{error}");
                }
            }
        }

        if !allowed {
            if let Some(job) = &self.job {
                job.cancel();
                self.cancelled = true;
            }
            if self.prepared.take().is_some() {
                self.job.take();
                self.cancelled = false;
            }
            self.requested = false;
            self.status = "模型自动更新已关闭".into();
        }
        if let Some(result) = self.job.as_ref().and_then(|j| j.receiver.try_recv().ok()) {
            if self.cancelled || !allowed {
                self.prepared.take();
                self.job.take();
                self.cancelled = false;
            } else {
                match result {
                    Ok(Some(prepared)) => {
                        self.status = "模型包已验证，等待当前检测任务结束后切换".into();
                        self.prepared = Some(prepared);
                    }
                    Ok(None) => {
                        self.status = "没有适用的模型组件更新".into();
                        self.failures = 0;
                        self.job.take();
                    }
                    Err(error) => {
                        self.failures = self.failures.saturating_add(1).min(9);
                        self.next_check =
                            elapsed.saturating_add((30u64 << self.failures).min(4 * 60 * 60));
                        self.status = format!("模型更新暂缓：{error}");
                        self.job.take();
                    }
                }
            }
        }
        if allowed
            && !heavy
            && !ai.is_busy()
            && let Some(prepared) = self.prepared.take()
        {
            self.status = match ai.activate_prepared_model(prepared, allowed) {
                Ok(()) => {
                    self.failures = 0;
                    "模型组件已切换并通过自检；检测仍为实验性，手动结论已保留".into()
                }
                Err(error) => format!("模型组件未切换，保留原组件：{error}"),
            };
            self.job.take();
        }
        if !allowed || self.job.is_some() || self.paused {
            return;
        }
        let Some((config, update_root)) = client.model_context() else {
            self.status = super::PRODUCTION_DISABLED_REASON.into();
            return;
        };
        let Some(installed) = ai.installed_model_for_update() else {
            self.status = "没有已安装且通过自检的模型组件；不会自动安装新组件".into();
            return;
        };
        let Some(model_root): Option<PathBuf> = ai.model_update_root() else {
            return;
        };
        let explicit = self.requested;
        if !explicit && elapsed < self.next_check {
            return;
        }
        if heavy || ai.is_busy() {
            self.status = "模型更新等待当前处理任务结束".into();
            return;
        }
        if super::platform::network_is_metered_or_unknown()
            && !client.preferences.allow_metered_metadata
            && !explicit
        {
            self.status = "模型更新等待非计费网络".into();
            return;
        }
        self.requested = false;
        self.cancelled = false;
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |t| u64::from(t.subsec_nanos() % 3601));
        self.next_check = elapsed.saturating_add(24 * 60 * 60 + jitter);
        self.status = "正在检查、下载并验证兼容模型组件；现有组件继续可用".into();
        self.active_metered = self.allow_metered_once;
        self.job = Some(ModelJob::start(
            update_root,
            model_root,
            config,
            installed,
            self.allow_metered_once,
            heavy,
        ));
        self.allow_metered_once = false;
    }
}
impl Drop for ModelClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_do_not_authorize_or_schedule_an_update() {
        let client = ModelClient::default();
        assert!(!client.is_busy());
        assert!(!client.allow_metered_once);
        assert!(!client.requested);
        assert_eq!(client.next_check, 60);
    }
    #[test]
    fn paused_model_job_needs_explicit_resume() {
        let mut client = ModelClient::default();
        client.pause();
        assert!(client.paused);
        client.request();
        assert!(!client.paused);
        assert!(client.requested);
    }
}
