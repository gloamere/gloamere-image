//! Native clipping for a fixed transparent sticker viewport, without a custom WndProc.
use eframe::egui::Rect;
use light_image_tool::{Result, ToolError};

#[derive(Default)]
pub(super) struct Region {
    #[cfg(windows)]
    native: native::NativeRegion,
}

impl Region {
    pub(super) fn is_ready(&self) -> bool {
        #[cfg(windows)]
        {
            self.native.is_ready()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Number of successful native region publications during this state's lifetime.
    pub(super) fn update_count(&self) -> u64 {
        #[cfg(windows)]
        {
            self.native.update_count()
        }
        #[cfg(not(windows))]
        {
            0
        }
    }

    /// Coordinates are local to the client area in logical egui points.
    /// A not-yet-created native window returns false and is looked up again next frame.
    pub(super) fn update(
        &mut self,
        title: &str,
        image_local: Rect,
        menu_local: Option<Rect>,
        ppp: f32,
        frame: u64,
    ) -> Result<bool> {
        #[cfg(windows)]
        {
            let shape = PixelRegion::from_logical(image_local, menu_local, ppp)?;
            self.native.update(title, shape, frame, ppp.to_bits())
        }
        #[cfg(not(windows))]
        {
            let _ = (title, image_local, menu_local, ppp, frame);
            Err(ToolError::Invalid(
                "当前平台暂不支持贴图原生窗口裁剪区域".into(),
            ))
        }
    }

    pub(super) fn needs_repaint(&self) -> bool {
        #[cfg(windows)]
        {
            self.native.needs_repaint
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    pub(super) fn presentation_waits(&self) -> [u64; 3] {
        #[cfg(windows)]
        {
            self.native.presentation_waits
        }
        #[cfg(not(windows))]
        {
            [0; 3]
        }
    }
}

// GDI region coordinates have a signed 27-bit range.
#[cfg(any(windows, test))]
const MAX_COORDINATE: i32 = (1 << 26) - 1;

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PixelRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(any(windows, test))]
impl PixelRect {
    const EMPTY: Self = Self {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    fn from_logical(rect: Rect, ppp: f32) -> Result<Option<Self>> {
        Self::quantized(rect, ppp, true)
    }

    fn image(rect: Rect, ppp: f32) -> Result<Option<Self>> {
        Self::quantized(rect, ppp, false)
    }

    fn quantized(rect: Rect, ppp: f32, outward: bool) -> Result<Option<Self>> {
        if !rect.is_finite()
            || rect.width() < 0.0
            || rect.height() < 0.0
            || !ppp.is_finite()
            || ppp <= 0.0
        {
            return Err(ToolError::Invalid("贴图裁剪区域或显示比例无效".into()));
        }
        if rect.width() == 0.0 || rect.height() == 0.0 {
            return Ok(None);
        }
        let edge = |value: f32, upper: bool| -> Result<i32> {
            // Match egui-wgpu's f32 scissor calculation for image edges. A
            // floor/ceil envelope exposes native pixels the GPU never paints.
            let physical = f64::from(value * ppp);
            let rounded = if !outward {
                physical.round()
            } else if upper {
                physical.ceil()
            } else {
                physical.floor()
            };
            if rounded < -f64::from(MAX_COORDINATE) || rounded > f64::from(MAX_COORDINATE) {
                return Err(ToolError::Invalid("贴图裁剪区域超出原生坐标范围".into()));
            }
            Ok(rounded as i32)
        };
        Ok(Some(Self {
            left: edge(rect.left(), false)?,
            top: edge(rect.top(), false)?,
            right: edge(rect.right(), true)?,
            bottom: edge(rect.bottom(), true)?,
        }))
    }

    fn contains(self, other: Self) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && self.right >= other.right
            && self.bottom >= other.bottom
    }

    fn merged(self, other: Self) -> Option<Self> {
        if self.contains(other) {
            return Some(self);
        }
        if other.contains(self) {
            return Some(other);
        }
        let horizontal = self.top == other.top
            && self.bottom == other.bottom
            && self.left <= other.right
            && other.left <= self.right;
        let vertical = self.left == other.left
            && self.right == other.right
            && self.top <= other.bottom
            && other.top <= self.bottom;
        (horizontal || vertical).then_some(Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        })
    }

    #[cfg(windows)]
    fn translated(self, offset: [i32; 2]) -> Result<Self> {
        let translate = |value: i32, amount: i32| {
            value
                .checked_add(amount)
                .filter(|value| (-MAX_COORDINATE..=MAX_COORDINATE).contains(value))
                .ok_or_else(|| ToolError::Invalid("贴图裁剪区域超出原生坐标范围".into()))
        };
        Ok(Self {
            left: translate(self.left, offset[0])?,
            top: translate(self.top, offset[1])?,
            right: translate(self.right, offset[0])?,
            bottom: translate(self.bottom, offset[1])?,
        })
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PixelRegion {
    first: Option<PixelRect>,
    second: Option<PixelRect>,
}

#[cfg(any(windows, test))]
impl PixelRegion {
    fn new(first: Option<PixelRect>, second: Option<PixelRect>) -> Self {
        match (first, second) {
            (None, None) => Self::default(),
            (Some(rect), None) | (None, Some(rect)) => Self {
                first: Some(rect),
                second: None,
            },
            (Some(a), Some(b)) => {
                if let Some(rect) = a.merged(b) {
                    Self {
                        first: Some(rect),
                        second: None,
                    }
                } else {
                    Self {
                        first: Some(a.min(b)),
                        second: Some(a.max(b)),
                    }
                }
            }
        }
    }

    fn from_logical(image: Rect, menu: Option<Rect>, ppp: f32) -> Result<Self> {
        let image = PixelRect::image(image, ppp)?;
        let menu = menu
            .map(|rect| PixelRect::from_logical(rect, ppp))
            .transpose()?
            .flatten();
        Ok(Self::new(image, menu))
    }

    fn rectangles(self) -> impl Iterator<Item = PixelRect> {
        [self.first, self.second].into_iter().flatten()
    }

    fn covered_by(self, other: Self) -> bool {
        self.rectangles()
            .all(|rect| other.rectangles().any(|cover| cover.contains(rect)))
    }

    #[cfg(windows)]
    fn translated(self, offset: [i32; 2]) -> Result<Self> {
        Ok(Self::new(
            self.first.map(|rect| rect.translated(offset)).transpose()?,
            self.second
                .map(|rect| rect.translated(offset))
                .transpose()?,
        ))
    }
}

#[cfg(any(windows, test))]
#[derive(Default)]
struct AppliedRegion {
    last: Option<(usize, PixelRegion)>,
}

#[cfg(any(windows, test))]
#[derive(Default)]
struct PresentedFrames {
    frame: Option<u64>,
    pending: PixelRegion,
    previous: PixelRegion,
    confirmed: bool,
}

#[cfg(any(windows, test))]
impl PresentedFrames {
    fn observe(&mut self, frame: u64, shape: PixelRegion) {
        if self.frame != Some(frame) {
            self.previous = self.pending;
            self.frame = Some(frame);
            self.confirmed = false;
        }
        self.pending = shape;
    }
}

#[cfg(any(windows, test))]
impl AppliedRegion {
    fn needs_update(&self, window: usize, shape: PixelRegion) -> bool {
        self.last != Some((window, shape))
    }

    fn commit(&mut self, window: usize, shape: PixelRegion) {
        self.last = Some((window, shape));
    }

    fn invalidate(&mut self) {
        self.last = None;
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, POINT, RECT},
        System::Threading::GetCurrentProcessId,
        UI::WindowsAndMessaging::{
            EnumWindows, GetClientRect, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
            GetWindowThreadProcessId, IsWindow,
        },
    };

    type RegionHandle = *mut std::ffi::c_void;
    const RGN_OR: i32 = 2;
    const RGN_AND: i32 = 1;
    // Keep this small ABI boundary local: enabling the entire generated GDI
    // module is unnecessary for these five stable functions.
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateRectRgn(left: i32, top: i32, right: i32, bottom: i32) -> RegionHandle;
        fn CombineRgn(
            destination: RegionHandle,
            first: RegionHandle,
            second: RegionHandle,
            mode: i32,
        ) -> i32;
        fn DeleteObject(object: *mut std::ffi::c_void) -> i32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn ClientToScreen(window: HWND, point: *mut POINT) -> i32;
        fn SetWindowRgn(window: HWND, region: RegionHandle, redraw: i32) -> i32;
    }
    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmFlush() -> i32;
    }
    #[derive(Default)]
    pub(super) struct NativeRegion {
        // Numeric handles keep Region usable inside Send/Sync deferred viewport state.
        window: Option<usize>,
        title: Vec<u16>,
        title_buffer: Vec<u16>,
        applied: AppliedRegion,
        applied_clip: Option<PixelRegion>,
        frames: PresentedFrames,
        pub(super) needs_repaint: bool,
        pub(super) presentation_waits: [u64; 3],
        surface: Option<(u32, i32, i32)>,
        updates: u64,
    }

    impl NativeRegion {
        pub(super) fn is_ready(&self) -> bool {
            self.window.is_some_and(|window| {
                self.applied
                    .last
                    .as_ref()
                    .is_some_and(|(applied, _)| *applied == window)
            })
        }

        pub(super) fn update_count(&self) -> u64 {
            self.updates
        }

        pub(super) fn update(
            &mut self,
            title: &str,
            shape: PixelRegion,
            frame: u64,
            dpi: u32,
        ) -> Result<bool> {
            let expected: Vec<u16> = title.encode_utf16().collect();
            if expected.is_empty() || expected.len() > 32767 || expected.contains(&0) {
                return Err(ToolError::Invalid("贴图原生窗口标题无效".into()));
            }
            if self.title != expected {
                self.title = expected;
                // One extra character prevents a concurrently lengthened title
                // from matching merely because GetWindowTextW truncated its suffix.
                self.title_buffer.resize(self.title.len() + 2, 0);
                self.window = None;
                self.applied.invalidate();
                self.frames = PresentedFrames::default();
                self.applied_clip = None;
            }
            let process = unsafe { GetCurrentProcessId() };
            if self.window.is_some_and(|window| {
                !matches_window(window as HWND, process, &self.title, &mut self.title_buffer)
            }) {
                self.window = None;
                self.applied.invalidate();
                self.frames = PresentedFrames::default();
                self.applied_clip = None;
            }
            if self.window.is_none()
                && let Some(window) = find_window(process, &self.title, &mut self.title_buffer)?
            {
                let hwnd = window as HWND;
                if !matches_window(hwnd, process, &self.title, &mut self.title_buffer) {
                    return Ok(false);
                }
                self.window = Some(window);
            }
            let Some(window) = self.window else {
                return Ok(false);
            };
            let hwnd = window as HWND;
            let mut client = RECT::default();
            if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
                return Err(native_error("无法读取贴图画布大小"));
            }
            let surface = (dpi, client.right - client.left, client.bottom - client.top);
            if self.surface != Some(surface) {
                self.frames = PresentedFrames::default();
                self.applied.invalidate();
                self.applied_clip = None;
                self.surface = Some(surface);
            }
            let shape = shape.translated(client_offset(hwnd)?)?;
            self.frames.observe(frame, shape);
            let already_covered = self.applied.last.is_some_and(|(applied_window, applied)| {
                applied_window == window
                    && shape.covered_by(applied)
                    && self.applied_clip.is_some_and(|clip| shape.covered_by(clip))
            });
            let mut clip = if already_covered {
                shape
            } else {
                self.frames.previous
            };
            if shape.covered_by(clip) {
                clip = shape;
            }
            self.needs_repaint = !shape.covered_by(clip);
            if !self.applied.needs_update(window, shape) && self.applied_clip == Some(clip) {
                return Ok(false);
            }
            if !already_covered && !self.frames.confirmed && clip.first.is_some() {
                // This runs in the next complete egui frame, after eframe has
                // presented `previous`. Confirm that presentation before exposing
                // any new native pixels. Contractions synchronize their mask below.
                self.flush_composition()?;
                self.frames.confirmed = true;
            }
            // Recheck immediately before publishing the region to a native window.
            if !matches_window(hwnd, process, &self.title, &mut self.title_buffer) {
                self.window = None;
                self.applied.invalidate();
                self.frames = PresentedFrames::default();
                self.applied_clip = None;
                return Ok(false);
            }
            let only_expands = self.applied.last.is_some_and(|(applied_window, applied)| {
                applied_window == window
                    && applied.covered_by(shape)
                    && self
                        .applied_clip
                        .is_some_and(|old_clip| old_clip.covered_by(clip))
            });
            set_region(hwnd, shape, clip)?;
            if !only_expands {
                // The compositor also applies a shrinking HWND mask asynchronously.
                // Make it effective before eframe presents a smaller image.
                self.flush_composition()?;
                self.frames.confirmed = true;
            }
            self.applied.commit(window, shape);
            self.applied_clip = Some(clip);
            self.updates = self.updates.saturating_add(1);
            Ok(true)
        }

        fn flush_composition(&mut self) -> Result<()> {
            let started = std::time::Instant::now();
            let status = unsafe { DwmFlush() };
            let micros = started.elapsed().as_micros().min(u64::MAX as u128) as u64;
            self.presentation_waits[0] = self.presentation_waits[0].saturating_add(1);
            self.presentation_waits[1] = self.presentation_waits[1].saturating_add(micros);
            self.presentation_waits[2] = self.presentation_waits[2].max(micros);
            if status < 0 {
                Err(ToolError::Invalid(format!("无法同步贴图呈现：{status:#x}")))
            } else {
                Ok(())
            }
        }
    }

    fn matches_window(hwnd: HWND, process: u32, title: &[u16], buffer: &mut [u16]) -> bool {
        unsafe {
            if IsWindow(hwnd) == 0 {
                return false;
            }
            let mut owner = 0;
            if GetWindowThreadProcessId(hwnd, &mut owner) == 0 || owner != process {
                return false;
            }
            if GetWindowTextLengthW(hwnd) != title.len() as i32 {
                return false;
            }
            let copied = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
            copied == title.len() as i32 && buffer[..title.len()] == *title
        }
    }

    struct Search<'a> {
        process: u32,
        title: &'a [u16],
        buffer: &'a mut [u16],
        window: Option<usize>,
        matches: usize,
    }

    unsafe extern "system" fn enum_window(hwnd: HWND, parameter: LPARAM) -> i32 {
        // EnumWindows invokes this synchronously while Search is alive on the caller's stack.
        let search = unsafe { &mut *(parameter as *mut Search<'_>) };
        if matches_window(hwnd, search.process, search.title, search.buffer) {
            search.matches = search.matches.saturating_add(1);
            search.window = Some(hwnd as usize);
        }
        1
    }

    fn find_window(process: u32, title: &[u16], buffer: &mut [u16]) -> Result<Option<usize>> {
        let mut search = Search {
            process,
            title,
            buffer,
            window: None,
            matches: 0,
        };
        // Include hidden windows so the region can be installed during native creation.
        if unsafe {
            EnumWindows(
                Some(enum_window),
                (&mut search as *mut Search<'_>) as LPARAM,
            )
        } == 0
        {
            return Err(native_error("无法查找贴图原生窗口"));
        }
        if search.matches > 1 {
            return Err(ToolError::Invalid(
                "贴图原生窗口标题不唯一，未修改任何窗口区域".into(),
            ));
        }
        Ok(search.window)
    }

    fn client_offset(hwnd: HWND) -> Result<[i32; 2]> {
        let mut client = POINT { x: 0, y: 0 };
        let mut window = RECT::default();
        if unsafe { ClientToScreen(hwnd, &mut client) } == 0
            || unsafe { GetWindowRect(hwnd, &mut window) } == 0
        {
            return Err(native_error("无法读取贴图窗口坐标"));
        }
        let x = client.x.checked_sub(window.left);
        let y = client.y.checked_sub(window.top);
        match (x, y) {
            (Some(x), Some(y)) => Ok([x, y]),
            _ => Err(ToolError::Invalid("贴图窗口坐标超出原生范围".into())),
        }
    }

    struct OwnedRegion(RegionHandle);

    impl OwnedRegion {
        fn rectangle(rect: PixelRect) -> Result<Self> {
            let region = unsafe { CreateRectRgn(rect.left, rect.top, rect.right, rect.bottom) };
            if region.is_null() {
                Err(native_error("无法创建贴图裁剪区域"))
            } else {
                Ok(Self(region))
            }
        }
    }

    impl Drop for OwnedRegion {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { DeleteObject(self.0) };
            }
        }
    }

    fn owned_shape(shape: PixelRegion) -> Result<OwnedRegion> {
        let mut rectangles = shape.rectangles();
        let combined = OwnedRegion::rectangle(rectangles.next().unwrap_or(PixelRect::EMPTY))?;
        if let Some(rect) = rectangles.next() {
            let menu = OwnedRegion::rectangle(rect)?;
            if unsafe { CombineRgn(combined.0, combined.0, menu.0, RGN_OR) } == 0 {
                return Err(native_error("无法合并贴图与菜单裁剪区域"));
            }
        }
        Ok(combined)
    }

    fn set_region(hwnd: HWND, shape: PixelRegion, clip: PixelRegion) -> Result<()> {
        let mut combined = owned_shape(shape)?;
        if !shape.covered_by(clip) {
            let presented = owned_shape(clip)?;
            if unsafe { CombineRgn(combined.0, combined.0, presented.0, RGN_AND) } == 0 {
                return Err(native_error("无法同步贴图可见区域"));
            }
        }
        // The caller is already painting this pose. Avoid an extra native paint
        // cycle for every animation sample.
        if unsafe { SetWindowRgn(hwnd, combined.0, 0) } == 0 {
            return Err(native_error("无法设置贴图原生裁剪区域"));
        }
        // On success Windows owns RegionHandle; no further GDI call may use or delete it.
        combined.0 = std::ptr::null_mut();
        Ok(())
    }

    fn native_error(action: &str) -> ToolError {
        ToolError::Invalid(format!("{action}：{}", std::io::Error::last_os_error()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Pos2;

    fn rect(left: f32, top: f32, right: f32, bottom: f32) -> Rect {
        Rect::from_min_max(Pos2::new(left, top), Pos2::new(right, bottom))
    }

    #[test]
    fn region_quantization_rounds_outward_at_fractional_dpi() {
        assert_eq!(
            PixelRect::from_logical(rect(1.2, -2.1, 20.1, 30.2), 1.25).unwrap(),
            Some(PixelRect {
                left: 1,
                top: -3,
                right: 26,
                bottom: 38
            })
        );
        assert_eq!(
            PixelRect::from_logical(rect(1.2, 2.3, 1.2, 9.1), 1.5).unwrap(),
            None
        );
        assert!(PixelRect::from_logical(rect(0.0, 0.0, 10.0, 10.0), 0.0).is_err());
        assert!(PixelRect::from_logical(rect(0.0, 0.0, f32::INFINITY, 10.0), 1.0).is_err());
        assert!(PixelRect::from_logical(rect(0.0, 0.0, 10.0, 10.0), f32::NAN).is_err());
    }

    #[test]
    fn image_region_matches_gpu_scissor_without_exposing_fractional_edge_pixels() {
        let image = rect(447.3334, 327.33334, 1087.3334, 807.3334);
        for ppp in [1.0, 1.25, 1.5, 1.75, 2.0] {
            let shape = PixelRegion::from_logical(image, None, ppp).unwrap();
            assert_eq!(
                shape.first,
                Some(PixelRect {
                    left: (image.left() * ppp).round() as i32,
                    top: (image.top() * ppp).round() as i32,
                    right: (image.right() * ppp).round() as i32,
                    bottom: (image.bottom() * ppp).round() as i32,
                })
            );
        }
        assert_eq!(PixelRect::image(image, 1.5).unwrap().unwrap().right, 1631);
        assert_eq!(PixelRect::image(image, 1.5).unwrap().unwrap().bottom, 1211);
    }

    #[test]
    fn image_menu_union_preserves_the_gap_instead_of_a_bounding_box() {
        let shape = PixelRegion::from_logical(
            rect(10.0, 10.0, 30.0, 30.0),
            Some(rect(50.0, 15.0, 70.0, 40.0)),
            1.0,
        )
        .unwrap();
        let contains = |x, y| {
            shape
                .rectangles()
                .any(|r| r.left <= x && x < r.right && r.top <= y && y < r.bottom)
        };
        assert!(contains(20, 20));
        assert!(contains(60, 20));
        assert!(!contains(40, 20));
        assert_eq!(shape.rectangles().count(), 2);
    }

    #[test]
    fn redundant_or_adjacent_rectangles_do_not_change_the_union() {
        let image = rect(0.0, 0.0, 40.0, 40.0);
        let without_menu = PixelRegion::from_logical(image, None, 1.0).unwrap();
        assert_eq!(
            without_menu,
            PixelRegion::from_logical(image, Some(rect(5.0, 5.0, 10.0, 10.0)), 1.0).unwrap()
        );
        let a = PixelRect::from_logical(rect(0.0, 0.0, 10.0, 10.0), 1.0).unwrap();
        let b = PixelRect::from_logical(rect(10.0, 0.0, 20.0, 10.0), 1.0).unwrap();
        let joined = PixelRegion::new(a, b);
        assert_eq!(joined, PixelRegion::new(b, a));
        assert_eq!(
            joined,
            PixelRegion::from_logical(rect(0.0, 0.0, 20.0, 10.0), None, 1.0).unwrap()
        );
    }

    #[test]
    fn region_updates_deduplicate_pixels_and_reapply_for_menu_or_window_changes() {
        let mut applied = AppliedRegion::default();
        let image = rect(10.0, 10.0, 30.0, 30.0);
        let first = PixelRegion::from_logical(image, None, 1.0).unwrap();
        assert!(applied.needs_update(1, first));
        applied.commit(1, first);
        assert!(!applied.needs_update(1, first));
        let subpixel = PixelRegion::from_logical(rect(10.1, 10.1, 29.9, 29.9), None, 1.0).unwrap();
        assert!(!applied.needs_update(1, subpixel));
        let menu =
            PixelRegion::from_logical(image, Some(rect(50.0, 15.0, 70.0, 40.0)), 1.0).unwrap();
        assert!(applied.needs_update(1, menu));
        applied.commit(1, menu);
        assert!(!applied.needs_update(1, menu));
        assert!(applied.needs_update(1, first));
        assert!(applied.needs_update(2, menu));
        applied.invalidate();
        assert!(applied.needs_update(1, menu));
    }

    #[test]
    fn region_state_is_send_and_sync_for_deferred_viewports() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Region>();
    }

    #[test]
    fn multiple_passes_only_publish_the_last_pose_of_a_completed_frame() {
        let initial = PixelRegion::from_logical(rect(10.0, 10.0, 30.0, 30.0), None, 1.0).unwrap();
        let expanded = PixelRegion::from_logical(rect(5.0, 5.0, 35.0, 35.0), None, 1.0).unwrap();
        let mut frames = PresentedFrames::default();
        frames.observe(1, initial);
        assert_eq!(frames.previous, PixelRegion::default());
        frames.observe(1, expanded);
        assert_eq!(frames.previous, PixelRegion::default());
        frames.observe(2, initial);
        assert_eq!(frames.previous, expanded);
        frames.confirmed = true;
        frames.observe(2, expanded);
        assert!(frames.confirmed);
        assert_eq!(frames.previous, expanded);
        frames.observe(3, expanded);
        assert!(!frames.confirmed);
    }

    #[test]
    fn contraction_is_safe_only_when_both_published_bounds_cover_it() {
        let old = PixelRegion::from_logical(rect(10.0, 10.0, 30.0, 30.0), None, 1.0).unwrap();
        let current = PixelRegion::from_logical(rect(8.0, 8.0, 32.0, 32.0), None, 1.0).unwrap();
        let inside_both =
            PixelRegion::from_logical(rect(11.0, 11.0, 29.0, 29.0), None, 1.0).unwrap();
        assert!(inside_both.covered_by(old) && inside_both.covered_by(current));
        let between = PixelRegion::from_logical(rect(9.0, 9.0, 31.0, 31.0), None, 1.0).unwrap();
        assert!(between.covered_by(current));
        assert!(!between.covered_by(old));
        assert!(PixelRegion::default().covered_by(old));
        assert!(!old.covered_by(PixelRegion::default()));
    }
}
