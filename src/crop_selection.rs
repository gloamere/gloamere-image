use eframe::egui::{Pos2, Rect, Vec2};

pub fn image_point(rect: Rect, dimensions: [u32; 2], point: Pos2) -> Option<[f64; 2]> {
    let [width, height] = dimensions;
    if width == 0
        || height == 0
        || !rect.is_positive()
        || !rect.min.x.is_finite()
        || !rect.min.y.is_finite()
        || !rect.width().is_finite()
        || !rect.height().is_finite()
        || !point.x.is_finite()
        || !point.y.is_finite()
    {
        return None;
    }
    Some([
        ((f64::from(point.x) - f64::from(rect.min.x)) / f64::from(rect.width()) * f64::from(width))
            .clamp(0.0, f64::from(width)),
        ((f64::from(point.y) - f64::from(rect.min.y)) / f64::from(rect.height())
            * f64::from(height))
        .clamp(0.0, f64::from(height)),
    ])
}

pub fn from_points(dimensions: [u32; 2], start: [f64; 2], end: [f64; 2]) -> Option<[u32; 4]> {
    let [width, height] = dimensions;
    if width == 0 || height == 0 || !start.iter().chain(end.iter()).all(|v| v.is_finite()) {
        return None;
    }
    let bounds = |a: f64, b: f64, limit: u32| {
        let low = a.min(b).floor().clamp(0.0, f64::from(limit - 1)) as u32;
        let high = a.max(b).ceil().clamp(f64::from(low + 1), f64::from(limit)) as u32;
        (low, high - low)
    };
    let (x, w) = bounds(start[0], end[0], width);
    let (y, h) = bounds(start[1], end[1], height);
    Some([x, y, w, h])
}

pub fn screen_rect(image_rect: Rect, dimensions: [u32; 2], crop: [u32; 4]) -> Option<Rect> {
    let [width, height] = dimensions;
    let [x, y, w, h] = crop;
    let right = x.checked_add(w)?;
    let bottom = y.checked_add(h)?;
    if width == 0
        || height == 0
        || w == 0
        || h == 0
        || right > width
        || bottom > height
        || !image_rect.is_positive()
        || !image_rect.min.x.is_finite()
        || !image_rect.min.y.is_finite()
        || !image_rect.width().is_finite()
        || !image_rect.height().is_finite()
    {
        return None;
    }
    let scale = image_rect.size() / Vec2::new(width as f32, height as f32);
    Some(Rect::from_min_max(
        image_rect.min + Vec2::new(x as f32, y as f32) * scale,
        image_rect.min + Vec2::new(right as f32, bottom as f32) * scale,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reversed_drag_and_outside_end_are_clamped_to_current_pixels() {
        assert_eq!(
            from_points([100, 80], [70.25, 60.5], [10.75, 20.25]),
            Some([10, 20, 61, 41])
        );
        assert_eq!(
            from_points([100, 80], [90.0, 70.0], [1000.0, 1000.0]),
            Some([90, 70, 10, 10])
        );
        assert_eq!(
            from_points([100, 80], [0.0, 0.0], [-100.0, -100.0]),
            Some([0, 0, 1, 1])
        );
        assert_eq!(
            from_points([100, 80], [100.0, 80.0], [100.0, 80.0]),
            Some([99, 79, 1, 1])
        );
    }
    #[test]
    fn zoom_pan_and_dpi_geometry_produce_the_same_pixel_selection() {
        for rect in [
            Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(600.0, 450.0)),
            Rect::from_min_size(Pos2::new(-900.0, -100.0), Vec2::new(1800.0, 1350.0)),
        ] {
            let start = rect.min + rect.size() * Vec2::splat(0.25);
            let end = rect.min + rect.size() * Vec2::splat(0.75);
            let crop = from_points(
                [1200, 900],
                image_point(rect, [1200, 900], start).unwrap(),
                image_point(rect, [1200, 900], end).unwrap(),
            )
            .unwrap();
            assert_eq!(crop, [300, 225, 600, 450]);
            assert_eq!(
                screen_rect(rect, [1200, 900], crop),
                Some(Rect::from_min_max(start, end))
            );
        }
    }
    #[test]
    fn invalid_and_overflowing_coordinates_never_form_a_selection() {
        assert!(from_points([0, 10], [0.0, 0.0], [1.0, 1.0]).is_none());
        assert!(from_points([10, 10], [f64::NAN, 0.0], [1.0, 1.0]).is_none());
        assert!(screen_rect(Rect::EVERYTHING, [10, 10], [u32::MAX, 0, 5, 5]).is_none());
        assert!(screen_rect(Rect::EVERYTHING, [10, 10], [0, 0, 0, 5]).is_none());
        assert!(screen_rect(Rect::EVERYTHING, [10, 10], [0, 0, 5, 5]).is_none());
        assert!(image_point(Rect::NOTHING, [10, 10], Pos2::ZERO).is_none());
    }
}
