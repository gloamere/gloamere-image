//! Small, dependency-free viewer chrome. Coordinates are logical egui points.
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

pub const PRODUCT_NAME: &str = "gloamere-image";
const WINDOW_ICON: &[u8] = include_bytes!("../assets/gloamere-image-64.png");

pub fn window_icon() -> egui::IconData {
    let image = image::load_from_memory_with_format(WINDOW_ICON, image::ImageFormat::Png)
        .expect("checked-in gloamere-image icon must decode")
        .into_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}
pub fn empty_brand_rect(area: Rect) -> Rect {
    let side = 48.0_f32.min(area.width()).min(area.height()).max(0.0);
    let y = (area.center().y - 105.0)
        .max(area.top() + side / 2.0 + 8.0)
        .min(area.bottom() - side / 2.0);
    Rect::from_center_size(Pos2::new(area.center().x, y), Vec2::splat(side))
}
pub fn load_brand_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let icon = window_icon();
    ctx.load_texture(
        "gloamere-image-brand",
        egui::ColorImage::from_rgba_unmultiplied(
            [icon.width as usize, icon.height as usize],
            &icon.rgba,
        ),
        egui::TextureOptions::LINEAR,
    )
}

pub const TOOLBAR_HEIGHT: f32 = 52.0;
pub const BUTTON_COUNT: usize = 11;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Open,
    Previous,
    Next,
    Minus,
    Plus,
    Fit,
    Actual,
    Rotate,
    Undo,
    Export,
    More,
    List,
    Crop,
    Info,
    RotateLeft,
    Fullscreen,
    Flip,
    Refresh,
    Locate,
    Restore,
    Recheck,
    CropApply,
    ResizeApply,
    Close,
}
impl Icon {
    pub const ALL: [Self; BUTTON_COUNT] = [
        Self::Open,
        Self::Previous,
        Self::Next,
        Self::Minus,
        Self::Plus,
        Self::Fit,
        Self::Actual,
        Self::Rotate,
        Self::Undo,
        Self::Export,
        Self::More,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "打开图片 · Ctrl+O",
            Self::Previous => "上一张 · ← / PageUp",
            Self::Next => "下一张 · → / PageDown",
            Self::Minus => "缩小",
            Self::Plus => "放大",
            Self::Fit => "适应窗口 · F",
            Self::Actual => "实际像素 1:1 · 1",
            Self::Rotate => "右转 90° · R",
            Self::Undo => "撤销 · Ctrl+Z",
            Self::Export => "导出新副本 · Ctrl+Shift+S",
            Self::More => "更多：编辑、列表、来源、背景 · F10",
            Self::List => "图片列表",
            Self::Crop => "裁剪 / 缩放",
            Self::Info => "信息 / 安全来源",
            Self::RotateLeft => "左转 90° · Shift+R",
            Self::Fullscreen => "全屏 · F11",
            Self::Flip => "水平翻转",
            Self::Refresh => "刷新图片列表",
            Self::Locate => "定位当前图片",
            Self::Restore => "恢复原图",
            Self::Recheck => "重新核验来源",
            Self::CropApply => "应用裁剪",
            Self::ResizeApply => "应用缩放",
            Self::Close => "关闭 / 取消",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolbarLayout {
    pub buttons: [Rect; BUTTON_COUNT],
    pub separators: Vec<f32>,
    pub metadata: Option<Rect>,
    pub zoom: Option<Rect>,
}
impl ToolbarLayout {
    pub fn new(area: Rect) -> Self {
        let full = area.width() >= 720.0;
        let compact = area.width() < 520.0;
        let side = if compact { 28.0 } else { 34.0 };
        let gap = if compact { 1.0 } else { 4.0 };
        let separator = if compact { 6.0 } else { 12.0 };
        let width = BUTTON_COUNT as f32 * side + (BUTTON_COUNT - 1) as f32 * gap + 3.0 * separator;
        let mut x = area.center().x - width / 2.0;
        let y = area.center().y - side / 2.0;
        let mut separators = Vec::new();
        let buttons = std::array::from_fn(|index| {
            if [1, 3, 7].contains(&index) {
                separators.push(x + separator / 2.0 - gap / 2.0);
                x += separator;
            }
            let rect = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(side));
            x += side + gap;
            rect
        });
        Self {
            metadata: full.then(|| {
                Rect::from_min_max(
                    area.min + Vec2::new(12.0, 0.0),
                    Pos2::new(buttons[0].left() - 10.0, area.bottom()),
                )
            }),
            zoom: full.then(|| {
                Rect::from_min_max(
                    Pos2::new(buttons[BUTTON_COUNT - 1].right() + 10.0, area.top()),
                    area.max - Vec2::new(12.0, 0.0),
                )
            }),
            buttons,
            separators,
        }
    }
}

pub fn edge_label(ui: &mut egui::Ui, rect: Rect, text: &str, right: bool) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.id().with(("viewer-metadata", right)),
        egui::Sense::hover(),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    ui.painter_at(rect).text(
        if right {
            rect.right_center()
        } else {
            rect.left_center()
        },
        if right {
            egui::Align2::RIGHT_CENTER
        } else {
            egui::Align2::LEFT_CENTER
        },
        text,
        egui::FontId::proportional(13.0),
        Color32::from_gray(74),
    );
    response
}

pub fn icon_id(icon: Icon) -> egui::Id {
    egui::Id::new("viewer-icon").with(icon as u8)
}
pub fn toolbar_has_focus(ctx: &egui::Context) -> bool {
    ctx.memory(|memory| {
        memory
            .focused()
            .is_some_and(|focused| Icon::ALL.into_iter().any(|icon| icon_id(icon) == focused))
    })
}

/// Compact command focus also permits the viewer's advertised shortcuts.
/// Text fields and menu choices are deliberately excluded.
pub fn command_has_focus(ctx: &egui::Context) -> bool {
    toolbar_has_focus(ctx)
        || ctx.memory(|m| m.focused()).is_some_and(|focused| {
            ctx.data(|data| data.get_temp::<egui::Id>(egui::Id::new("viewer-command-focus")))
                == Some(focused)
        })
}

pub fn icon_button(
    ui: &mut egui::Ui,
    rect: Rect,
    icon: Icon,
    enabled: bool,
    selected: bool,
) -> egui::Response {
    let response = ui.interact(
        rect,
        icon_id(icon),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, icon.label()));
    let color = if !enabled {
        Color32::from_gray(183)
    } else {
        Color32::from_gray(47)
    };
    if selected || (enabled && response.hovered()) {
        ui.painter().rect_filled(
            rect,
            5.0,
            Color32::from_gray(if selected { 228 } else { 241 }),
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            5.0,
            egui::Stroke::new(1.5, Color32::from_rgb(39, 99, 151)),
            egui::StrokeKind::Inside,
        );
    }
    paint_icon(ui.painter(), rect.center(), icon, color);
    action_tooltip(response, icon.label())
}

/// Names are always available to assistive technology; captions appear only on
/// hover or keyboard focus so command controls remain visually quiet.
fn action_tooltip(response: egui::Response, help: &str) -> egui::Response {
    if response.has_focus() {
        response.show_tooltip_text(help);
        response
    } else {
        response.on_hover_text(help)
    }
}

pub const ACTION_SIZE: f32 = 32.0;

pub struct CompactAction<'a> {
    icon: Icon,
    name: &'a str,
    help: &'a str,
    selected: bool,
}
impl<'a> CompactAction<'a> {
    pub fn new(icon: Icon, name: &'a str) -> Self {
        Self {
            icon,
            name,
            help: name,
            selected: false,
        }
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub fn help(mut self, help: &'a str) -> Self {
        self.help = help;
        self
    }
}
impl egui::Widget for CompactAction<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let enabled = ui.is_enabled();
        let side = ui.available_width().clamp(28.0, ACTION_SIZE);
        let (_, rect) = ui.allocate_space(Vec2::splat(side));
        let response = ui.interact(
            rect,
            ui.make_persistent_id(("compact-action", self.name)),
            egui::Sense::click(),
        );
        if response.has_focus() {
            ui.ctx().data_mut(|data| {
                data.insert_temp(egui::Id::new("viewer-command-focus"), response.id)
            });
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, self.selected, self.name)
        });
        let visuals = ui.style().interact_selectable(&response, self.selected);
        if self.selected
            || (enabled && (response.hovered() || response.is_pointer_button_down_on()))
        {
            ui.painter().rect_filled(rect, 5.0, visuals.bg_fill);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                5.0,
                egui::Stroke::new(1.5, Color32::from_rgb(39, 99, 151)),
                egui::StrokeKind::Inside,
            );
        }
        paint_icon(
            ui.painter(),
            rect.center(),
            self.icon,
            if enabled {
                visuals.fg_stroke.color
            } else {
                Color32::from_gray(169)
            },
        );
        action_tooltip(response, self.help)
    }
}

/// Text stays visible and accessible; the accompanying vector is decoration.
pub struct ActionButton<'a> {
    icon: Icon,
    label: &'a str,
    selected: bool,
    framed: bool,
    full_width: bool,
}
impl<'a> ActionButton<'a> {
    pub fn new(icon: Icon, label: &'a str) -> Self {
        Self {
            icon,
            label,
            selected: false,
            framed: true,
            full_width: false,
        }
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub fn menu(mut self) -> Self {
        self.framed = false;
        self.full_width = true;
        self
    }
}
impl egui::Widget for ActionButton<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let enabled = ui.is_enabled();
        let font = egui::TextStyle::Button.resolve(ui.style());
        let galley =
            ui.painter()
                .layout_no_wrap(self.label.to_owned(), font, ui.visuals().text_color());
        let width = if self.full_width {
            ui.available_width()
        } else {
            galley.size().x + 38.0
        };
        let size = Vec2::new(
            width.min(ui.available_width()).max(28.0),
            ui.spacing()
                .interact_size
                .y
                .max(galley.size().y + if self.full_width { 4.0 } else { 8.0 }),
        );
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, self.selected, self.label)
        });
        let visuals = ui.style().interact_selectable(&response, self.selected);
        if self.framed || self.selected || response.hovered() {
            ui.painter().rect_filled(rect, 4.0, visuals.bg_fill);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                4.0,
                egui::Stroke::new(1.5, Color32::from_rgb(39, 99, 151)),
                egui::StrokeKind::Inside,
            );
        }
        let color = if enabled {
            visuals.fg_stroke.color
        } else {
            Color32::from_gray(169)
        };
        paint_icon(
            ui.painter(),
            Pos2::new(rect.left() + 16.0, rect.center().y),
            self.icon,
            color,
        );
        let text_rect = Rect::from_min_max(
            Pos2::new(rect.left() + 32.0, rect.top()),
            rect.max - Vec2::new(5.0, 0.0),
        );
        ui.painter_at(text_rect).galley(
            Pos2::new(text_rect.left(), rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
        response.on_hover_text(self.label)
    }
}

fn paint_icon(painter: &egui::Painter, center: Pos2, icon: Icon, color: Color32) {
    let p = |x, y| center + Vec2::new(x, y);
    let stroke = egui::Stroke::new(1.5, color);
    let line = |points: &[(f32, f32)]| {
        painter.add(egui::Shape::line(
            points.iter().map(|&(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };
    match icon {
        Icon::Open => line(&[
            (-9., 7.),
            (-9., -6.),
            (-3., -6.),
            (0., -3.),
            (9., -3.),
            (9., 7.),
            (-9., 7.),
        ]),
        Icon::Previous => line(&[(3., -8.), (-4., 0.), (3., 8.)]),
        Icon::Next => line(&[(-3., -8.), (4., 0.), (-3., 8.)]),
        Icon::Minus | Icon::Plus => {
            line(&[(-7., 0.), (7., 0.)]);
            if icon == Icon::Plus {
                line(&[(0., -7.), (0., 7.)]);
            }
        }
        Icon::Fit => {
            for (x, y) in [(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)] {
                line(&[(x * 3., y * 8.), (x * 8., y * 8.), (x * 8., y * 3.)]);
            }
        }
        Icon::Actual => {
            painter.rect_stroke(
                Rect::from_center_size(center, Vec2::new(21., 17.)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                "1:1",
                egui::FontId::proportional(10.0),
                color,
            );
        }
        Icon::Rotate | Icon::RotateLeft => {
            let direction = if icon == Icon::RotateLeft { -1.0 } else { 1.0 };
            let points = (0..=24)
                .map(|i| {
                    let a = 0.70 + i as f32 / 24.0 * 5.05;
                    p(direction * a.cos() * 8.0, a.sin() * 8.0)
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
            line(&[
                (direction * 1., -5.),
                (direction * 7., -4.),
                (direction * 7., -10.),
            ]);
        }
        Icon::Undo => {
            line(&[(-3., -8.), (-9., -2.), (-3., 4.)]);
            line(&[
                (-9., -2.),
                (2., -2.),
                (5., -1.),
                (7., 2.),
                (7., 5.),
                (5., 8.),
            ]);
        }
        Icon::Refresh => {
            for direction in [-1.0, 1.0] {
                let points = (0..=12)
                    .map(|i| {
                        let a = -2.8 + i as f32 / 12.0 * 2.5;
                        p(direction * a.cos() * 7.5, direction * a.sin() * 7.5)
                    })
                    .collect();
                painter.add(egui::Shape::line(points, stroke));
                line(&[
                    (direction * 2.5, direction * -3.),
                    (direction * 7.5, direction * -2.),
                    (direction * 8.5, direction * -7.),
                ]);
            }
        }
        Icon::Locate => {
            painter.circle_stroke(center, 6.0, stroke);
            painter.circle_filled(center, 1.8, color);
            for direction in [-1.0, 1.0] {
                line(&[(direction * 6., 0.), (direction * 10., 0.)]);
                line(&[(0., direction * 6.), (0., direction * 10.)]);
            }
        }
        Icon::Restore => {
            line(&[(-8., -1.), (-8., 8.), (8., 8.), (8., -8.), (-1., -8.)]);
            line(&[(-5., 5.), (-1., 1.), (2., 4.), (4., 2.), (6., 5.)]);
            line(&[(-4., -11.), (-8., -7.), (-4., -3.)]);
            line(&[(-8., -7.), (-1., -7.)]);
        }
        Icon::Recheck => {
            line(&[
                (0., -10.),
                (8., -7.),
                (7., 2.),
                (4., 7.),
                (0., 10.),
                (-4., 7.),
                (-7., 2.),
                (-8., -7.),
                (0., -10.),
            ]);
            line(&[(-4., -1.), (-1., 3.), (4., -3.)]);
        }
        Icon::CropApply => {
            line(&[(-5., -10.), (-5., 3.), (1., 3.)]);
            line(&[(-10., -5.), (3., -5.), (3., 0.)]);
            line(&[(1., 7.), (4., 10.), (10., 3.)]);
        }
        Icon::ResizeApply => {
            line(&[(9., -2.), (9., -9.), (-9., -9.), (-9., 9.), (-2., 9.)]);
            line(&[(-5., 3.), (4., -6.)]);
            line(&[(-1., -6.), (4., -6.), (4., -1.)]);
            line(&[(1., 7.), (4., 10.), (10., 3.)]);
        }
        Icon::Export => {
            line(&[(-8., 1.), (-8., 8.), (8., 8.), (8., 1.)]);
            line(&[(0., 3.), (0., -8.)]);
            line(&[(-4., -4.), (0., -8.), (4., -4.)]);
        }
        Icon::List => {
            for y in [-6.0, 0.0, 6.0] {
                painter.circle_filled(p(-7.0, y), 1.2, color);
                line(&[(-2.0, y), (8.0, y)]);
            }
        }
        Icon::Crop => {
            line(&[(-5.0, -10.0), (-5.0, 5.0), (10.0, 5.0)]);
            line(&[(-10.0, -5.0), (5.0, -5.0), (5.0, 10.0)]);
        }
        Icon::Info => {
            painter.circle_stroke(center, 8.5, stroke);
            painter.circle_filled(p(0.0, -4.0), 1.0, color);
            line(&[(0.0, -0.5), (0.0, 5.0)]);
        }
        Icon::Fullscreen => {
            for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                line(&[(x * 3.0, y * 8.0), (x * 8.0, y * 8.0), (x * 8.0, y * 3.0)]);
                line(&[(x * 3.0, y * 3.0), (x * 8.0, y * 8.0)]);
            }
        }
        Icon::Flip => {
            line(&[(-3.0, -7.0), (-9.0, 7.0), (-3.0, 7.0), (-3.0, -7.0)]);
            line(&[(3.0, -7.0), (9.0, 7.0), (3.0, 7.0), (3.0, -7.0)]);
        }
        Icon::Close => {
            line(&[(-6.0, -6.0), (6.0, 6.0)]);
            line(&[(-6.0, 6.0), (6.0, -6.0)]);
        }
        Icon::More => {
            for x in [-6., 0., 6.] {
                painter.circle_filled(p(x, 0.), 1.5, color);
            }
        }
    }
}

pub fn paint_checker(painter: &egui::Painter, area: Rect, colors: ([u8; 3], [u8; 3])) {
    let (a, b) = colors;
    painter.rect_filled(area, 0.0, Color32::from_rgb(a[0], a[1], a[2]));
    let cell = 16.0;
    let mut mesh = egui::Mesh::default();
    for y in 0..(area.height() / cell).ceil() as usize {
        for x in 0..(area.width() / cell).ceil() as usize {
            if (x + y) % 2 == 1 {
                let rect = Rect::from_min_size(
                    area.min + Vec2::new(x as f32 * cell, y as f32 * cell),
                    Vec2::splat(cell),
                )
                .intersect(area);
                mesh.add_colored_rect(rect, Color32::from_rgb(b[0], b[1], b[2]));
            }
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;
    fn compact_frame(
        ctx: &egui::Context,
        width: f32,
        enabled: bool,
        selected: bool,
        time: f64,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, egui::Response) {
        let mut response = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 200.0))),
                time: Some(time),
                focused: true,
                events,
                ..Default::default()
            },
            |ui| {
                ui.set_width(width);
                response = Some(
                    ui.add_enabled(
                        enabled,
                        CompactAction::new(Icon::CropApply, "应用裁剪")
                            .help("应用裁剪 · 确认选区")
                            .selected(selected),
                    ),
                );
            },
        );
        output.textures_delta.clear(); // These synthetic tests intentionally do not upload textures.
        (output, response.unwrap())
    }
    fn texts(output: &egui::FullOutput) -> Vec<String> {
        fn collect(shape: &egui::Shape, found: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => found.push(text.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, found)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut found);
        }
        found
    }
    #[test]
    fn compact_actions_are_icon_only_with_named_accessible_28_to_32_point_targets() {
        for width in [28.0, 30.0, 32.0, 240.0] {
            for enabled in [true, false] {
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                compact_frame(&ctx, width, enabled, false, 0.0, vec![]);
                let (output, response) = compact_frame(&ctx, width, enabled, false, 0.1, vec![]);
                assert_eq!(response.rect.size(), Vec2::splat(width.clamp(28.0, 32.0)));
                assert!(
                    texts(&output).is_empty(),
                    "an idle action painted a caption"
                );
                let node = &output
                    .platform_output
                    .accesskit_update
                    .as_ref()
                    .unwrap()
                    .nodes
                    .iter()
                    .find(|(id, _)| *id == response.id.accesskit_id())
                    .unwrap()
                    .1;
                assert_eq!(node.label(), Some("应用裁剪"));
                assert_eq!(node.role(), egui::accesskit::Role::Button);
                assert_eq!(node.is_disabled(), !enabled);
            }
        }
    }
    #[test]
    fn compact_action_tooltip_appears_for_hover_and_keyboard_focus() {
        let ctx = egui::Context::default();
        let (_, response) = compact_frame(&ctx, 240.0, true, false, 0.0, vec![]);
        compact_frame(
            &ctx,
            240.0,
            true,
            false,
            0.1,
            vec![egui::Event::PointerMoved(response.rect.center())],
        );
        let mut hover_text = Vec::new();
        for time in [1.0, 1.5, 2.0] {
            hover_text = texts(&compact_frame(&ctx, 240.0, true, false, time, vec![]).0);
        }
        assert!(hover_text.iter().any(|s| s == "应用裁剪 · 确认选区"));
        compact_frame(
            &ctx,
            240.0,
            true,
            false,
            2.1,
            vec![egui::Event::PointerGone],
        );
        ctx.memory_mut(|m| m.request_focus(response.id));
        compact_frame(&ctx, 240.0, true, false, 2.2, vec![]);
        let (output, response) = compact_frame(&ctx, 240.0, true, false, 2.3, vec![]);
        assert!(response.has_focus());
        assert!(texts(&output).iter().any(|s| s == "应用裁剪 · 确认选区"));
    }
    #[test]
    fn compact_action_pointer_keyboard_selected_and_disabled_states_work() {
        for enabled in [true, false] {
            let ctx = egui::Context::default();
            let (idle, response) = compact_frame(&ctx, 240.0, enabled, false, 0.0, vec![]);
            let (selected, _) = compact_frame(&ctx, 240.0, enabled, true, 0.1, vec![]);
            assert_ne!(
                format!("{:?}", idle.shapes),
                format!("{:?}", selected.shapes)
            );
            for pressed in [true, false] {
                let (_, result) = compact_frame(
                    &ctx,
                    240.0,
                    enabled,
                    false,
                    if pressed { 0.2 } else { 0.3 },
                    vec![
                        egui::Event::PointerMoved(response.rect.center()),
                        egui::Event::PointerButton {
                            pos: response.rect.center(),
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
                if !pressed {
                    assert_eq!(result.clicked(), enabled);
                }
            }
            ctx.memory_mut(|m| m.request_focus(response.id));
            compact_frame(
                &ctx,
                240.0,
                enabled,
                false,
                0.4,
                vec![egui::Event::PointerGone],
            );
            let (_, result) = compact_frame(
                &ctx,
                240.0,
                enabled,
                false,
                0.5,
                [true, false]
                    .into_iter()
                    .map(|pressed| egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: Some(egui::Key::Enter),
                        pressed,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    })
                    .collect(),
            );
            assert_eq!(result.clicked(), enabled);
        }
    }
    #[test]
    fn distinct_actions_have_distinct_vector_glyphs() {
        let ctx = egui::Context::default();
        let icons = [
            Icon::Undo,
            Icon::RotateLeft,
            Icon::Rotate,
            Icon::Restore,
            Icon::Refresh,
            Icon::Recheck,
            Icon::Crop,
            Icon::CropApply,
            Icon::ResizeApply,
        ];
        let mut glyphs = Vec::new();
        for icon in icons {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                paint_icon(ui.painter(), Pos2::new(20.0, 20.0), icon, Color32::BLACK);
            });
            output.textures_delta.clear();
            let shapes = format!("{:?}", output.shapes);
            assert!(!glyphs.contains(&shapes), "duplicate glyph for {icon:?}");
            assert!(texts(&output).is_empty());
            glyphs.push(shapes);
        }
    }
    #[test]
    fn empty_brand_mark_stays_in_high_dpi_minimum_canvas() {
        for size in [Vec2::new(800.0, 498.0), Vec2::new(400.0, 223.0)] {
            let area = Rect::from_min_size(Pos2::ZERO, size);
            assert!(area.contains_rect(empty_brand_rect(area)));
        }
    }
    #[test]
    fn branded_window_icon_is_small_rgba_and_has_transparent_corners() {
        let icon = window_icon();
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);
        assert_eq!(icon.rgba[3], 0);
        assert!(
            icon.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] == 255)
        );
        assert_eq!(PRODUCT_NAME, "gloamere-image");
    }
    #[test]
    fn toolbar_rectangles_fit_single_row_at_supported_sizes_and_high_dpi() {
        for width in [400.0, 533.3, 720.0, 800.0, 1180.0, 1600.0] {
            let area = Rect::from_min_size(Pos2::new(0., 498.), Vec2::new(width, TOOLBAR_HEIGHT));
            let layout = ToolbarLayout::new(area);
            for rect in &layout.buttons {
                assert!(area.contains_rect(*rect), "width={width}, rect={rect:?}");
                assert!(rect.width() >= 28.0);
            }
            for pair in layout.buttons.windows(2) {
                assert!(pair[0].right() < pair[1].left());
            }
            if let Some(metadata) = layout.metadata {
                assert!(metadata.right() < layout.buttons[0].left());
            }
            if let Some(zoom) = layout.zoom {
                assert!(zoom.left() > layout.buttons[10].right());
            }
        }
    }
    #[test]
    fn all_icons_have_distinct_accessible_names() {
        let mut names: Vec<_> = Icon::ALL.into_iter().map(Icon::label).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), BUTTON_COUNT);
        assert!(names.iter().all(|name| !name.is_empty()));
    }
}
