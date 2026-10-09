use image::{Rgba, RgbaImage};
fn main() {
    let out = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .expect("output dir");
    std::fs::create_dir_all(&out).unwrap();
    let image = RgbaImage::from_fn(900, 600, |x, y| {
        let alpha = if (180..720).contains(&x) && (100..500).contains(&y) {
            255
        } else {
            ((x + y) % 256) as u8
        };
        Rgba([(x * 255 / 900) as u8, (y * 255 / 600) as u8, 180, alpha])
    });
    image.save(out.join("透明 中文素材.png")).unwrap();
    image::DynamicImage::ImageRgba8(image.clone())
        .to_rgb8()
        .save(out.join("photo.jpg"))
        .unwrap();
    let wide = RgbaImage::from_fn(12000, 100, |x, y| {
        Rgba([(x % 255) as u8, (y * 2) as u8, 80, 255])
    });
    wide.save(out.join("wide.png")).unwrap();
    let original = out.join("透明 中文素材.png");
    let loaded =
        light_image_tool::pipeline::load(&original, &light_image_tool::Cancellation::default())
            .unwrap();
    let edited = light_image_tool::edit::apply(
        &loaded.pixels,
        &light_image_tool::edit::EditOp::Rotate90,
        &light_image_tool::Cancellation::default(),
    )
    .unwrap();
    let dest = out.join("rotated-export.png");
    light_image_tool::export::save_new(
        &edited,
        &original,
        &light_image_tool::export::ExportOptions {
            destination: dest,
            format: light_image_tool::export::ExportFormat::Png,
            quality: 92,
            matte: [255; 3],
        },
        &light_image_tool::Cancellation::default(),
    )
    .unwrap();
}
