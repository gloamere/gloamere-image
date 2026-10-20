//! Pure CPU image motion inside a fixed native canvas. This module never plans a
//! native resize: callers draw image_world relative to the actual canvas origin.
use eframe::egui::{Pos2, Rect, Vec2};
use std::time::Duration;

const WHEEL_SENSITIVITY: f64 = 0.0006;
const TIME_CONSTANT: f64 = 0.05;
const EPSILON: f32 = 0.05;

#[derive(Clone, Debug)]
pub(super) struct FramePlan {
    pub current: f64,
    pub target: f64,
    pub image_world: Rect,
    pub target_world: Rect,
    pub canvas_world: Rect,
    pub active: bool,
    pub closed: bool,
    pub repaint: bool,
    pub repaint_after: Option<Duration>,
    pub animation_frames: u64,
}

#[derive(Clone, Debug)]
pub(super) struct Zoom {
    /// Multipliers relative to the initial displayed size, not source pixels.
    pub current: f64,
    pub target: f64,
    base_size: Vec2,
    image: Rect,
    target_origin: Pos2,
    canvas: Rect,
    anchor: Pos2,
    anchor_uv: Vec2,
    last_event: f64,
    last_tick: f64,
    active: bool,
    closed: bool,
    animation_frames: u64,
    wheel_direction: f64,
}

impl Zoom {
    pub(super) fn new(initial_image_world: Rect, canvas_world: Rect) -> Option<Self> {
        if !valid_rect(initial_image_world) || !valid_rect(canvas_world) {
            return None;
        }
        let base_size = initial_image_world.size();
        let scale = (canvas_world.width() / base_size.x)
            .min(canvas_world.height() / base_size.y)
            .min(1.0);
        let size = base_size * scale;
        let origin = bounded_origin(initial_image_world.min, size, canvas_world);
        Some(Self {
            current: f64::from(scale),
            target: f64::from(scale),
            base_size,
            image: Rect::from_min_size(origin, size),
            target_origin: origin,
            canvas: canvas_world,
            anchor: origin + size / 2.0,
            anchor_uv: Vec2::splat(0.5),
            last_event: f64::NEG_INFINITY,
            last_tick: 0.0,
            active: false,
            closed: false,
            animation_frames: 0,
            wheel_direction: 0.0,
        })
    }

    pub(super) fn image_world(&self) -> Rect {
        self.image
    }

    pub(super) fn target_world(&self) -> Rect {
        Rect::from_min_size(self.target_origin, self.scaled(self.target))
    }

    #[cfg(test)]
    pub(super) fn is_active(&self) -> bool {
        self.active
    }

    #[cfg(test)]
    pub(super) fn is_closed(&self) -> bool {
        self.closed
    }

    /// Accept raw wheel points even when a native hover window has not acquired
    /// keyboard focus. The caller decides when an explicit loss cancels motion.
    pub(super) fn wheel(&mut self, points: f64, cursor_world: Pos2, time: f64) -> bool {
        if self.closed
            || !time.is_finite()
            || time < self.last_event
            || !points.is_finite()
            || points == 0.0
            || !cursor_world.is_finite()
            || !inside(self.image, cursor_world)
        {
            return false;
        }
        self.anchor = cursor_world;
        self.anchor_uv = (cursor_world - self.image.min) / self.image.size();
        let direction = points.signum();
        let baseline =
            if self.active && self.wheel_direction != 0.0 && direction != self.wheel_direction {
                self.current
            } else {
                self.target
            };
        self.wheel_direction = direction;
        let factor = (points * WHEEL_SENSITIVITY).clamp(-20.0, 20.0).exp();
        self.retarget(baseline * factor, time);
        self.last_event = time;
        true
    }

    /// Return to the initial displayed size around the current image center.
    pub(super) fn reset(&mut self, time: f64) -> bool {
        if self.closed || !time.is_finite() || time < self.last_event {
            return false;
        }
        self.anchor = self.image.center();
        self.anchor_uv = Vec2::splat(0.5);
        self.wheel_direction = 0.0;
        self.retarget(1.0, time);
        self.last_event = time;
        true
    }

    pub(super) fn tick(&mut self, time: f64) -> FramePlan {
        if self.active && !self.closed && time.is_finite() && time > self.last_tick {
            let dt = (time - self.last_tick).min(0.05);
            self.last_tick = time;
            let alpha = 1.0 - (-dt / TIME_CONSTANT).exp();
            self.current += (self.target - self.current) * alpha;
            let origin = self.image.min + (self.target_origin - self.image.min) * alpha as f32;
            self.image = Rect::from_min_size(origin, self.scaled(self.current));
            self.animation_frames += 1;
            if rect_near(self.image, self.target_world(), EPSILON) {
                self.current = self.target;
                self.image = self.target_world();
                self.active = false;
            }
        }
        FramePlan {
            current: self.current,
            target: self.target,
            image_world: self.image,
            target_world: self.target_world(),
            canvas_world: self.canvas,
            active: self.active,
            closed: self.closed,
            repaint: self.active && !self.closed,
            repaint_after: (self.active && !self.closed).then_some(Duration::from_millis(8)),
            animation_frames: self.animation_frames,
        }
    }

    /// Stop exactly the last displayed pose. Do not advance to an unseen pose at
    /// the loss timestamp, and do not discard subpixel accumulated scale.
    pub(super) fn lose_focus(&mut self, time: f64) {
        self.active = false;
        self.wheel_direction = 0.0;
        self.target = self.current;
        self.target_origin = self.image.min;
        if time.is_finite() {
            self.last_tick = self.last_tick.max(time);
        }
    }

    pub(super) fn close(&mut self) {
        self.closed = true;
        self.lose_focus(self.last_tick);
    }

    pub(super) fn rebase(&mut self, delta: Vec2) -> bool {
        if self.closed || !delta.is_finite() {
            return false;
        }
        self.canvas = self.canvas.translate(delta);
        self.image = self.image.translate(delta);
        self.target_origin += delta;
        self.anchor += delta;
        true
    }

    /// Position-only native moves preserve the entire gesture by a translation.
    /// A DPI/size change uniformly fits current pixels when necessary, then
    /// updates target limits; it never seeds scale from the native canvas size.
    pub(super) fn set_canvas(&mut self, canvas_world: Rect) -> bool {
        if self.closed || !valid_rect(canvas_world) || self.canvas == canvas_world {
            return false;
        }
        let delta = canvas_world.min - self.canvas.min;
        let size_changed = self.canvas.size() != canvas_world.size();
        self.rebase(delta);
        self.canvas = canvas_world;
        if size_changed {
            let fit = (self.canvas.width() / self.image.width())
                .min(self.canvas.height() / self.image.height())
                .min(1.0);
            self.current *= f64::from(fit);
            let size = self.scaled(self.current);
            self.image =
                Rect::from_min_size(bounded_origin(self.image.min, size, self.canvas), size);
            let limits = self.limits();
            self.target = self.target.clamp(limits[0], limits[1]);
            self.update_target_origin();
            self.active = !rect_near(self.image, self.target_world(), f32::EPSILON);
        }
        true
    }

    fn retarget(&mut self, scale: f64, time: f64) {
        if !self.active {
            self.last_tick = self.last_tick.max(time);
        }
        let limits = self.limits();
        self.target = scale.clamp(limits[0], limits[1]);
        self.update_target_origin();
        // A subpixel input is still retained, even if its f32 rectangle rounds
        // to the same coordinates. Further events accumulate on this target.
        self.active = (self.current - self.target).abs() > f64::EPSILON
            || self.image.min.distance(self.target_origin) > f32::EPSILON;
    }

    fn scaled(&self, scale: f64) -> Vec2 {
        Vec2::new(
            (f64::from(self.base_size.x) * scale) as f32,
            (f64::from(self.base_size.y) * scale) as f32,
        )
    }

    fn limits(&self) -> [f64; 2] {
        let max = (f64::from(self.canvas.width() * 0.9) / f64::from(self.base_size.x))
            .min(f64::from(self.canvas.height() * 0.9) / f64::from(self.base_size.y));
        let min = (24.0 / f64::from(self.base_size.x.max(self.base_size.y))).min(max);
        [min, max]
    }

    fn update_target_origin(&mut self) {
        let size = self.scaled(self.target);
        self.target_origin = bounded_origin(self.anchor - size * self.anchor_uv, size, self.canvas);
    }
}

fn valid_rect(rect: Rect) -> bool {
    rect.is_finite() && rect.size().is_finite() && rect.is_positive()
}

fn inside(rect: Rect, point: Pos2) -> bool {
    point.x >= rect.left()
        && point.x < rect.right()
        && point.y >= rect.top()
        && point.y < rect.bottom()
}

fn bounded_origin(origin: Pos2, size: Vec2, canvas: Rect) -> Pos2 {
    Pos2::new(
        origin
            .x
            .clamp(canvas.left(), (canvas.right() - size.x).max(canvas.left())),
        origin
            .y
            .clamp(canvas.top(), (canvas.bottom() - size.y).max(canvas.top())),
    )
}

fn rect_near(a: Rect, b: Rect, tolerance: f32) -> bool {
    (a.min.x - b.min.x).abs() <= tolerance
        && (a.min.y - b.min.y).abs() <= tolerance
        && (a.max.x - b.max.x).abs() <= tolerance
        && (a.max.y - b.max.y).abs() <= tolerance
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h))
    }
    fn make_zoom() -> Zoom {
        Zoom::new(
            rect(300.0, 200.0, 640.0, 480.0),
            rect(0.0, 0.0, 1920.0, 1080.0),
        )
        .unwrap()
    }
    fn finish(zoom: &mut Zoom, start: f64) -> FramePlan {
        for frame in 1..100 {
            let plan = zoom.tick(start + f64::from(frame) * 0.008);
            if !plan.active {
                return plan;
            }
        }
        panic!("GPU motion did not converge");
    }

    #[test]
    fn cursor_anchor_and_ratio_remain_stable_across_gpu_frames() {
        let mut zoom = make_zoom();
        let anchor = Pos2::new(430.0, 350.0);
        let uv = (anchor - zoom.image.min) / zoom.image.size();
        assert!(zoom.wheel(120.0, anchor, 0.0));
        let canvas = zoom.canvas;
        let mut previous = zoom.image.width();
        for frame in 1..12 {
            let plan = zoom.tick(f64::from(frame) * 0.008);
            assert_eq!(plan.canvas_world, canvas);
            assert!(plan.image_world.width() > previous);
            previous = plan.image_world.width();
            assert!(
                (plan.image_world.width() / plan.image_world.height() - 4.0 / 3.0).abs() < 0.0001
            );
            assert!((plan.image_world.min + plan.image_world.size() * uv).distance(anchor) < 0.001);
        }
        let plan = finish(&mut zoom, 0.1);
        assert!(plan.animation_frames > 3);
        assert!(!plan.repaint);
        assert_eq!(plan.repaint_after, None);
    }

    #[test]
    fn reversing_input_updates_target_without_snapping_current_image() {
        let mut zoom = make_zoom();
        let anchor = zoom.image.center();
        zoom.wheel(40.0, anchor, 0.0);
        zoom.tick(0.025);
        let current = zoom.image;
        zoom.wheel(-80.0, anchor, 0.026);
        assert_eq!(zoom.image, current);
        assert!(zoom.target < zoom.current);
        assert!(zoom.tick(0.042).image_world.width() < current.width());
    }

    #[test]
    fn one_reverse_tick_drops_unseen_forward_motion_without_jumping() {
        let mut zoom = make_zoom();
        let anchor = zoom.image.center();
        zoom.wheel(400.0, anchor, 0.0);
        let visible = zoom.tick(0.016).image_world;
        let current = zoom.current;
        zoom.wheel(-40.0, anchor, 0.016);
        assert_eq!(zoom.image_world(), visible);
        assert!(zoom.target < current);
        assert!(zoom.tick(0.032).image_world.width() < visible.width());
    }

    #[test]
    fn high_precision_points_accumulate_across_completed_subpixel_steps() {
        let mut zoom = make_zoom();
        let anchor = zoom.image.center();
        for frame in 0..1000 {
            let time = f64::from(frame) * 0.01;
            zoom.wheel(0.001, anchor, time);
            zoom.tick(time + 0.008);
        }
        assert!((zoom.target - WHEEL_SENSITIVITY.exp()).abs() < 1e-10);
        assert!(zoom.image.width() > 640.0);
    }

    #[test]
    fn elapsed_time_has_same_result_at_different_frame_rates() {
        let mut fast = make_zoom();
        fast.wheel(120.0, fast.image.center(), 0.0);
        let mut slow = fast.clone();
        for frame in 1..=16 {
            fast.tick(f64::from(frame) * 0.008);
        }
        for frame in 1..=8 {
            slow.tick(f64::from(frame) * 0.016);
        }
        assert!((fast.current - slow.current).abs() < 1e-10);
        assert!(rect_near(fast.image, slow.image, 0.001));
        finish(&mut fast, 0.13);
        let count = fast.animation_frames;
        assert!(!fast.tick(100.0).active);
        assert_eq!(fast.animation_frames, count);
    }

    #[test]
    fn reset_returns_to_initial_size_around_current_center() {
        let mut zoom = make_zoom();
        zoom.wheel(500.0, zoom.image.center(), 0.0);
        finish(&mut zoom, 0.0);
        let center = zoom.image.center();
        assert!(zoom.reset(1.0));
        let plan = finish(&mut zoom, 1.0);
        assert!((plan.image_world.width() - 640.0).abs() < 0.001);
        assert!(plan.image_world.center().distance(center) < 0.001);
    }

    #[test]
    fn explicit_loss_freezes_current_and_close_rejects_future_input() {
        let mut zoom = make_zoom();
        zoom.wheel(120.0, zoom.image.center(), 0.0);
        zoom.tick(0.025);
        let shown = zoom.image;
        zoom.lose_focus(0.03);
        assert_eq!(zoom.tick(1.0).image_world, shown);
        assert!(!zoom.is_active());
        zoom.wheel(40.0, shown.center(), 1.1);
        assert!(zoom.is_active());
        zoom.close();
        assert!(zoom.is_closed());
        assert!(!zoom.tick(2.0).repaint);
        assert!(!zoom.wheel(40.0, shown.center(), 2.1));
        assert!(!zoom.reset(2.2));
    }

    #[test]
    fn pointer_events_outside_picture_or_backwards_in_time_are_ignored() {
        let mut zoom = make_zoom();
        for point in [Pos2::ZERO, zoom.image.max, Pos2::new(f32::NAN, 0.0)] {
            assert!(!zoom.wheel(40.0, point, 0.0));
        }
        assert!(!zoom.wheel(f64::NAN, zoom.image.center(), 0.0));
        assert!(!zoom.wheel(40.0, zoom.image.center(), f64::NAN));
        assert!(zoom.wheel(40.0, zoom.image.center(), 1.0));
        let target = zoom.target;
        assert!(!zoom.wheel(40.0, zoom.image.center(), 0.5));
        assert_eq!(zoom.target, target);
    }

    #[test]
    fn bounds_keep_negative_monitor_picture_whole_at_min_and_max_scale() {
        let canvas = rect(-1920.0, -200.0, 1920.0, 1080.0);
        let mut zoom = Zoom::new(rect(-700.0, 100.0, 640.0, 480.0), canvas).unwrap();
        zoom.wheel(1e6, zoom.image.min + Vec2::splat(1.0), 0.0);
        for frame in 1..80 {
            let plan = zoom.tick(f64::from(frame) * 0.008);
            assert!(canvas.expand(0.001).contains_rect(plan.image_world));
        }
        assert!(zoom.image.width() <= canvas.width() * 0.9001);
        assert!(zoom.image.height() <= canvas.height() * 0.9001);
        zoom.wheel(-1e6, zoom.image.center(), 1.0);
        let plan = finish(&mut zoom, 1.0);
        assert!((plan.image_world.width() - 24.0).abs() < 0.001);
        assert!((plan.image_world.width() / plan.image_world.height() - 4.0 / 3.0).abs() < 0.0001);
    }

    #[test]
    fn same_canvas_samples_never_reseed_the_scale_target() {
        let mut zoom = make_zoom();
        let canvas = zoom.canvas;
        zoom.wheel(120.0, zoom.image.center(), 0.0);
        let target = zoom.target;
        for frame in 1..30 {
            assert!(!zoom.set_canvas(canvas));
            zoom.tick(f64::from(frame) * 0.008);
            assert_eq!(zoom.target, target);
        }
    }

    #[test]
    fn rebase_moves_current_target_and_anchor_without_resetting_motion() {
        let mut zoom = make_zoom();
        zoom.wheel(120.0, zoom.image.center(), 0.0);
        zoom.tick(0.02);
        let image = zoom.image;
        let target = zoom.target_world();
        let scale = zoom.current;
        let delta = Vec2::new(-2000.0, 125.0);
        assert!(zoom.rebase(delta));
        assert_eq!(zoom.image, image.translate(delta));
        assert!(rect_near(
            zoom.target_world(),
            target.translate(delta),
            0.001
        ));
        assert_eq!(zoom.current, scale);
        assert!(zoom.is_active());
    }

    #[test]
    fn canvas_size_change_uniformly_fits_picture_and_updates_target_limits() {
        let mut zoom = make_zoom();
        zoom.wheel(120.0, zoom.image.center(), 0.0);
        zoom.tick(0.02);
        let canvas = rect(-1000.0, -100.0, 500.0, 400.0);
        assert!(zoom.set_canvas(canvas));
        assert!(canvas.expand(0.001).contains_rect(zoom.image));
        let plan = finish(&mut zoom, 0.025);
        assert!(canvas.expand(0.001).contains_rect(plan.image_world));
        assert!((plan.image_world.width() / plan.image_world.height() - 4.0 / 3.0).abs() < 0.0001);
        assert!(plan.image_world.width() <= 450.01);
        assert!(plan.image_world.height() <= 360.01);
    }

    #[test]
    fn invalid_geometry_and_nonfinite_ticks_do_not_poison_motion() {
        assert!(Zoom::new(Rect::NOTHING, rect(0.0, 0.0, 100.0, 100.0)).is_none());
        let mut zoom = make_zoom();
        zoom.wheel(40.0, zoom.image.center(), 0.0);
        let image = zoom.image;
        assert!(!zoom.set_canvas(Rect::NOTHING));
        assert!(!zoom.rebase(Vec2::new(f32::INFINITY, 0.0)));
        assert_eq!(zoom.tick(f64::NAN).image_world, image);
        assert!(zoom.tick(0.02).image_world.width() > image.width());
    }
}
