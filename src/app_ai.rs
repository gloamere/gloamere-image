//! v0.7 view state only. Evidence, identity and persistence belong to the AI core.
use super::*;
use light_image_tool::ai::{
    policy::{Filter, FilterCounts},
    types::{ComponentState, Decision, ManualDecision, ReleaseTier, RunState},
};
use light_image_tool::browsing::{FileMetadata, FormatFilter, SortOrder};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClearRecords {
    Automatic,
    Manual,
    Model,
}

pub(super) struct AiView {
    pub filter: Filter,
    pub include_suspected: bool,
    pub search: String,
    pub rows: Vec<usize>,
    pub counts: FilterCounts,
    pub scope_len: usize,
    pub sort: SortOrder,
    pub format: FormatFilter,
    pub metadata: HashMap<PathBuf, FileMetadata>,
    row_counts: Vec<FilterCounts>,
    order_ranks: Vec<usize>,
    #[cfg(test)]
    full_rebuilds: usize,
    pub confirm: Option<ClearRecords>,
    pub settings_open: bool,
    #[cfg(test)]
    pub manual_buttons: [Option<Rect>; 3],
    #[cfg(test)]
    pub source_button: Option<Rect>,
    #[cfg(test)]
    pub detection_menu_button: Option<Rect>,
    #[cfg(test)]
    pub badge_rect: Option<Rect>,
    #[cfg(test)]
    pub filter_button: Option<Rect>,
    #[cfg(test)]
    pub badge_toggle: Option<Rect>,
    #[cfg(test)]
    pub clear_buttons: [Option<Rect>; 3],
}
impl Default for AiView {
    fn default() -> Self {
        Self {
            filter: Filter::All,
            include_suspected: true,
            search: String::new(),
            rows: Vec::new(),
            counts: FilterCounts::default(),
            scope_len: 0,
            sort: SortOrder::default(),
            format: FormatFilter::default(),
            metadata: HashMap::new(),
            row_counts: Vec::new(),
            order_ranks: Vec::new(),
            #[cfg(test)]
            full_rebuilds: 0,
            confirm: None,
            settings_open: false,
            #[cfg(test)]
            manual_buttons: [None; 3],
            #[cfg(test)]
            source_button: None,
            #[cfg(test)]
            detection_menu_button: None,
            #[cfg(test)]
            badge_rect: None,
            #[cfg(test)]
            filter_button: None,
            #[cfg(test)]
            badge_toggle: None,
            #[cfg(test)]
            clear_buttons: [None; 3],
        }
    }
}

pub(super) fn thumbnail_badge_label<'a>(decision: &Decision, badge: &'a str) -> &'a str {
    if decision.manual_active {
        if decision.bucket == light_image_tool::ai::Bucket::Ai {
            "AI · 手"
        } else {
            "非 · 手"
        }
    } else if badge.contains("实验") {
        "AI? 实"
    } else {
        badge
    }
}

/// Shared with deterministic CPU UI regression fixtures. Component readiness,
/// task execution and release qualification are always separately visible.
fn ai_detection_summary(
    ui: &mut egui::Ui,
    decision: &Decision,
    component: &ComponentState,
    component_label: &str,
    run: &RunState,
    tier: ReleaseTier,
) {
    ui.strong(&decision.label);
    ui.small(format!("组件：{component_label}"));
    ui.small(format!("执行：{}", run.label()));
    ui.small(format!("资格：{}", tier.label()));
    if let ComponentState::Failed(reason) = component {
        ui.small(reason);
    }
    if let RunState::Unsupported(reason)
    | RunState::Failed(reason)
    | RunState::SourceFailed(reason) = run
    {
        ui.small(reason);
    }
    for reason in &decision.reasons {
        ui.small(reason);
    }
}

impl ImageApp {
    pub(super) fn browser_set_metadata(&mut self, metadata: HashMap<PathBuf, FileMetadata>) {
        self.ai_view.metadata = metadata;
        self.ai_rebuild_view();
    }
    pub(super) fn browser_clear_metadata(&mut self) {
        self.ai_view.metadata.clear();
    }
    pub(super) fn browser_sort_filter_ui(&mut self, ui: &mut egui::Ui) {
        let previous = (self.ai_view.sort, self.ai_view.format);
        egui::ComboBox::from_id_salt("browser-sort-order")
            .width(ui.available_width().min(220.0))
            .selected_text(self.ai_view.sort.label())
            .show_ui(ui, |ui| {
                for order in SortOrder::ALL {
                    if ui
                        .selectable_value(&mut self.ai_view.sort, order, order.label())
                        .clicked()
                    {
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("相同属性按自然文件名排序；无法读取属性的文件排在最后");
        egui::ComboBox::from_id_salt("browser-format-filter")
            .width(ui.available_width().min(220.0))
            .selected_text(self.ai_view.format.label())
            .show_ui(ui, |ui| {
                for format in FormatFilter::ALL {
                    if ui
                        .selectable_value(&mut self.ai_view.format, format, format.label())
                        .clicked()
                    {
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("按文件扩展名筛选，与文件名搜索和 AI 状态筛选取交集");
        if previous != (self.ai_view.sort, self.ai_view.format) {
            self.ai_rebuild_view();
        }
    }
    pub(super) fn ai_detection_menu_entry(&mut self, ui: &mut egui::Ui) -> bool {
        let ready = self.pixels.is_some() && !self.loading && self.work.is_none();
        let response = ui.add_enabled(
            ready,
            ActionButton::new(Icon::AiDetect, "AI 检测当前图片").menu(),
        );
        #[cfg(test)]
        {
            self.ai_view.detection_menu_button = Some(response.rect);
        }
        if response.clicked() {
            self.ai_detect_current();
            true
        } else {
            false
        }
    }
    pub(super) fn ai_detect_current(&mut self) {
        if self.loading || self.work.is_some() {
            return;
        }
        self.info_open = true;
        self.editing = false;
        self.more_open = false;
        let Some(path) = self.loaded.as_ref().map(|image| image.path.clone()) else {
            return;
        };
        if self
            .requested_path
            .as_ref()
            .is_some_and(|requested| requested != &path)
        {
            return;
        }
        self.ai.scan_current(&path);
        self.ai_view.settings_open = self.ai.component_state() != &ComponentState::Ready;
        self.ai_rebuild_view();
    }
    pub(super) fn ai_sync_scope(&mut self) {
        self.ai.set_scanned_scope(
            &self.siblings,
            self.requested_path.as_deref(),
            &self.ai_view.metadata,
        );
        self.ai_rebuild_view();
    }
    pub(super) fn ai_rebuild_view(&mut self) {
        #[cfg(test)]
        {
            self.ai_view.full_rebuilds += 1;
        }
        let query = self.ai_view.search.trim().to_lowercase();
        let mut counts = FilterCounts::default();
        let mut rows = Vec::new();
        let mut row_counts = Vec::with_capacity(self.siblings.len());
        for (index, path) in self.siblings.iter().enumerate() {
            let decision = self.ai.decision(path);
            counts.add(&decision);
            let mut contribution = FilterCounts::default();
            contribution.add(&decision);
            row_counts.push(contribution);
            if self
                .ai_view
                .filter
                .matches(&decision, self.ai_view.include_suspected)
                && self.ai_view.format.matches(path)
                && (query.is_empty()
                    || path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&query))
            {
                rows.push(index);
            }
        }
        let mut order: Vec<_> = (0..self.siblings.len()).collect();
        browsing::sort_indices(
            &self.siblings,
            &self.ai_view.metadata,
            &mut order,
            self.ai_view.sort,
        );
        let mut ranks = vec![0; self.siblings.len()];
        for (rank, index) in order.into_iter().enumerate() {
            ranks[index] = rank;
        }
        rows.sort_unstable_by_key(|index| ranks[*index]);
        self.ai_view.rows = rows;
        self.ai_view.row_counts = row_counts;
        self.ai_view.order_ranks = ranks;
        self.ai_view.counts = counts;
        self.ai_view.scope_len = self.siblings.len();
        // No open, position assignment, focus request or scroll request here.
    }
    pub(super) fn ai_refresh_current(&mut self, path: &std::path::Path) {
        let Some(index) = self.siblings.iter().position(|candidate| candidate == path) else {
            return;
        };
        if self.ai_view.scope_len != self.siblings.len()
            || self.ai_view.row_counts.len() != self.siblings.len()
            || self.ai_view.order_ranks.len() != self.siblings.len()
        {
            self.ai_rebuild_view();
            return;
        }
        let decision = self.ai.decision(path);
        let old = self.ai_view.row_counts[index];
        let mut contribution = FilterCounts::default();
        contribution.add(&decision);
        let total = &mut self.ai_view.counts;
        macro_rules! replace_count {
            ($($field:ident),+ $(,)?) => { $(total.$field = total.$field.saturating_sub(old.$field) + contribution.$field;)+ };
        }
        replace_count!(
            all,
            ai,
            unmarked,
            pending,
            error,
            manual,
            suspected,
            source,
            no_signal,
            unsupported
        );
        self.ai_view.row_counts[index] = contribution;
        let query = self.ai_view.search.trim().to_lowercase();
        let visible = self
            .ai_view
            .filter
            .matches(&decision, self.ai_view.include_suspected)
            && self.ai_view.format.matches(path)
            && (query.is_empty()
                || path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&query));
        self.ai_view.rows.retain(|candidate| *candidate != index);
        if visible {
            let rank = self.ai_view.order_ranks[index];
            let insert = self
                .ai_view
                .rows
                .binary_search_by_key(&rank, |candidate| self.ai_view.order_ranks[*candidate])
                .unwrap_or_else(|index| index);
            self.ai_view.rows.insert(insert, index);
        }
    }
    pub(super) fn ai_visible_indices(&self) -> Vec<usize> {
        // The unfiltered default also keeps headless v0.5 fixtures independent of scanner startup.
        if self.ai_view.filter == Filter::All
            && self.ai_view.search.trim().is_empty()
            && self.ai_view.sort == SortOrder::Natural
            && self.ai_view.format == FormatFilter::All
        {
            (0..self.siblings.len()).collect()
        } else {
            self.ai_view.rows.clone()
        }
    }
    pub(super) fn ai_navigation_target(&self, step: isize) -> Option<usize> {
        let rows = self.ai_visible_indices();
        if rows.is_empty() {
            return None;
        }
        if let Some(current) = rows.iter().position(|index| *index == self.position) {
            Some(rows[(current as isize + step).rem_euclid(rows.len() as isize) as usize])
        } else if step < 0 {
            let current_rank = self
                .ai_view
                .order_ranks
                .get(self.position)
                .copied()
                .unwrap_or(self.position);
            rows.iter()
                .rev()
                .find(|index| {
                    self.ai_view
                        .order_ranks
                        .get(**index)
                        .copied()
                        .unwrap_or(**index)
                        < current_rank
                })
                .or(rows.last())
                .copied()
        } else {
            let current_rank = self
                .ai_view
                .order_ranks
                .get(self.position)
                .copied()
                .unwrap_or(self.position);
            rows.iter()
                .find(|index| {
                    self.ai_view
                        .order_ranks
                        .get(**index)
                        .copied()
                        .unwrap_or(**index)
                        > current_rank
                })
                .or(rows.first())
                .copied()
        }
    }
    pub(super) fn ai_select_boundary(&mut self, last: bool) {
        let rows = self.ai_visible_indices();
        if let Some(index) = if last { rows.last() } else { rows.first() } {
            self.select_image(*index);
        }
    }
    pub(super) fn ai_current_excluded(&self) -> bool {
        self.requested_path.as_ref().is_some_and(|path| {
            let decision = self.ai.decision(path);
            let query = self.ai_view.search.trim().to_lowercase();
            !self
                .ai_view
                .filter
                .matches(&decision, self.ai_view.include_suspected)
                || (!query.is_empty()
                    && !path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&query))
                || !self.ai_view.format.matches(path)
        })
    }
    pub(super) fn ai_poll(&mut self, ctx: &egui::Context) {
        if self.ai.poll() {
            self.ai_rebuild_view();
            ctx.request_repaint();
        }
        if self.ai.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    pub(super) fn ai_filter_choices(&mut self, ui: &mut egui::Ui) {
        if self.ai_view.scope_len != self.siblings.len() {
            self.ai_rebuild_view();
        }
        let mut changed = false;
        for filter in Filter::ALL {
            let visible = self
                .ai_view
                .counts
                .visible_count(filter, self.ai_view.include_suspected);
            let total = self.ai_view.counts.count(filter);
            let label = if visible == total {
                format!("{}  {}", filter.label(), total)
            } else {
                format!("{}  {}（总计 {}）", filter.label(), visible, total)
            };
            if ui
                .selectable_label(self.ai_view.filter == filter, label)
                .clicked()
            {
                self.ai_view.filter = filter;
                changed = true;
                ui.close();
            }
        }
        ui.separator();
        changed |= ui
            .checkbox(&mut self.ai_view.include_suspected, "AI 筛选包含疑似")
            .changed();
        ui.small("来源、疑似、未见强信号、不支持与人工标记可分别筛选。未完成不计为已检测；无信号不能排除 AI。");
        if changed {
            self.ai_rebuild_view();
        }
    }
    pub(super) fn ai_list_header(&mut self, ui: &mut egui::Ui) {
        if self.ai_view.scope_len != self.siblings.len() {
            self.ai_rebuild_view();
        }
        if ui
            .add_sized(
                [ui.available_width(), 30.0],
                egui::TextEdit::singleline(&mut self.ai_view.search)
                    .id(egui::Id::new("ai-folder-search"))
                    .hint_text("搜索文件名")
                    .margin(egui::Margin::symmetric(8, 6))
                    .desired_width(ui.available_width()),
            )
            .changed()
        {
            self.ai_rebuild_view();
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui
                .add(
                    CompactAction::new(Icon::Locate, "定位当前图片")
                        .help("定位当前图片：滚动到当前图片"),
                )
                .clicked()
            {
                self.locate_current = true;
            }
            if ui
                .add(
                    CompactAction::new(Icon::Refresh, "刷新图片列表")
                        .help("刷新图片列表：重新读取目录和缩略图"),
                )
                .clicked()
            {
                self.refresh_browser();
            }
            let filtered = self.ai_view.filter != Filter::All
                || self.ai_view.sort != SortOrder::default()
                || self.ai_view.format != FormatFilter::default();
            let filter = ui.add(
                CompactAction::new(Icon::Filter, "AI 状态筛选")
                    .selected(filtered)
                    .help("筛选与排序：文件排序、图片格式和 AI 状态"),
            );
            #[cfg(test)]
            {
                self.ai_view.filter_button = Some(filter.rect);
            }
            egui::Popup::menu(&filter).width(260.0).show(|ui| {
                self.browser_sort_filter_ui(ui);
                ui.separator();
                ui.weak("AI 状态");
                self.ai_filter_choices(ui);
                ui.separator();
                ui.small(self.ai.status());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.ai_view.filter == Filter::All {
                    "筛选与排序"
                } else {
                    self.ai_view.filter.label()
                };
                ui.add(egui::Label::new(egui::RichText::new(label).small().weak()).truncate());
            });
        });
        if self.ai.is_busy() {
            ui.add(egui::Label::new(egui::RichText::new(self.ai.status()).small()).truncate())
                .on_hover_text(self.ai.status());
        }
        if self.ai_current_excluded() {
            ui.small("当前图片不在筛选结果中。");
        }
        if self.ai_visible_indices().is_empty() {
            ui.small("没有匹配图片，可清除搜索或筛选。");
        }
    }
    pub(super) fn ai_settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong(self.ai.model_status());
        ui.small(self.ai.release_tier().label());
        if let ComponentState::Failed(reason) = self.ai.component_state() {
            ui.small(reason);
        }
        if let Some(info) = self.ai.component_info() {
            ui.collapsing("已核验组件信息", |ui| {
                ui.small(format!(
                    "模型：{} · ONNX Runtime {}",
                    info.model_id, info.runtime_version
                ));
                ui.small(format!("实际安装文件：{} 字节", info.installed_bytes));
                ui.small(format!("来源：CoDE，修订 {}", info.source_revision));
                ui.small(info.license);
            });
        }
        ui.small("本地处理，不上传原图。实验像素范围：不透明 8 位 JPEG / PNG；仅中心 224×224，非整幅覆盖。透明、动画、多页及超限输入不支持。来源检查独立。");
        ui.small("尚无已配置的可信在线分发源；可导入完整离线包。校验清单、许可、哈希和固定自检后才启用。");
        if ui
            .button("离线导入实验模型组件…")
            .on_hover_text(
                "选择已核验的完整组件文件夹；后台检查许可、清单及哈希，不下载或执行包内程序",
            )
            .clicked()
            && let Some(path) = rfd::FileDialog::new()
                .set_title("选择本地 AI 组件文件夹")
                .pick_folder()
        {
            match self.ai.import_model_pack(&path) {
                Ok(()) => {
                    self.status = "正在后台校验并导入本地模型组件…".into();
                    self.notice_until = Some(Instant::now() + std::time::Duration::from_secs(4));
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        if ui.button("删除本机模型组件…").clicked() {
            self.ai_view.confirm = Some(ClearRecords::Model);
            self.more_open = false;
        }

        ui.small("默认不进行 AI 检测。需要时选择“更多 → AI 检测当前图片”；打开、翻图和刷新列表不会触发文件夹扫描。");
        ui.small("组件已安装时首次点击会先异步核验，再继续本次检测。实验结果可能误报或漏报，未检测与未见强信号均不等于非 AI。");
        let mut badges = self.ai.settings().show_badges;
        let mut persist = self.ai.settings().persist_results;
        let response = ui.checkbox(&mut badges, "显示 AI 角标");
        #[cfg(test)]
        {
            self.ai_view.badge_toggle = Some(response.rect);
        }
        if response.changed() {
            self.ai.set_show_badges(badges);
        }
        ui.small("角标仅叠加显示；筛选、详情、复制和导出独立。");
        if ui.checkbox(&mut persist, "保存本地检测结果").changed() {
            self.ai.set_persist_results(persist);
        }
        ui.small("关闭保存不清除既有记录；人工决定单独保存（缓存不可用时仅限本次会话）。清除范围包括本机其他文件夹的记录。");
        ui.small(self.ai.status());
        if ui.button("清除本机检测结果…").clicked() {
            self.ai_view.confirm = Some(ClearRecords::Automatic);
            self.more_open = false;
        }
        if ui.button("清除本机所有人工标记…").clicked() {
            self.ai_view.confirm = Some(ClearRecords::Manual);
            self.more_open = false;
        }
    }
    pub(super) fn ai_details(&mut self, ui: &mut egui::Ui) {
        let colors = crate::app_theme::palette(ui.ctx());
        ui.heading("AI 检测");
        let path = self.loaded.as_ref().map(|image| image.path.clone());
        if let Some(path) = path {
            let decision = self.ai.decision(&path);
            ai_detection_summary(
                ui,
                &decision,
                self.ai.component_state(),
                self.ai.model_status(),
                &self.ai.run_state(&path),
                self.ai.release_tier(),
            );
            if self.edited {
                ui.small("此记录属于磁盘原图，不代表当前编辑副本。");
            }
            if self.playback.is_some() {
                ui.small("不以动画单帧判定整个动画；动画暂不支持像素检测。");
            }
            if decision.conflict {
                ui.colored_label(
                    colors.warning,
                    "人工决定或自动证据存在冲突；下方保留各项原始依据。",
                );
            }
            if let Some(record) = self.ai.record(&path) {
                if record.checked_unix_seconds == 0 {
                    ui.small("尚未取得检查记录");
                } else {
                    ui.small(format!(
                        "本次来源检查 UTC Unix：{}",
                        record.checked_unix_seconds
                    ));
                }
                for evidence in &record.evidence {
                    ui.label(&evidence.summary);
                    ui.small(format!(
                        "提供方：{} · {}",
                        evidence.provider,
                        if evidence.authenticated {
                            "认证条件通过"
                        } else {
                            "非认证来源信息"
                        }
                    ));
                }
                if let Some(model) = &record.model {
                    if let Some(time) = record.pixel_checked_unix_seconds {
                        ui.small(format!("像素结果时间 UTC Unix：{time}"));
                    }
                    ui.collapsing("像素模型技术记录", |ui| {
                        ui.small(format!("模型：{}", model.fingerprint));
                        if !model.approved {
                            ui.colored_label(
                                colors.warning,
                                "模型尚未通过正式发布的验证标准；结果仅为实验信号，不能证明来源。",
                            );
                        }
                        ui.small(format!("输入范围：{}", model.input_scope));
                        ui.small(format!("检测分值：{}", model.raw_score));
                        if model.approved {
                            ui.small(format!("发布阈值：{}", model.threshold));
                        } else {
                            ui.small("研究分界：0；保持原候选边界，未经产品质量校准。");
                        }
                        ui.small("检测分值不是概率；不能证明来源或画面内容真实。");
                    });
                }
            }
            if let Some(mark) = self.ai.manual(&path) {
                ui.small(format!(
                    "人工记录：{} · UTC Unix {}{}",
                    if mark.decision == ManualDecision::Ai {
                        "AI"
                    } else {
                        "非 AI"
                    },
                    mark.created_unix_seconds,
                    if decision.manual_active {
                        ""
                    } else {
                        " · 已过期，需重新确认"
                    }
                ));
            }
            let source = ui.add_enabled(
                !self.loading
                    && self.work.is_none()
                    && self
                        .requested_path
                        .as_ref()
                        .is_none_or(|requested| requested == &path),
                ActionButton::new(Icon::Info, "读取来源信息").menu(),
            );
            #[cfg(test)]
            {
                self.ai_view.source_button = Some(source.rect);
            }
            if source.clicked() {
                self.ai.inspect_source_current(&path);
                self.ai_rebuild_view();
            }
            ui.small("仅读取当前文件的来源信息并确认文件身份，之后可添加人工标记；此操作不运行像素模型。");
            self.ai_manual_controls(ui, &path);
            if ui
                .add_enabled(
                    !self.loading,
                    ActionButton::new(Icon::AiDetect, "检测当前图片").menu(),
                )
                .clicked()
            {
                self.ai_detect_current();
            }
        } else {
            ui.small("临时剪贴板图像或提取帧没有原文件来源记录；当前未提供临时像素检测。");
        }
        ui.small("无信号不等于非 AI；元数据可伪造，来源信息不证明画面内容真实。");
        if ui.button("模型组件与设置").clicked() {
            self.ai_view.settings_open = true;
        }
        let open_settings = self.ai_view.settings_open;
        egui::CollapsingHeader::new("模型组件与本地设置")
            .open(open_settings.then_some(true))
            .show(ui, |ui| {
                if open_settings {
                    ui.scroll_to_cursor(Some(egui::Align::TOP));
                }
                self.ai_settings_ui(ui);
                ui.separator();
                ui.collapsing("筛选分类", |ui| self.ai_filter_choices(ui));
            });
        self.ai_view.settings_open = false;
    }
    pub(super) fn ai_manual_controls(&mut self, ui: &mut egui::Ui, path: &std::path::Path) {
        ui.small("人工纠正仅用于本机整理，保留自动证据。");
        for (index, (label, choice)) in [
            ("标记为 AI", Some(ManualDecision::Ai)),
            ("标记为非 AI", Some(ManualDecision::NotAi)),
            ("撤销人工标记", None),
        ]
        .into_iter()
        .enumerate()
        {
            let enabled = !self.loading
                && (if choice.is_none() {
                    self.ai.manual(path).is_some()
                } else {
                    self.ai
                        .record(path)
                        .is_some_and(|record| record.identity.has_content_hash())
                });
            let response = ui.add_enabled(enabled, egui::Button::new(label));
            #[cfg(test)]
            {
                self.ai_view.manual_buttons[index] = Some(response.rect);
            }
            #[cfg(not(test))]
            let _ = index;
            if response.clicked() {
                if let Err(error) = self.ai.mark(path, choice) {
                    self.error = Some(error.to_string());
                }
                self.ai_rebuild_view();
            }
        }
    }
    pub(super) fn ai_clear_dialog(&mut self, ctx: &egui::Context) {
        let Some(kind) = self.ai_view.confirm else {
            return;
        };
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Modal::new(egui::Id::new("ai-clear-confirmation"))
            .show(ctx, |ui| {
                ui.set_width((ctx.viewport_rect().width() - 48.0).clamp(240.0, 390.0));
                ui.horizontal(|ui| {
                    ui.strong("清除本地 AI 记录");
                    let response = ui.add(CompactAction::new(Icon::Close, "取消清除 AI 记录"));
                    #[cfg(test)] { self.ai_view.clear_buttons[0] = Some(response.rect); }
                    cancelled |= response.clicked();
                });
                ui.label(match kind {
                    ClearRecords::Automatic => "清除本机所有文件夹保存的检测结果。保留人工标记和原图；可按需重新检测。",
                    ClearRecords::Manual => "清除本机所有文件夹的人工 AI / 非 AI 标记，包括过期待确认记录。此操作不可撤销。保留自动证据和原图。",
                    ClearRecords::Model => "删除本机安装的像素模型组件并清除本次会话模型分值。保留人工标记、独立来源证据及原图；之后可重新离线导入。",
                });
                if kind == ClearRecords::Model && let Some(info) = self.ai.component_info() { ui.small(format!("将释放已核验组件文件 {} 字节", info.installed_bytes)); }
                ui.horizontal_wrapped(|ui| {
                    let cancel = ui.button("取消");
                    let confirm = ui.button("确认清除");
                    #[cfg(test)] { self.ai_view.clear_buttons[1] = Some(cancel.rect); self.ai_view.clear_buttons[2] = Some(confirm.rect); }
                    cancelled |= cancel.clicked();
                    confirmed = confirm.clicked();
                });
            });
        if confirmed {
            let result = match kind {
                ClearRecords::Automatic => self.ai.clear_automatic(),
                ClearRecords::Manual => self.ai.clear_manual(),
                ClearRecords::Model => self.ai.remove_model(),
            };
            if let Err(error) = result {
                self.error = Some(error.to_string());
            }
            self.ai_rebuild_view();
        }
        if confirmed || cancelled {
            self.ai_view.confirm = None;
        }
    }
    pub(super) fn ai_canvas_overlay(&mut self, ui: &mut egui::Ui, area: Rect) {
        let colors = crate::app_theme::palette(ui.ctx());
        #[cfg(test)]
        {
            self.ai_view.badge_rect = None;
        }
        if self.loading || self.canvas_unavailable || self.pixels.is_none() {
            return;
        }
        let Some(path) = self.loaded.as_ref().map(|image| image.path.clone()) else {
            return;
        };
        if self
            .requested_path
            .as_ref()
            .is_some_and(|requested| requested != &path)
        {
            return;
        }
        if self.ai.settings().show_badges {
            let decision = self.ai.decision(&path);
            if let Some(badge) = &decision.badge {
                let badge = if self.edited {
                    format!("原图 · {badge}")
                } else {
                    badge.clone()
                };
                let rect = viewer_ui::paint_ai_badge(
                    ui.painter(),
                    area.left_top() + Vec2::splat(16.0),
                    &badge,
                    decision.suspected,
                    decision.manual_active,
                    true,
                );
                let response =
                    ui.interact(rect, egui::Id::new("ai-canvas-badge"), egui::Sense::click());
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        format!("{} · 查看 AI 与来源详情", decision.label),
                    )
                });
                if response.has_focus() {
                    ui.painter().rect_stroke(
                        rect,
                        5.0,
                        egui::Stroke::new(2.0, colors.accent),
                        egui::StrokeKind::Inside,
                    );
                }
                #[cfg(test)]
                {
                    self.ai_view.badge_rect = Some(rect);
                }
                if response
                    .on_hover_text(format!(
                        "{} · 点击查看依据。角标不会写入图像。",
                        decision.label
                    ))
                    .clicked()
                {
                    self.info_open = true;
                    self.editing = false;
                }
            }
        }
        if self.ai_current_excluded() {
            let rect = Rect::from_min_size(
                area.left_bottom() + Vec2::new(12.0, -70.0),
                Vec2::new((area.width() - 24.0).clamp(0.0, 400.0), 34.0),
            );
            ui.painter().rect_filled(rect, 5.0, colors.popover);
            ui.put(
                rect,
                egui::Label::new("已不符合当前筛选 · 主动翻图时进入结果集").truncate(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn app_in_unscanned(
        directory: &Path,
        names: &[&str],
    ) -> (ImageApp, egui::Context, Vec<PathBuf>) {
        let paths: Vec<_> = names.iter().map(|name| directory.join(name)).collect();
        for (index, path) in paths.iter().enumerate() {
            let pixels =
                image::RgbaImage::from_pixel(8, 6, image::Rgba([index as u8, 30, 50, 255]));
            if FormatFilter::Jpeg.matches(path) {
                image::DynamicImage::ImageRgba8(pixels)
                    .into_rgb8()
                    .save(path)
                    .unwrap();
            } else {
                pixels.save(path).unwrap();
            }
        }
        let ctx = egui::Context::default();
        let mut app = ImageApp::from_context(ctx.clone(), "AI headless test".into(), None, None);
        configure(&ctx);
        let loaded = light_image_tool::pipeline::load(&paths[0], &Cancellation::default()).unwrap();
        app.pixels = Some(loaded.pixels.clone());
        app.loaded = Some(loaded);
        app.requested_path = Some(paths[0].clone());
        app.directory_path = Some(directory.to_path_buf());
        app.siblings = paths.clone();
        app.ai_sync_scope();
        (app, ctx, paths)
    }
    fn app_in(directory: &Path, names: &[&str]) -> (ImageApp, egui::Context, Vec<PathBuf>) {
        let (mut app, ctx, paths) = app_in_unscanned(directory, names);
        for path in &paths {
            app.ai.inspect_source_current(path);
            wait_ai(&mut app, &ctx);
        }
        app.ai_rebuild_view();
        (app, ctx, paths)
    }
    fn wait_ai(app: &mut ImageApp, ctx: &egui::Context) {
        let start = Instant::now();
        while app.ai.is_busy() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(5),
                "AI scan did not finish"
            );
            app.ai_poll(ctx);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        app.ai_poll(ctx);
    }
    fn frame(app: &mut ImageApp, ctx: &egui::Context, size: Vec2, events: Vec<egui::Event>) {
        let mut host = eframe::Frame::_new_kittest();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                focused: true,
                events,
                ..Default::default()
            },
            |ui| eframe::App::ui(app, ui, &mut host),
        );
        output.textures_delta.clear();
    }
    fn key(key: egui::Key) -> Vec<egui::Event> {
        [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
            .collect()
    }
    fn pointer(position: Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }
    fn mark(app: &mut ImageApp, path: &Path, choice: Option<ManualDecision>) {
        app.ai.mark(path, choice).unwrap();
        app.ai_rebuild_view();
    }

    #[test]
    fn result_changes_anchor_current_file_and_navigation_uses_search_intersection() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in(directory.path(), &["a.png", "b.png", "c.png"]);
        for path in &paths {
            mark(&mut app, path, Some(ManualDecision::Ai));
        }
        app.ai_view.filter = Filter::Ai;
        app.ai_rebuild_view();
        let original = app.pixels.clone().unwrap();
        mark(&mut app, &paths[0], Some(ManualDecision::NotAi));
        assert_eq!(app.requested_path.as_ref(), Some(&paths[0]));
        assert_eq!(app.loaded.as_ref().unwrap().path, paths[0]);
        assert_eq!(app.position, 0);
        assert!(Arc::ptr_eq(&original, app.pixels.as_ref().unwrap()));
        assert!(app.ai_current_excluded());
        assert_eq!(app.ai_visible_indices(), vec![1, 2]);
        assert_eq!(app.ai_navigation_target(1), Some(1));
        assert_eq!(app.ai_navigation_target(-1), Some(2));
        app.ai_view.search = "C.PNG".into();
        app.ai_rebuild_view();
        assert_eq!(app.ai_visible_indices(), vec![2]);
        app.navigate(1);
        assert_eq!(app.requested_path.as_ref(), Some(&paths[2]));
        app.ai_view.search = "no-match".into();
        app.ai_rebuild_view();
        app.navigate(1);
        assert_eq!(app.requested_path.as_ref(), Some(&paths[2]));
        app.ai_select_boundary(false);
        assert_eq!(app.requested_path.as_ref(), Some(&paths[2]));
        // Pending async completion may change classification, but never open another file.
        wait_ai(&mut app, &ctx);
        assert_eq!(app.requested_path.as_ref(), Some(&paths[2]));
    }

    #[test]
    fn material_sort_format_search_and_ai_filters_intersect_without_changing_selection() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, _ctx, paths) = app_in(
            directory.path(),
            &["image10.png", "image2.jpg", "image1.png", "image20.PNG"],
        );
        for path in &paths {
            mark(&mut app, path, Some(ManualDecision::Ai));
        }
        let selected = app.requested_path.clone();
        let original = app.pixels.clone().unwrap();
        app.ai_view.sort = SortOrder::SizeLargest;
        app.ai_view.format = FormatFilter::Png;
        app.ai_view.filter = Filter::Ai;
        let metadata = paths
            .iter()
            .zip([200, 400, 200, 300])
            .map(|(path, bytes)| {
                (
                    path.clone(),
                    FileMetadata {
                        bytes: Some(bytes),
                        regular_file: true,
                        ..Default::default()
                    },
                )
            })
            .collect();
        app.browser_set_metadata(metadata);
        assert_eq!(app.ai_visible_indices(), vec![3, 2, 0]);
        app.ai_view.search = "IMAGE1".into();
        app.ai_rebuild_view();
        assert_eq!(app.ai_visible_indices(), vec![2, 0]);
        mark(&mut app, &paths[0], Some(ManualDecision::NotAi));
        assert_eq!(app.ai_visible_indices(), vec![2]);
        assert_eq!(app.requested_path, selected);
        assert_eq!(app.position, 0);
        assert!(Arc::ptr_eq(&original, app.pixels.as_ref().unwrap()));
        assert!(app.ai_current_excluded());
    }

    #[test]
    fn same_directory_current_refresh_updates_one_row_without_full_decision_rebuild() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, _ctx, paths) = app_in(directory.path(), &["a.png", "b.png", "c.png"]);
        mark(&mut app, &paths[1], Some(ManualDecision::Ai));
        app.ai_view.filter = Filter::Ai;
        app.ai_rebuild_view();
        assert_eq!(app.ai_visible_indices(), vec![1]);
        let rebuilds = app.ai_view.full_rebuilds;
        app.ai.set_current(&paths[1]);
        app.ai_refresh_current(&paths[1]);
        assert_eq!(app.ai_view.full_rebuilds, rebuilds);
        let counts = app.ai_view.counts;
        let rows = app.ai_visible_indices();
        assert!(rows.is_empty());
        app.ai_rebuild_view();
        assert_eq!(app.ai_view.counts, counts);
        assert_eq!(app.ai_visible_indices(), rows);
    }

    #[test]
    fn excluded_current_navigation_and_metadata_refresh_follow_material_order() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, _ctx, paths) =
            app_in_unscanned(directory.path(), &["a.png", "b.jpg", "c.png"]);
        app.position = 1;
        app.requested_path = Some(paths[1].clone());
        app.ai_view.sort = SortOrder::SizeLargest;
        app.ai_view.format = FormatFilter::Png;
        let metadata = paths
            .iter()
            .zip([10, 20, 30])
            .map(|(path, bytes)| {
                (
                    path.clone(),
                    FileMetadata {
                        bytes: Some(bytes),
                        regular_file: true,
                        ..Default::default()
                    },
                )
            })
            .collect();
        app.browser_set_metadata(metadata);
        assert_eq!(app.ai_visible_indices(), vec![2, 0]);
        assert_eq!(app.ai_navigation_target(1), Some(0));
        assert_eq!(app.ai_navigation_target(-1), Some(2));
        app.ai_view.metadata.get_mut(&paths[0]).unwrap().bytes = Some(40);
        app.browser_set_metadata(app.ai_view.metadata.clone());
        assert_eq!(app.ai_visible_indices(), vec![0, 2]);
        assert_eq!(app.position, 1);
        assert_eq!(app.requested_path.as_ref(), Some(&paths[1]));
    }

    #[test]
    fn badges_are_display_only_and_independent_of_scan_filters_and_pixels() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in(directory.path(), &["a.png"]);
        let source = std::fs::read(&paths[0]).unwrap();
        let pixels = app.pixels.as_ref().unwrap().as_raw().clone();
        mark(&mut app, &paths[0], Some(ManualDecision::Ai));
        app.ai_view.filter = Filter::Ai;
        app.ai_rebuild_view();
        let rows = app.ai_visible_indices();
        let counts = app.ai_view.counts;
        frame(&mut app, &ctx, Vec2::new(1180.0, 780.0), vec![]);
        assert!(app.ai_view.badge_rect.is_some());
        let automatic = app.ai.settings().auto_scan;
        app.ai.set_show_badges(false);
        frame(&mut app, &ctx, Vec2::new(1180.0, 780.0), vec![]);
        assert!(app.ai_view.badge_rect.is_none());
        assert_eq!(app.ai.settings().auto_scan, automatic);
        assert_eq!(app.ai_visible_indices(), rows);
        assert_eq!(app.ai_view.counts, counts);
        app.ai.set_auto_scan(false);
        app.ai.set_show_badges(true);
        frame(&mut app, &ctx, Vec2::new(1180.0, 780.0), vec![]);
        assert!(app.ai_view.badge_rect.is_some());
        assert_eq!(app.ai_view.counts, counts);
        assert_eq!(app.pixels.as_ref().unwrap().as_raw(), &pixels);
        assert_eq!(std::fs::read(&paths[0]).unwrap(), source);
    }

    #[test]
    fn manual_buttons_set_reverse_and_reset_without_erasing_machine_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in(directory.path(), &["a.png"]);
        let original_record = app.ai.record(&paths[0]).unwrap().clone();
        let draw = |app: &mut ImageApp, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(360.0, 320.0))),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| {
                    app.ai_manual_controls(ui, &paths[0]);
                },
            );
            output.textures_delta.clear();
        };
        draw(&mut app, vec![]);
        for (index, expected) in [Some(ManualDecision::Ai), Some(ManualDecision::NotAi), None]
            .into_iter()
            .enumerate()
        {
            let position = app.ai_view.manual_buttons[index].unwrap().center();
            draw(&mut app, pointer(position, true));
            draw(&mut app, pointer(position, false));
            assert_eq!(
                app.ai.manual(&paths[0]).map(|value| value.decision),
                expected
            );
            assert_eq!(app.ai.record(&paths[0]), Some(&original_record));
        }
    }

    #[test]
    fn filename_text_focus_survives_results_and_blocks_navigation_shortcuts() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in(directory.path(), &["a.png", "b.png"]);
        app.preferences.list_visible = true;
        let size = Vec2::new(1180.0, 780.0);
        frame(&mut app, &ctx, size, vec![]);
        let search = egui::Id::new("ai-folder-search");
        ctx.memory_mut(|memory| memory.request_focus(search));
        frame(&mut app, &ctx, size, vec![]);
        assert!(ctx.text_edit_focused());
        mark(&mut app, &paths[1], Some(ManualDecision::Ai));
        frame(&mut app, &ctx, size, key(egui::Key::ArrowRight));
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(search));
        assert_eq!(app.requested_path.as_ref(), Some(&paths[0]));
        frame(&mut app, &ctx, size, key(egui::Key::End));
        assert_eq!(app.requested_path.as_ref(), Some(&paths[0]));
        assert!(!app.edited);
    }

    #[test]
    fn clear_confirmation_blocks_navigation_and_escape_preserves_records() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in(directory.path(), &["a.png", "b.png"]);
        mark(&mut app, &paths[0], Some(ManualDecision::Ai));
        let size = Vec2::new(600.0, 420.0);
        for kind in [ClearRecords::Automatic, ClearRecords::Manual] {
            app.ai_view.confirm = Some(kind);
            frame(&mut app, &ctx, size, vec![]);
            frame(&mut app, &ctx, size, key(egui::Key::ArrowRight));
            assert_eq!(app.requested_path.as_ref(), Some(&paths[0]));
            assert_eq!(app.ai_view.confirm, Some(kind));
            frame(&mut app, &ctx, size, key(egui::Key::Escape));
            assert!(app.ai_view.confirm.is_none());
            assert!(app.ai.manual(&paths[0]).is_some());
            assert!(app.ai.record(&paths[0]).is_some());
        }
    }

    #[test]
    fn narrow_window_collapses_list_preserves_preference_and_icon_toolbar() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in(directory.path(), &["a.png"]);
        app.preferences.list_visible = true;
        app.info_open = true;
        mark(&mut app, &paths[0], Some(ManualDecision::Ai));
        let size = Vec2::new(360.0, 320.0);
        frame(&mut app, &ctx, size, vec![]);
        assert!(app.preferences.list_visible && app.list_viewport_rect.is_none());
        let screen = Rect::from_min_size(Pos2::ZERO, size);
        let toolbar = app.toolbar_layout.as_ref().unwrap();
        for (rect, visible) in toolbar.buttons.iter().zip(toolbar.visible) {
            if visible {
                assert!(screen.contains_rect(*rect));
            }
        }
        assert_eq!(
            toolbar.visible.iter().filter(|visible| **visible).count(),
            7
        );
        app.info_open = false;
        frame(&mut app, &ctx, Vec2::new(1180.0, 780.0), vec![]);
        assert!(app.list_viewport_rect.is_some());
    }

    #[test]
    fn filter_popup_owns_navigation_and_escape_only_closes_popup() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in(directory.path(), &["a.png", "b.png"]);
        app.preferences.list_visible = true;
        app.info_open = true;
        let size = Vec2::new(1180.0, 780.0);
        frame(&mut app, &ctx, size, vec![]);
        let position = app.ai_view.filter_button.unwrap().center();
        for pressed in [true, false] {
            frame(&mut app, &ctx, size, pointer(position, pressed));
        }
        assert!(egui::Popup::is_any_open(&ctx));
        frame(&mut app, &ctx, size, key(egui::Key::ArrowRight));
        assert_eq!(app.requested_path.as_ref(), Some(&paths[0]));
        frame(&mut app, &ctx, size, key(egui::Key::Escape));
        assert!(!egui::Popup::is_any_open(&ctx));
        assert!(app.info_open);
    }

    #[test]
    fn clear_close_cancel_and_confirm_have_distinct_non_destructive_scopes() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in(directory.path(), &["a.png", "b.png"]);
        mark(&mut app, &paths[0], Some(ManualDecision::Ai));
        let source = std::fs::read(&paths[0]).unwrap();
        let size = Vec2::new(600.0, 420.0);
        for cancel_index in [0, 1] {
            app.ai_view.confirm = Some(ClearRecords::Manual);
            frame(&mut app, &ctx, size, vec![]);
            frame(&mut app, &ctx, size, vec![]);
            let position = app.ai_view.clear_buttons[cancel_index].unwrap().center();
            for pressed in [true, false] {
                frame(&mut app, &ctx, size, pointer(position, pressed));
            }
            assert!(app.ai_view.confirm.is_none());
            assert!(app.ai.manual(&paths[0]).is_some());
        }
        for kind in [
            ClearRecords::Automatic,
            ClearRecords::Manual,
            ClearRecords::Model,
        ] {
            app.ai_view.confirm = Some(kind);
            frame(&mut app, &ctx, size, vec![]);
            frame(&mut app, &ctx, size, vec![]);
            let position = app.ai_view.clear_buttons[2].unwrap().center();
            for pressed in [true, false] {
                frame(&mut app, &ctx, size, pointer(position, pressed));
            }
            assert!(app.ai_view.confirm.is_none());
            if kind == ClearRecords::Automatic {
                assert!(app.ai.manual(&paths[0]).is_some());
            } else {
                assert!(app.ai.manual(&paths[0]).is_none());
            }
            // Releasing or clicking the old dialog target cannot repeat the clear or navigate.
            for pressed in [true, false] {
                frame(&mut app, &ctx, size, pointer(position, pressed));
            }
            assert_eq!(app.requested_path.as_ref(), Some(&paths[0]));
            assert_eq!(std::fs::read(&paths[0]).unwrap(), source);
        }
    }

    #[test]
    fn settings_badge_toggle_keeps_detection_on_demand() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, _) = app_in(directory.path(), &["a.png"]);
        let draw = |app: &mut ImageApp, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 1000.0))),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| app.ai_settings_ui(ui),
            );
            output.textures_delta.clear();
        };
        draw(&mut app, vec![]);
        let position = app.ai_view.badge_toggle.unwrap().center();
        for pressed in [true, false] {
            draw(&mut app, pointer(position, pressed));
        }
        assert!(!app.ai.settings().show_badges);
        assert!(!app.ai.settings().auto_scan);
        let position = app.ai_view.badge_toggle.unwrap().center();
        for pressed in [true, false] {
            draw(&mut app, pointer(position, pressed));
        }
        assert!(app.ai.settings().show_badges);
        assert!(!app.ai.settings().auto_scan);
    }
    /// A deterministic CPU raster of the *actual* egui meshes and texture deltas.
    /// This is a layout/content check, not a native window, GPU or font-driver test.
    struct CpuRenderer {
        textures: std::collections::HashMap<egui::TextureId, egui::ColorImage>,
        capture: bool,
    }
    impl CpuRenderer {
        fn render(
            &mut self,
            ctx: &egui::Context,
            mut output: egui::FullOutput,
            size: Vec2,
        ) -> image::RgbaImage {
            for (id, deltas) in &output.textures_delta.set {
                for delta in deltas {
                    let egui::ImageData::Color(image) = &delta.image;
                    if let Some([x, y]) = delta.pos {
                        let texture = self
                            .textures
                            .get_mut(id)
                            .expect("partial texture before full upload");
                        for row in 0..image.size[1] {
                            let offset = (y + row) * texture.size[0] + x;
                            texture.pixels[offset..offset + image.size[0]].copy_from_slice(
                                &image.pixels[row * image.size[0]..(row + 1) * image.size[0]],
                            );
                        }
                    } else {
                        self.textures.insert(*id, (**image).clone());
                    }
                }
            }
            if !self.capture {
                for id in &output.textures_delta.free {
                    self.textures.remove(id);
                }
                output.textures_delta.clear();
                return image::RgbaImage::new(0, 0);
            }
            let scale = output.pixels_per_point;
            let width = (size.x * scale).round() as u32;
            let height = (size.y * scale).round() as u32;
            let mut image =
                image::RgbaImage::from_pixel(width, height, image::Rgba([247, 248, 250, 255]));
            for primitive in ctx.tessellate(output.shapes, scale) {
                let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive else {
                    panic!("unexpected custom paint callback");
                };
                let texture = self
                    .textures
                    .get(&mesh.texture_id)
                    .expect("mesh references uploaded texture");
                let clip = primitive.clip_rect * scale;
                for indices in mesh.indices.as_chunks::<3>().0 {
                    let vertices = [
                        mesh.vertices[indices[0] as usize],
                        mesh.vertices[indices[1] as usize],
                        mesh.vertices[indices[2] as usize],
                    ];
                    let [a, b, c] = vertices.map(|v| v.pos * scale);
                    let determinant = (b.y - c.y) * (a.x - c.x) + (c.x - b.x) * (a.y - c.y);
                    if determinant.abs() < 0.00001 {
                        continue;
                    }
                    let min_x = a.x.min(b.x).min(c.x).floor().max(clip.left()).max(0.0) as u32;
                    let max_x =
                        a.x.max(b.x)
                            .max(c.x)
                            .ceil()
                            .min(clip.right())
                            .min(width as f32) as u32;
                    let min_y = a.y.min(b.y).min(c.y).floor().max(clip.top()).max(0.0) as u32;
                    let max_y =
                        a.y.max(b.y)
                            .max(c.y)
                            .ceil()
                            .min(clip.bottom())
                            .min(height as f32) as u32;
                    for y in min_y..max_y {
                        for x in min_x..max_x {
                            let p = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
                            let w0 = ((b.y - c.y) * (p.x - c.x) + (c.x - b.x) * (p.y - c.y))
                                / determinant;
                            let w1 = ((c.y - a.y) * (p.x - c.x) + (a.x - c.x) * (p.y - c.y))
                                / determinant;
                            let w2 = 1.0 - w0 - w1;
                            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                                continue;
                            }
                            let weights = [w0, w1, w2];
                            let mut uv = Vec2::ZERO;
                            let mut color = [0.0; 4];
                            for (v, weight) in vertices.iter().zip(weights) {
                                uv += v.uv.to_vec2() * weight;
                                for (channel, value) in color.iter_mut().enumerate() {
                                    *value +=
                                        f32::from(v.color.to_array()[channel]) * weight / 255.0;
                                }
                            }
                            let tx = (uv.x * texture.size[0] as f32 - 0.5)
                                .clamp(0.0, (texture.size[0] - 1) as f32);
                            let ty = (uv.y * texture.size[1] as f32 - 0.5)
                                .clamp(0.0, (texture.size[1] - 1) as f32);
                            let x0 = tx.floor() as usize;
                            let y0 = ty.floor() as usize;
                            let x1 = (x0 + 1).min(texture.size[0] - 1);
                            let y1 = (y0 + 1).min(texture.size[1] - 1);
                            let fx = tx.fract();
                            let fy = ty.fract();
                            let samples = [
                                (x0, y0, (1.0 - fx) * (1.0 - fy)),
                                (x1, y0, fx * (1.0 - fy)),
                                (x0, y1, (1.0 - fx) * fy),
                                (x1, y1, fx * fy),
                            ];
                            let mut sample = [0.0; 4];
                            for (sx, sy, weight) in samples {
                                for (channel, value) in sample.iter_mut().enumerate() {
                                    *value += f32::from(
                                        texture.pixels[sy * texture.size[0] + sx].to_array()
                                            [channel],
                                    ) * weight
                                        / 255.0;
                                }
                            }
                            let alpha = color[3] * sample[3];
                            let destination = image.get_pixel_mut(x, y);
                            for channel in 0..3 {
                                destination[channel] = (color[channel] * sample[channel] * 255.0
                                    + f32::from(destination[channel]) * (1.0 - alpha))
                                    .round()
                                    .clamp(0.0, 255.0)
                                    as u8;
                            }
                        }
                    }
                }
            }
            for id in &output.textures_delta.free {
                self.textures.remove(id);
            }
            output.textures_delta.clear();
            image
        }
    }
    fn render_actual_frame(
        app: &mut ImageApp,
        ctx: &egui::Context,
        renderer: &mut CpuRenderer,
        size: Vec2,
        events: Vec<egui::Event>,
    ) -> image::RgbaImage {
        let mut frame = eframe::Frame::_new_kittest();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                focused: true,
                time: Some(app.frames as f64 * 0.25 + 1.0),
                events,
                ..Default::default()
            },
            |ui| {
                app.poll(ctx);
                eframe::App::ui(app, ui, &mut frame);
            },
        );
        renderer.render(ctx, output, size)
    }

    #[test]
    #[ignore = "requires GLOAMERE_AI_RENDER_DIR; CPU mesh QA, not native GPU acceptance"]
    fn render_actual_ai_ui_states() {
        let Some(destination) = std::env::var_os("GLOAMERE_AI_RENDER_DIR") else {
            eprintln!(
                "UI render not requested; set GLOAMERE_AI_RENDER_DIR for artifact generation"
            );
            return;
        };
        let destination = PathBuf::from(destination);
        std::fs::create_dir_all(&destination).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in(
            directory.path(),
            &[
                "01_landscape.png",
                "02_illustration.png",
                "03_metadata.png",
                "04_unknown.png",
            ],
        );
        configure_with_font_candidates(
            &ctx,
            &[
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                "C:\\Windows\\Fonts\\msyh.ttc",
                "/System/Library/Fonts/PingFang.ttc",
            ],
        );
        let fixture = image::RgbaImage::from_fn(640, 420, |x, y| {
            let hill = 270.0 + (x as f32 / 125.0).sin() * 35.0;
            if (x as i32 - 485).pow(2) + (y as i32 - 95).pow(2) < 32_i32.pow(2) {
                image::Rgba([250, 228, 168, 255])
            } else if y as f32 > hill + 75.0 {
                image::Rgba([38, 73, 74, 255])
            } else if y as f32 > hill {
                image::Rgba([73, 111, 108, 255])
            } else {
                image::Rgba([153 + (y / 15) as u8, 183 + (y / 20) as u8, 198, 255])
            }
        });
        for path in &paths {
            fixture.save(path).unwrap();
        }
        {
            let file = std::fs::File::create(&paths[2]).unwrap();
            let mut encoder = png::Encoder::new(file, fixture.width(), fixture.height());
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .add_text_chunk(
                    "parameters".into(),
                    "QA synthetic fixture\nSteps: 20, Sampler: Euler, Seed: 1".into(),
                )
                .unwrap();
            encoder
                .write_header()
                .unwrap()
                .write_image_data(fixture.as_raw())
                .unwrap();
        }
        let loaded = light_image_tool::pipeline::load(&paths[0], &Cancellation::default()).unwrap();
        app.pixels = Some(loaded.pixels.clone());
        app.loaded = Some(loaded);
        app.refresh_texture(&ctx);
        app.ai.refresh();
        for path in &paths {
            app.ai.inspect_source_current(path);
            wait_ai(&mut app, &ctx);
        }
        app.preferences.list_visible = true;
        app.info_open = true;
        let mut renderer = CpuRenderer {
            textures: Default::default(),
            capture: false,
        };
        let desktop = Vec2::new(1180.0, 780.0);
        for _ in 0..8 {
            render_actual_frame(&mut app, &ctx, &mut renderer, desktop, vec![]);
        }
        renderer.capture = true;
        render_actual_frame(&mut app, &ctx, &mut renderer, desktop, vec![])
            .save(destination.join("source-only-1180x780.png"))
            .unwrap();
        renderer.capture = false;
        mark(&mut app, &paths[0], Some(ManualDecision::Ai));
        app.ai_view.filter = Filter::Ai;
        app.ai_rebuild_view();
        for _ in 0..3 {
            render_actual_frame(&mut app, &ctx, &mut renderer, desktop, vec![]);
        }
        renderer.capture = true;
        render_actual_frame(&mut app, &ctx, &mut renderer, desktop, vec![])
            .save(destination.join("manual-filter-1180x780.png"))
            .unwrap();
        renderer.capture = false;
        let position = app.ai_view.filter_button.unwrap().center();
        for pressed in [true, false] {
            render_actual_frame(
                &mut app,
                &ctx,
                &mut renderer,
                desktop,
                pointer(position, pressed),
            );
        }
        for _ in 0..3 {
            render_actual_frame(&mut app, &ctx, &mut renderer, desktop, vec![]);
        }
        renderer.capture = true;
        render_actual_frame(&mut app, &ctx, &mut renderer, desktop, vec![])
            .save(destination.join("filter-popup-1180x780.png"))
            .unwrap();
        renderer.capture = false;
        render_actual_frame(
            &mut app,
            &ctx,
            &mut renderer,
            desktop,
            key(egui::Key::Escape),
        );
        let narrow = Vec2::new(360.0, 640.0);
        for _ in 0..5 {
            render_actual_frame(&mut app, &ctx, &mut renderer, narrow, vec![]);
        }
        renderer.capture = true;
        render_actual_frame(&mut app, &ctx, &mut renderer, narrow, vec![])
            .save(destination.join("manual-drawer-360x640.png"))
            .unwrap();
        renderer.capture = false;
        app.ai_view.confirm = Some(ClearRecords::Manual);
        for _ in 0..3 {
            render_actual_frame(&mut app, &ctx, &mut renderer, narrow, vec![]);
        }
        renderer.capture = true;
        render_actual_frame(&mut app, &ctx, &mut renderer, narrow, vec![])
            .save(destination.join("manual-clear-360x640.png"))
            .unwrap();
        renderer.capture = false;
        app.ai_view.confirm = None;
        app.ai_view.settings_open = true;
        for _ in 0..4 {
            render_actual_frame(&mut app, &ctx, &mut renderer, desktop, vec![]);
        }
        renderer.capture = true;
        render_actual_frame(&mut app, &ctx, &mut renderer, desktop, vec![])
            .save(destination.join("component-settings-1180x780.png"))
            .unwrap();
        renderer.capture = false;
        std::fs::write(destination.join("README.txt"), "Actual ImageApp::ui egui mesh/texture CPU renders. Synthetic test image and unsigned PNG test metadata. No native window, GPU, OS input, or classifier quality evidence. Renderer uses bilinear texture sampling and premultiplied alpha; native GPU glyph/edge rendering may differ.\n").unwrap();
    }
    #[test]
    fn ai_detection_more_menu_opens_current_image_details_at_all_widths() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in_unscanned(directory.path(), &["a.png", "b.png"]);
        let original = std::fs::read(&paths[0]).unwrap();
        assert!(!Icon::ALL.contains(&Icon::AiDetect));
        for size in [Vec2::new(1180.0, 780.0), Vec2::new(360.0, 640.0)] {
            app.info_open = false;
            frame(&mut app, &ctx, size, vec![]);
            assert!(!app.info_open);
            let index = Icon::ALL
                .iter()
                .position(|icon| *icon == Icon::More)
                .unwrap();
            let layout = app.toolbar_layout.as_ref().unwrap();
            assert!(layout.visible[index]);
            let position = layout.buttons[index].center();
            for pressed in [true, false] {
                frame(&mut app, &ctx, size, pointer(position, pressed));
            }
            frame(&mut app, &ctx, size, vec![]);
            assert!(app.more_open && !app.ai.is_busy());
            assert!(app.ai.record(&paths[0]).is_none());
            let position = app.ai_view.detection_menu_button.unwrap().center();
            assert!(Rect::from_min_size(Pos2::ZERO, size).contains(position));
            for pressed in [true, false] {
                frame(&mut app, &ctx, size, pointer(position, pressed));
            }
            assert!(app.info_open && !app.editing && !app.more_open);
            assert_eq!(app.requested_path.as_ref(), Some(&paths[0]));
            assert_eq!(app.loaded.as_ref().unwrap().path, paths[0]);
            assert!(!app.ai.settings().auto_scan && !app.ai.experimental_auto_scan());
            wait_ai(&mut app, &ctx);
            assert!(app.ai.record(&paths[1]).is_none());
            assert_eq!(app.ai.component_state(), &ComponentState::Missing);
            assert!(app.ai.record(&paths[0]).is_none());
            assert_eq!(app.ai.run_state(&paths[0]), RunState::Unscanned);
        }
        assert_eq!(std::fs::read(&paths[0]).unwrap(), original);
    }
    #[test]
    fn source_information_button_allows_manual_mark_without_model() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, ctx, paths) = app_in_unscanned(directory.path(), &["a.png", "b.png"]);
        let original = std::fs::read(&paths[0]).unwrap();
        assert_eq!(app.ai.component_state(), &ComponentState::Missing);
        assert!(!app.ai.is_busy() && app.ai.record(&paths[0]).is_none());
        app.ai_detect_current();
        assert_eq!(app.ai.run_state(&paths[0]), RunState::Unscanned);
        assert!(app.ai.record(&paths[0]).is_none());
        let draw = |app: &mut ImageApp, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(500.0, 1400.0))),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| app.ai_details(ui),
            );
            output.textures_delta.clear();
        };
        draw(&mut app, vec![]);
        let position = app.ai_view.source_button.unwrap().center();
        for pressed in [true, false] {
            draw(&mut app, pointer(position, pressed));
        }
        wait_ai(&mut app, &ctx);
        let record = app.ai.record(&paths[0]).unwrap();
        assert!(record.identity.has_content_hash() && record.model.is_none());
        assert_eq!(app.ai.run_state(&paths[0]), RunState::SourceOnly);
        assert!(app.ai.record(&paths[1]).is_none());
        draw(&mut app, vec![]);
        let position = app.ai_view.manual_buttons[0].unwrap().center();
        for pressed in [true, false] {
            draw(&mut app, pointer(position, pressed));
        }
        assert_eq!(
            app.ai.manual(&paths[0]).unwrap().decision,
            ManualDecision::Ai
        );
        assert_eq!(app.ai.component_state(), &ComponentState::Missing);
        assert!(!app.ai.is_busy() && app.ai.record(&paths[0]).unwrap().model.is_none());
        assert_eq!(std::fs::read(&paths[0]).unwrap(), original);
    }
    #[test]
    fn detection_summary_preserves_installed_pending_validation_label() {
        fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => texts.push(text.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, texts);
                    }
                }
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let decision = light_image_tool::ai::policy::decide(None, None);
        let component_label = "组件已安装 · 点击 AI 检测时校验";
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ai_detection_summary(
                ui,
                &decision,
                &ComponentState::Missing,
                component_label,
                &RunState::Unscanned,
                ReleaseTier::Unavailable,
            );
        });
        output.textures_delta.clear();
        let mut texts = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut texts);
        }
        assert!(
            texts
                .iter()
                .any(|text| text == &format!("组件：{component_label}"))
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.contains(ComponentState::Missing.label()))
        );
    }

    #[test]
    #[ignore = "requires GLOAMERE_AI_RENDER_DIR; synthetic UI-state fixtures, not classifier-quality evidence"]
    fn render_v07_detection_summary_states() {
        use light_image_tool::ai::{model, policy, types::*};
        let Some(destination) = std::env::var_os("GLOAMERE_AI_RENDER_DIR") else {
            return;
        };
        let destination = PathBuf::from(destination);
        std::fs::create_dir_all(&destination).unwrap();
        let ctx = egui::Context::default();
        configure_with_font_candidates(
            &ctx,
            &["/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"],
        );
        let mut renderer = CpuRenderer {
            textures: Default::default(),
            capture: true,
        };
        let variants = [
            ("missing", ComponentState::Missing, RunState::SourceOnly),
            ("disabled", ComponentState::Ready, RunState::Disabled),
            ("queued", ComponentState::Ready, RunState::Queued),
            ("running", ComponentState::Ready, RunState::Running),
            (
                "completed-positive",
                ComponentState::Ready,
                RunState::CompletedExperimental,
            ),
            (
                "completed-negative",
                ComponentState::Ready,
                RunState::CompletedExperimental,
            ),
            (
                "unsupported",
                ComponentState::Ready,
                RunState::Unsupported("透明或动画输入不在当前验证范围".into()),
            ),
            ("expired", ComponentState::Ready, RunState::Expired),
            ("timeout", ComponentState::Ready, RunState::TimedOut),
            (
                "failed",
                ComponentState::Failed("固定自检未通过".into()),
                RunState::Failed("组件自检失败，可重试离线导入".into()),
            ),
        ];
        for (name, component, run) in variants {
            let mut record = MachineRecord::unscanned(AssetIdentity {
                key: "synthetic-ui-state".into(),
                content_hash: "a".repeat(64),
                size: 1,
                modified_ns: 0,
            });
            record.run_state = Some(run.clone());
            record.component_state = component.clone();
            record.release_tier = if component == ComponentState::Ready {
                ReleaseTier::Experimental
            } else {
                ReleaseTier::Unavailable
            };
            record.phase = match &run {
                RunState::CompletedExperimental => Phase::Complete,
                RunState::Unsupported(s) => Phase::Unsupported(s.clone()),
                RunState::TimedOut => Phase::TimedOut,
                RunState::Failed(s) => Phase::Failed(s.clone()),
                _ => Phase::AwaitingModel,
            };
            if run == RunState::CompletedExperimental {
                record.model = Some(ModelResult {
                    fingerprint: model::fingerprint(),
                    input_scope: "中心 224×224，非整幅覆盖".into(),
                    raw_score: if name == "completed-positive" {
                        0.2528097
                    } else {
                        -4.0221
                    },
                    threshold: 0.0,
                    approved: false,
                    high_signal: false,
                });
            }
            let decision = policy::decide(Some(&record), None);
            for width in [360.0, 640.0] {
                let size = Vec2::new(width, 640.0);
                for _ in 0..3 {
                    let output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                            ..Default::default()
                        },
                        |ui| {
                            egui::CentralPanel::default().show(ui, |ui| {
                                ui.heading("AI 筛查");
                                ui.small("v0.7 状态界面测试夹具");
                                ui.separator();
                                ai_detection_summary(
                                    ui,
                                    &decision,
                                    &component,
                                    component.label(),
                                    &run,
                                    record.release_tier,
                                );
                                ui.separator();
                                ui.small("无信号不能排除 AI；模型分值不是来源证明。");
                                if let Some(badge) = &decision.badge {
                                    viewer_ui::paint_ai_badge(
                                        ui.painter(),
                                        Pos2::new(16.0, 540.0),
                                        badge,
                                        decision.suspected,
                                        decision.manual_active,
                                        true,
                                    );
                                }
                            });
                        },
                    );
                    let image = renderer.render(&ctx, output, size);
                    image
                        .save(destination.join(format!("state-{name}-{}x640.png", width as u32)))
                        .unwrap();
                }
            }
        }
    }
    #[test]
    #[ignore = "requires actual record JSON and GLOAMERE_AI_RENDER_DIR; recorded isolated-worker UI evidence"]
    fn render_actual_v07_worker_records() {
        use light_image_tool::ai::{policy, types::MachineRecord};
        #[derive(serde::Deserialize)]
        struct Fixture {
            name: String,
            image_path: PathBuf,
            record: MachineRecord,
        }
        let Some(records) = std::env::var_os("GLOAMERE_AI_QA_RECORDS") else {
            return;
        };
        let destination =
            PathBuf::from(std::env::var_os("GLOAMERE_AI_RENDER_DIR").expect("render destination"));
        std::fs::create_dir_all(&destination).unwrap();
        let fixtures: Vec<Fixture> =
            serde_json::from_slice(&std::fs::read(records).unwrap()).unwrap();
        assert_eq!(fixtures.len(), 2);
        let ctx = egui::Context::default();
        configure_with_font_candidates(
            &ctx,
            &["/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"],
        );
        let mut renderer = CpuRenderer {
            textures: Default::default(),
            capture: true,
        };
        for fixture in fixtures {
            let pixels = image::open(&fixture.image_path).unwrap().to_rgba8();
            let texture = ctx.load_texture(
                &fixture.name,
                egui::ColorImage::from_rgba_unmultiplied(
                    [pixels.width() as usize, pixels.height() as usize],
                    pixels.as_raw(),
                ),
                egui::TextureOptions::LINEAR,
            );
            let decision = policy::decide(Some(&fixture.record), None);
            assert_eq!(
                fixture.record.run_state,
                Some(RunState::CompletedExperimental)
            );
            assert!(!fixture.record.model.as_ref().unwrap().approved);
            let original_size = Vec2::new(pixels.width() as f32, pixels.height() as f32);
            for size in [Vec2::new(1180.0, 780.0), Vec2::new(360.0, 780.0)] {
                for _ in 0..3 {
                    let output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                            ..Default::default()
                        },
                        |ui| {
                            egui::CentralPanel::default().show(ui, |ui| {
                                ui.heading("gloamere image · AI 筛查");
                                ui.small("实际本地 worker 输出 · CPU 界面回归");
                                ui.separator();
                                let image_size = if size.x > 700.0 {
                                    original_size
                                        * (700.0 / original_size.x).min(650.0 / original_size.y)
                                } else {
                                    original_size
                                        * (ui.available_width() / original_size.x)
                                            .min(240.0 / original_size.y)
                                };
                                let show_image = |ui: &mut egui::Ui| {
                                    let response = ui.image((texture.id(), image_size));
                                    if let Some(badge) = &decision.badge {
                                        viewer_ui::paint_ai_badge(
                                            ui.painter(),
                                            response.rect.min + Vec2::splat(12.0),
                                            badge,
                                            decision.suspected,
                                            false,
                                            true,
                                        );
                                    }
                                };
                                if size.x > 700.0 {
                                    ui.horizontal_top(|ui| {
                                        show_image(ui);
                                        ui.vertical(|ui| {
                                            ui.set_width(380.0);
                                            ai_detection_summary(
                                                ui,
                                                &decision,
                                                &fixture.record.component_state,
                                                fixture.record.component_state.label(),
                                                fixture.record.run_state.as_ref().unwrap(),
                                                fixture.record.release_tier,
                                            );
                                        });
                                    });
                                } else {
                                    show_image(ui);
                                    ui.separator();
                                    ai_detection_summary(
                                        ui,
                                        &decision,
                                        &fixture.record.component_state,
                                        fixture.record.component_state.label(),
                                        fixture.record.run_state.as_ref().unwrap(),
                                        fixture.record.release_tier,
                                    );
                                }
                            });
                        },
                    );
                    let image = renderer.render(&ctx, output, size);
                    image
                        .save(destination.join(format!(
                            "actual-{}-{}x{}.png",
                            fixture.name, size.x as u32, size.y as u32
                        )))
                        .unwrap();
                }
            }
        }
    }
    #[test]
    fn experimental_thumbnail_badge_fits_fifty_pixel_tile() {
        let ctx = egui::Context::default();
        configure_with_font_candidates(
            &ctx,
            &["/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"],
        );
        let decision = Decision {
            bucket: light_image_tool::ai::Bucket::Ai,
            label: "疑似 AI · 实验".into(),
            badge: Some("AI ? · 实验".into()),
            conflict: false,
            manual_active: false,
            suspected: true,
            reasons: vec![],
        };
        let size = Vec2::new(150.0, 105.0);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                ..Default::default()
            },
            |ui| {
                ui.small("50 px 缩略图角标");
                let (tile, _) = ui.allocate_exact_size(Vec2::splat(50.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(tile, 2.0, Color32::from_rgb(120, 151, 165));
                let label = thumbnail_badge_label(&decision, decision.badge.as_ref().unwrap());
                assert_eq!(label, "AI? 实");
                let rect = viewer_ui::paint_ai_badge(
                    &ui.painter_at(tile),
                    tile.left_bottom() - Vec2::new(0.0, 19.0),
                    label,
                    true,
                    false,
                    false,
                );
                assert!(
                    rect.width() <= 50.0,
                    "thumbnail badge width {}",
                    rect.width()
                );
            },
        );
        if let Some(destination) = std::env::var_os("GLOAMERE_AI_RENDER_DIR") {
            let mut renderer = CpuRenderer {
                textures: Default::default(),
                capture: true,
            };
            renderer
                .render(&ctx, output, size)
                .save(PathBuf::from(destination).join("experimental-thumbnail-50px.png"))
                .unwrap();
            return;
        }
        output.textures_delta.clear();
    }
}
