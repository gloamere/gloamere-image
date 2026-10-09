use crate::{Cancellation, Result, ToolError, pipeline::validate_dimensions};
use image::{
    RgbaImage,
    imageops::{self, FilterType},
};

#[derive(Clone, Debug)]
pub enum EditOp {
    Rotate90,
    Rotate270,
    FlipHorizontal,
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Resize {
        width: u32,
        height: u32,
    },
}

pub fn apply(
    image: &RgbaImage,
    op: &EditOp,
    cancel: &Cancellation,
) -> Result<crate::memory::Pixels> {
    cancel.check()?;
    let mut reservation = crate::memory::reserve(extra_bytes(image, op)?)?;
    let result = match *op {
        EditOp::Rotate90 => imageops::rotate90(image),
        EditOp::Rotate270 => imageops::rotate270(image),
        EditOp::FlipHorizontal => imageops::flip_horizontal(image),
        EditOp::Crop {
            x,
            y,
            width,
            height,
        } => {
            validate_dimensions(width, height)?;
            if x.checked_add(width).is_none_or(|v| v > image.width())
                || y.checked_add(height).is_none_or(|v| v > image.height())
            {
                return Err(ToolError::Invalid("裁剪区域必须在图片内".into()));
            }
            imageops::crop_imm(image, x, y, width, height).to_image()
        }
        EditOp::Resize { width, height } => {
            validate_dimensions(width, height)?;
            // Premultiply alpha so invisible RGB cannot bleed into visible edges.
            let mut premultiplied = image.clone();
            for p in premultiplied.pixels_mut() {
                for c in 0..3 {
                    p[c] = ((u16::from(p[c]) * u16::from(p[3]) + 127) / 255) as u8;
                }
            }
            let mut resized = imageops::resize(&premultiplied, width, height, FilterType::Lanczos3);
            for p in resized.pixels_mut() {
                if p[3] == 0 {
                    p[0] = 0;
                    p[1] = 0;
                    p[2] = 0;
                } else {
                    for c in 0..3 {
                        p[c] = ((u32::from(p[c]) * 255 + u32::from(p[3]) / 2) / u32::from(p[3]))
                            .min(255) as u8;
                    }
                }
            }
            resized
        }
    };
    cancel.check()?;
    crate::memory::Pixels::from_reserved(result, &mut reservation)
}

pub fn extra_bytes(image: &RgbaImage, op: &EditOp) -> Result<u64> {
    let (w, h) = image.dimensions();
    Ok(match *op {
        EditOp::Resize { width, height } => {
            validate_dimensions(width, height)?;
            crate::memory::resize_extra(w, h, width, height)
        }
        EditOp::Crop { width, height, .. } => {
            validate_dimensions(width, height)?;
            crate::memory::rgba_bytes(width, height) + crate::memory::OVERHEAD
        }
        _ => crate::memory::rgba_bytes(w, h) + crate::memory::OVERHEAD,
    })
}
