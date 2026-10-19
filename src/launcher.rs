#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let silent = args
        .first()
        .is_some_and(|a| a == "--installer-transaction" || a == "--update-helper");
    match light_image_tool::updater::launch::run(args) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            log::error!("{error}");
            if !silent {
                rfd::MessageDialog::new()
                    .set_title("gloamere-image · 启动与恢复")
                    .set_description(error.to_string())
                    .set_level(rfd::MessageLevel::Error)
                    .show();
            }
            std::process::exit(1);
        }
    }
}
