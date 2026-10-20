#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod app;
mod crop_selection;
mod viewer_ui;
use eframe::{
    egui,
    egui_wgpu::{WgpuConfiguration, WgpuSetupCreateNew},
};
fn configure_taskbar_identity() {
    #[cfg(windows)]
    {
        // Match the shortcut's stable AppUserModelID without touching user pins
        // or file defaults. Windows requires this before presenting any UI.
        let identity: Vec<u16> = "LightImageTool.Image"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let result = unsafe {
            windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(
                identity.as_ptr(),
            )
        };
        if result < 0 {
            log::warn!("Could not configure gloamere-image taskbar identity: {result:#x}");
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();
    let worker_args: Vec<_> = std::env::args_os().skip(1).collect();
    configure_taskbar_identity();
    let _runtime_guard = match light_image_tool::updater::launch::RuntimeGuard::acquire() {
        Ok(guard) => guard,
        Err(error) => {
            if worker_args
                .first()
                .is_some_and(|a| a == "--ai-pixel-worker")
            {
                std::process::exit(2);
            }
            rfd::MessageDialog::new()
                .set_title("gloamere-image · 更新维护")
                .set_description(error.to_string())
                .set_level(rfd::MessageLevel::Error)
                .show();
            std::process::exit(1);
        }
    };
    if worker_args
        .first()
        .is_some_and(|a| a == "--ai-pixel-worker")
    {
        let result = match worker_args.as_slice() {
            [_, pack] => light_image_tool::ai::model::worker_main(std::path::Path::new(pack)),
            _ => Err(light_image_tool::ToolError::Invalid(
                "模型 worker 参数无效".into(),
            )),
        };
        if result.is_err() {
            std::process::exit(2);
        }
        return;
    }

    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let is_smoke = args.first().is_some_and(|a| a == "--smoke");
    let initial = if is_smoke {
        args.get(2).map(std::path::PathBuf::from)
    } else {
        match light_image_tool::cli::open_path(&args) {
            Ok(path) => path,
            Err(error) => {
                log::error!("{error}");
                rfd::MessageDialog::new()
                    .set_title("gloamere-image · 打开图片")
                    .set_description(error.to_string())
                    .set_level(rfd::MessageLevel::Error)
                    .show();
                std::process::exit(2);
            }
        }
    };
    let smoke = if is_smoke {
        args.get(1).map(|dir| {
            let mut smoke = app::Smoke::new(
                std::path::PathBuf::from(dir),
                args.get(3)
                    .and_then(|v| v.to_str()?.parse::<f32>().ok())
                    .unwrap_or(1.0)
                    .clamp(0.5, 3.0),
                args.iter().any(|v| v == "--show-info"),
            );
            smoke.idle_seconds = args
                .iter()
                .position(|v| v == "--idle-seconds")
                .and_then(|i| args.get(i + 1))
                .and_then(|v| v.to_str()?.parse::<u64>().ok())
                .unwrap_or(0)
                .min(60);
            smoke.show_more = args.iter().any(|v| v == "--show-more");
            smoke.inspect_pixels = args.iter().any(|v| v == "--inspect-pixels");
            smoke.alpha_view = args.iter().any(|v| v == "--alpha-view");
            smoke.nearest_pixels = args.iter().any(|v| v == "--nearest-pixels");
            smoke.reference_on_top = args.iter().any(|v| v == "--reference-on-top");
            smoke.show_sticker = args.iter().any(|v| v == "--show-sticker");
            smoke.show_context_menu = args.iter().any(|v| v == "--show-context-menu");
            smoke.show_rename = args.iter().any(|v| v == "--show-rename");
            smoke.sticker_zoom_smoke = args.iter().any(|v| v == "--sticker-zoom-smoke");
            smoke.sticker_image = args
                .iter()
                .position(|v| v == "--sticker-image")
                .and_then(|i| args.get(i + 1))
                .map(std::path::PathBuf::from);
            smoke.compare_image = args
                .iter()
                .position(|v| v == "--compare-image")
                .and_then(|index| args.get(index + 1))
                .map(std::path::PathBuf::from);
            smoke.show_edit = args.iter().any(|v| v == "--show-edit");
            smoke.show_export = args.iter().any(|v| v == "--show-export");
            smoke.show_error = args.iter().any(|v| v == "--show-error");
            smoke.jpeg_preview = args.iter().any(|v| v == "--jpeg-preview");
            smoke.crop_preview = args.iter().any(|v| v == "--crop-preview");
            smoke.hide_list =
                !args.iter().any(|v| v == "--show-list") || args.iter().any(|v| v == "--hide-list");
            smoke.background = args
                .iter()
                .position(|v| v == "--background")
                .and_then(|i| args.get(i + 1))
                .and_then(|v| match v.to_str()? {
                    "dark" => Some(light_image_tool::settings::CanvasBackground::Dark),
                    "light" => Some(light_image_tool::settings::CanvasBackground::Light),
                    "checker_dark" => {
                        Some(light_image_tool::settings::CanvasBackground::CheckerDark)
                    }
                    "checker_light" => {
                        Some(light_image_tool::settings::CanvasBackground::CheckerLight)
                    }
                    _ => None,
                });
            smoke.cjk_fallback = args.iter().any(|v| v == "--cjk-fallback");
            smoke.destroy_during_export =
                args.iter().any(|v| v == "--destroy-device-during-export");
            smoke.destroy_device =
                smoke.destroy_during_export || args.iter().any(|v| v == "--destroy-device");
            smoke.workflow_image = args
                .iter()
                .position(|v| v == "--workflow-image")
                .and_then(|i| args.get(i + 1))
                .map(std::path::PathBuf::from);
            smoke.workflow_opens = args
                .iter()
                .position(|v| v == "--workflow-opens")
                .and_then(|i| args.get(i + 1))
                .and_then(|v| v.to_str()?.parse::<u16>().ok())
                .unwrap_or(4)
                .clamp(4, 1000)
                & !1;
            smoke.workflow_delay_ms = args
                .iter()
                .position(|v| v == "--workflow-delay-ms")
                .and_then(|i| args.get(i + 1))
                .and_then(|v| v.to_str()?.parse::<u64>().ok())
                .unwrap_or(0)
                .min(2000);
            smoke
        })
    } else {
        None
    };
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    #[cfg(windows)]
    {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
        setup
            .instance_descriptor
            .backend_options
            .dx12
            .presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
    }
    #[cfg(target_os = "macos")]
    {
        setup.instance_descriptor.backends = wgpu::Backends::METAL;
    }
    if is_smoke && args.iter().any(|v| v == "--gpu-init-failure") {
        setup.instance_descriptor.backends = wgpu::Backends::empty();
    }
    setup.power_preference = wgpu::PowerPreference::LowPower;
    let memory_usage_hint = (cfg!(windows)
        && !(is_smoke && args.iter().any(|v| v == "--gpu-performance")))
        || (is_smoke && args.iter().any(|v| v == "--gpu-memory-usage"));
    if memory_usage_hint {
        let default_descriptor = setup.device_descriptor.clone();
        setup.device_descriptor = std::sync::Arc::new(move |adapter| {
            let mut descriptor = default_descriptor(adapter);
            descriptor.memory_hints = wgpu::MemoryHints::MemoryUsage;
            descriptor
        });
    }
    log::info!(
        "GPU memory hint: {}",
        if memory_usage_hint {
            "MemoryUsage (backend may ignore)"
        } else {
            "default Performance"
        }
    );
    let window_size = if is_smoke && args.iter().any(|v| v == "--small-window") {
        [800.0, 550.0]
    } else {
        [1180.0, 780.0]
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(viewer_ui::window_icon())
            .with_inner_size(window_size)
            .with_decorations(false)
            .with_resizable(true)
            .with_transparent(true)
            .with_min_inner_size([360.0, 320.0]),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: WgpuConfiguration {
            wgpu_setup: setup.into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let diagnostic_directory = smoke.as_ref().map(|s| s.directory.clone());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eframe::run_native(
            "gloamere-image（未签名测试版）",
            options,
            Box::new(move |cc| Ok(Box::new(app::ImageApp::new(cc, initial, smoke)))),
        )
    }));
    log::info!("shutdown: eframe returned; renderer/window teardown completed");
    let failure = match outcome {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some((false, error.to_string())),
        Err(payload) => {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_else(|| "renderer/window panic".into());
            Some((true, message))
        }
    };
    if let Some((panic_caught, error)) = failure {
        log::error!(
            "GPU/window failure (initialization or runtime); panic_caught={panic_caught}: {error}"
        );
        if let Some(directory) = diagnostic_directory {
            let report = serde_json::json!({"failure_reported":true,"panic_caught_by_boundary":panic_caught,"error":error,"exit_code":1,"device_recreated":false,"native_error_dialog_tested":false});
            let _ = std::fs::write(
                directory.join("runtime-failure.json"),
                serde_json::to_vec_pretty(&report).unwrap_or_default(),
            );
        }
        if !is_smoke {
            rfd::MessageDialog::new()
                .set_title("gloamere-image窗口无法继续运行")
                .set_description(format!("GPU / 窗口错误：{error}\n请重新打开应用。本应用不会覆盖原图；未完成的导出请检查后重试。"))
                .set_level(rfd::MessageLevel::Error)
                .show();
        }
        std::process::exit(1);
    }
}
