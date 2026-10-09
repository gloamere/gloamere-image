//! CLI-only QA harness for the real shared pipeline; not a product interface.
use light_image_tool::{
    Cancellation,
    edit::{self, EditOp},
    export::{self, ExportFormat, ExportOptions},
    pipeline,
};
use std::{io::Write, path::PathBuf};
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() < 2 {
        return Err(
            "usage: core_workflow inspect INPUT | export INPUT OUTPUT [png|jpeg] [WIDTH HEIGHT]"
                .into(),
        );
    }
    let input = PathBuf::from(&args[1]);
    let cancel = Cancellation::default();
    let loaded = pipeline::load(&input, &cancel)?;
    if args[0] == "inspect" {
        println!(
            "{}",
            serde_json::json!({"dimensions":loaded.pixels.dimensions(),"alpha":loaded.has_alpha,"color_managed":loaded.color_managed,"load_ms":loaded.load_ms,"sha256":loaded.source_sha256,"owned_budget_bytes":light_image_tool::memory::in_use(),"owned_budget_peak_bytes":light_image_tool::memory::peak()})
        );
        return Ok(());
    }
    if args[0] != "export" || args.len() < 3 {
        return Err("invalid operation".into());
    }
    if !loaded.color_managed {
        return Err("ICC not calibrated; export blocked".into());
    }
    let pixels = if args.len() >= 6 {
        Some(edit::apply(
            &loaded.pixels,
            &EditOp::Resize {
                width: args[4].to_str().ok_or("width")?.parse()?,
                height: args[5].to_str().ok_or("height")?.parse()?,
            },
            &cancel,
        )?)
    } else {
        None
    };
    let image: &image::RgbaImage = pixels.as_deref().unwrap_or(&loaded.pixels);
    let options = ExportOptions {
        destination: PathBuf::from(&args[2]),
        format: if args.get(3).is_some_and(|a| a == "jpeg") {
            ExportFormat::Jpeg
        } else {
            ExportFormat::Png
        },
        quality: 92,
        matte: [255; 3],
    };
    println!("export-start");
    std::io::stdout().flush()?;
    let destination = export::save_new(image, &input, &options, &cancel)?;
    println!(
        "{}",
        serde_json::json!({"exported":destination,"dimensions":image.dimensions(),"owned_budget_peak_bytes":light_image_tool::memory::peak()})
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
