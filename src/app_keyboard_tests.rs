//! Headless regressions for viewer toolbar and More-menu focus isolation.
//! These tests never activate the native Open dialog: Enter is used only with
//! Rotate initially focused and the menu's internal path dialog as the target.
use super::*;

fn make() -> (ImageApp, egui::Context) {
    let ctx = egui::Context::default();
    configure(&ctx);
    let mut app = ImageApp::from_context(ctx.clone(), "keyboard-review".into(), None, None);
    run(&mut app, &ctx, Vec::new());
    run(&mut app, &ctx, Vec::new());
    (app, ctx)
}

fn run(app: &mut ImageApp, ctx: &egui::Context, events: Vec<egui::Event>) {
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 550.0))),
        focused: true,
        events,
        ..Default::default()
    };
    let mut frame = eframe::Frame::_new_kittest();
    let mut output = ctx.run_ui(input, |ui| eframe::App::ui(app, ui, &mut frame));
    output.textures_delta.clear(); // No GPU upload in the headless harness.
}

fn key(app: &mut ImageApp, ctx: &egui::Context, key: egui::Key, shift: bool) {
    let modifiers = egui::Modifiers {
        shift,
        ..egui::Modifiers::NONE
    };
    run(
        app,
        ctx,
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers,
            },
        ],
    );
}

fn click(app: &mut ImageApp, ctx: &egui::Context, pos: Pos2) {
    for pressed in [true, false] {
        run(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

fn assert_more_focus(ctx: &egui::Context) {
    assert_eq!(
        ctx.memory(|memory| memory.focused()),
        Some(viewer_ui::icon_id(Icon::More)),
        "closing More should return keyboard focus to its toolbar button"
    );
}

#[test]
fn f10_opens_more_after_tab_focuses_toolbar() {
    let (mut app, ctx) = make();
    key(&mut app, &ctx, egui::Key::Tab, false);
    assert!(viewer_ui::toolbar_has_focus(&ctx));
    assert!(!ctx.text_edit_focused());
    key(&mut app, &ctx, egui::Key::F10, false);
    assert!(app.more_open, "toolbar focus must not block F10");
}

#[test]
fn f10_closes_more_from_menu_focus_and_restores_more_focus() {
    let (mut app, ctx) = make();
    key(&mut app, &ctx, egui::Key::F10, false);
    assert!(app.more_open);
    assert!(!viewer_ui::toolbar_has_focus(&ctx));
    assert!(ctx.memory(|memory| memory.focused()).is_some());
    key(&mut app, &ctx, egui::Key::F10, false);
    assert!(
        !app.more_open,
        "F10 must also work while a menu item owns focus"
    );
    run(&mut app, &ctx, Vec::new());
    assert_more_focus(&ctx);
    // The assertion above prevents Enter ever reaching the native Open button.
    key(&mut app, &ctx, egui::Key::Enter, false);
    assert!(app.more_open, "restored More focus should support Enter");
    assert!(
        !app.path_dialog,
        "one Enter must not also activate a menu action"
    );
}

#[test]
fn tab_and_shift_tab_remain_in_more_menu_layer() {
    let (mut app, ctx) = make();
    key(&mut app, &ctx, egui::Key::F10, false);
    let menu_layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("viewer-more-menu"));
    let mut focused_samples = 0;
    for shift in [false, true] {
        for _ in 0..24 {
            key(&mut app, &ctx, egui::Key::Tab, shift);
            assert!(app.more_open);
            if let Some(focused) = ctx.memory(|memory| memory.focused()) {
                let response = ctx.read_response(focused).expect("focused widget exists");
                assert_eq!(response.layer_id, menu_layer, "Tab escaped the menu");
                focused_samples += 1;
            }
        }
    }
    assert!(
        focused_samples > 0,
        "the menu must offer keyboard focus targets"
    );
}

#[test]
fn enter_after_f10_invokes_menu_without_rotating_background_image() {
    let (mut app, ctx) = make();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keyboard-review.png");
    image::RgbaImage::from_pixel(4, 3, image::Rgba([80, 90, 100, 128]))
        .save(&path)
        .unwrap();
    let loaded = light_image_tool::pipeline::load(&path, &Cancellation::default()).unwrap();
    app.pixels = Some(loaded.pixels.clone());
    app.loaded = Some(loaded);
    run(&mut app, &ctx, Vec::new());
    ctx.memory_mut(|memory| memory.request_focus(viewer_ui::icon_id(Icon::Rotate)));
    run(&mut app, &ctx, Vec::new());
    key(&mut app, &ctx, egui::Key::F10, false);
    assert!(app.more_open);
    key(&mut app, &ctx, egui::Key::Enter, false);
    assert!(
        app.work.is_none() && !app.edited,
        "Enter rotated behind More"
    );
    assert_eq!(app.dimensions(), [4, 3]);
    assert!(app.undo.is_empty());
    assert!(
        app.path_dialog,
        "Enter should activate the first menu action"
    );
    assert!(!app.more_open);
}

#[test]
fn repeated_more_click_opens_closes_and_reopens_menu() {
    let (mut app, ctx) = make();
    let pos = app.more_button_rect.unwrap().center();
    for expected_open in [true, false, true, false] {
        click(&mut app, &ctx, pos);
        assert_eq!(
            app.more_open, expected_open,
            "More click did not toggle its menu"
        );
        run(&mut app, &ctx, Vec::new());
    }
    // A pointer close can occur after the toolbar registered this frame's
    // modal layer. Allow at most three idle passes total for teardown and
    // deferred restoration; all four open/close states above remain immediate.
    for _ in 0..2 {
        if ctx.memory(|memory| memory.focused()) == Some(viewer_ui::icon_id(Icon::More)) {
            break;
        }
        run(&mut app, &ctx, Vec::new());
    }
    assert_more_focus(&ctx);
}

#[test]
fn escape_closes_only_more_and_restores_toolbar_focus() {
    let (mut app, ctx) = make();
    app.editing = true;
    app.crop_selecting = true;
    app.fullscreen = true;
    key(&mut app, &ctx, egui::Key::F10, false);
    assert!(app.more_open);
    key(&mut app, &ctx, egui::Key::Escape, false);
    assert!(!app.more_open);
    assert!(app.editing && app.crop_selecting && app.fullscreen);
    run(&mut app, &ctx, Vec::new());
    assert_more_focus(&ctx);
}

#[test]
fn outside_click_closes_more_and_restores_toolbar_focus() {
    let (mut app, ctx) = make();
    key(&mut app, &ctx, egui::Key::F10, false);
    run(&mut app, &ctx, Vec::new());
    let outside = Pos2::new(8.0, 8.0);
    assert!(!app.menu_rect.unwrap().contains(outside));
    click(&mut app, &ctx, outside);
    assert!(!app.more_open);
    run(&mut app, &ctx, Vec::new());
    assert_more_focus(&ctx);
}

#[test]
fn stable_more_menu_closes_when_its_toolbar_trigger_is_clicked_again() {
    let (mut app, ctx) = make();
    let position = app.more_button_rect.unwrap().center();
    for _ in 0..3 {
        click(&mut app, &ctx, position);
        assert!(app.more_open);
        for _ in 0..8 {
            run(&mut app, &ctx, Vec::new());
        }
        assert!(ctx.memory(|memory| memory.top_modal_layer()).is_some());
        click(&mut app, &ctx, position);
        assert!(!app.more_open, "stable modal must let its trigger close it");
        for _ in 0..3 {
            run(&mut app, &ctx, Vec::new());
        }
        assert_more_focus(&ctx);
    }
}

#[test]
fn more_trigger_press_closes_and_release_does_not_reopen() {
    let (mut app, ctx) = make();
    let position = app.more_button_rect.unwrap().center();
    key(&mut app, &ctx, egui::Key::F10, false);
    for _ in 0..8 {
        run(&mut app, &ctx, Vec::new());
    }
    for pressed in [true, false] {
        run(
            &mut app,
            &ctx,
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
        assert!(
            !app.more_open,
            "trigger press and matching release must both leave More closed"
        );
    }
    for _ in 0..3 {
        run(&mut app, &ctx, Vec::new());
    }
    assert_more_focus(&ctx);
    click(&mut app, &ctx, position);
    assert!(app.more_open, "a new pointer gesture can reopen More");
}

fn action_frame(
    app: &mut ImageApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut frame = eframe::Frame::_new_kittest();
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1180.0, 900.0))),
            focused: true,
            events,
            ..Default::default()
        },
        |ui| eframe::App::ui(app, ui, &mut frame),
    );
    output.textures_delta.clear();
    output
}

fn action_node(
    output: &egui::FullOutput,
    name: &str,
) -> (egui::accesskit::NodeId, egui::accesskit::Node) {
    output
        .platform_output
        .accesskit_update
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .find(|(_, node)| {
            node.role() == egui::accesskit::Role::Button && node.label() == Some(name)
        })
        .unwrap_or_else(|| panic!("missing accessible action {name}"))
        .clone()
}

fn click_action(app: &mut ImageApp, ctx: &egui::Context, name: &str) {
    let output = action_frame(app, ctx, vec![]);
    let (_, node) = action_node(&output, name);
    let bounds = node.bounds().unwrap();
    let pos = Pos2::new(
        ((bounds.x0 + bounds.x1) / 2.0) as f32,
        ((bounds.y0 + bounds.y1) / 2.0) as f32,
    );
    assert!(Rect::from_min_size(Pos2::ZERO, Vec2::new(1180.0, 900.0)).contains(pos));
    for pressed in [true, false] {
        action_frame(
            app,
            ctx,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}

fn focus_action(app: &mut ImageApp, ctx: &egui::Context, name: &str) {
    let output = action_frame(app, ctx, vec![]);
    let (id, _) = action_node(&output, name);
    action_frame(
        app,
        ctx,
        vec![
            egui::Event::PointerGone,
            egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Focus,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: id,
                data: None,
            }),
        ],
    );
    action_frame(app, ctx, vec![]);
    assert_eq!(
        ctx.memory(|m| m.focused()).map(|id| id.accesskit_id()),
        Some(id)
    );
}

fn action_key(app: &mut ImageApp, ctx: &egui::Context, key: egui::Key, modifiers: egui::Modifiers) {
    action_frame(
        app,
        ctx,
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers,
            },
        ],
    );
}

fn compact_editor() -> (tempfile::TempDir, ImageApp, egui::Context) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("compact-actions.png");
    image::RgbaImage::from_pixel(4, 3, image::Rgba([80, 90, 100, 128]))
        .save(&path)
        .unwrap();
    let (mut app, ctx) = make();
    let loaded = light_image_tool::pipeline::load(&path, &Cancellation::default()).unwrap();
    app.pixels = Some(loaded.pixels.clone());
    app.loaded = Some(loaded);
    app.editing = true;
    app.evidence_attempted = true;
    app.crop = [0, 0, 4, 3];
    app.resize = [4, 3];
    app.refresh_texture(&ctx);
    ctx.enable_accesskit();
    action_frame(&mut app, &ctx, vec![]);
    action_frame(&mut app, &ctx, vec![]);
    (directory, app, ctx)
}

fn finish_edit(app: &mut ImageApp, ctx: &egui::Context) {
    let start = Instant::now();
    while app.work.is_some() {
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        app.poll(ctx);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    action_frame(app, ctx, vec![]);
}

#[test]
fn focused_compact_editor_actions_support_advertised_shortcuts_and_more() {
    let (_directory, mut app, ctx) = compact_editor();
    focus_action(&mut app, &ctx, "顺时针旋转 90°");
    assert!(viewer_ui::command_has_focus(&ctx));
    action_key(&mut app, &ctx, egui::Key::R, egui::Modifiers::NONE);
    finish_edit(&mut app, &ctx);
    assert_eq!(app.dimensions(), [3, 4]);
    focus_action(&mut app, &ctx, "撤销");
    action_key(&mut app, &ctx, egui::Key::Z, egui::Modifiers::COMMAND);
    finish_edit(&mut app, &ctx);
    assert_eq!(app.dimensions(), [4, 3]);
    focus_action(&mut app, &ctx, "逆时针旋转 90°");
    action_key(&mut app, &ctx, egui::Key::R, egui::Modifiers::SHIFT);
    finish_edit(&mut app, &ctx);
    assert_eq!(app.dimensions(), [3, 4]);
    focus_action(&mut app, &ctx, "逆时针旋转 90°");
    action_key(&mut app, &ctx, egui::Key::F10, egui::Modifiers::NONE);
    assert!(app.more_open);
    action_key(&mut app, &ctx, egui::Key::Escape, egui::Modifiers::NONE);
    assert!(!app.more_open && app.editing);
}

#[test]
fn compact_editor_pointer_actions_preserve_undo_restore_resize_and_sticky_crop() {
    let (_directory, mut app, ctx) = compact_editor();
    let source = app.loaded.as_ref().unwrap().path.clone();
    let original = std::fs::read(&source).unwrap();
    click_action(&mut app, &ctx, "顺时针旋转 90°");
    finish_edit(&mut app, &ctx);
    assert_eq!(app.dimensions(), [3, 4]);
    click_action(&mut app, &ctx, "撤销");
    finish_edit(&mut app, &ctx);
    assert_eq!(app.dimensions(), [4, 3]);
    click_action(&mut app, &ctx, "逆时针旋转 90°");
    finish_edit(&mut app, &ctx);
    click_action(&mut app, &ctx, "恢复原图");
    assert_eq!(app.dimensions(), [4, 3]);
    assert!(app.undo.is_empty() && !app.edited);
    app.resize = [2, 2];
    click_action(&mut app, &ctx, "应用缩放");
    finish_edit(&mut app, &ctx);
    assert_eq!(app.dimensions(), [2, 2]);
    click_action(&mut app, &ctx, "恢复原图");
    app.crop = [0, 0, 2, 2];
    click_action(&mut app, &ctx, "拖拽选择裁剪");
    assert!(app.crop_selecting);
    click_action(&mut app, &ctx, "应用裁剪");
    finish_edit(&mut app, &ctx);
    assert_eq!(app.dimensions(), [2, 2]);
    assert_eq!(std::fs::read(source).unwrap(), original);
    click_action(&mut app, &ctx, "关闭面板");
    assert!(!app.editing && !app.info_open && !app.crop_selecting);
}

#[test]
fn dialogs_disable_background_compact_editor_and_list_commands() {
    for dialog in ["path", "export", "error"] {
        let (_directory, mut app, ctx) = compact_editor();
        app.preferences.list_visible = true;
        app.siblings = vec![app.loaded.as_ref().unwrap().path.clone()];
        app.crop_selecting = true;
        app.crop = [0, 0, 2, 2];
        match dialog {
            "path" => app.path_dialog = true,
            "export" => app.begin_export(),
            "error" => app.error = Some("test error".into()),
            _ => unreachable!(),
        }
        let output = action_frame(&mut app, &ctx, vec![]);
        for name in [
            "顺时针旋转 90°",
            "关闭面板",
            "应用裁剪",
            "刷新图片列表",
            "定位当前图片",
        ] {
            assert!(
                action_node(&output, name).1.is_disabled(),
                "{dialog}: {name}"
            );
        }
        click_action(&mut app, &ctx, "顺时针旋转 90°");
        click_action(&mut app, &ctx, "应用裁剪");
        click_action(&mut app, &ctx, "关闭面板");
        assert!(app.work.is_none() && !app.edited && app.editing && app.crop_selecting);
        assert_eq!(app.dimensions(), [4, 3]);
        action_key(&mut app, &ctx, egui::Key::Escape, egui::Modifiers::NONE);
        assert!(!app.path_dialog && !app.export_dialog && app.error.is_none());
        assert!(app.editing && app.crop_selecting);
    }
}
