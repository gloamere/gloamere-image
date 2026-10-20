use crate::{Result, ToolError};
use std::{
    ops::Deref,
    sync::{
        Arc, LazyLock, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
#[derive(Debug)]
pub struct Budget {
    limit: u64,
    used: AtomicU64,
    peak: AtomicU64,
}
impl Budget {
    pub fn new(limit: u64) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        })
    }
    pub fn in_use(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }
    pub fn peak(&self) -> u64 {
        self.peak.load(Ordering::SeqCst)
    }
    pub fn reserve(self: &Arc<Self>, bytes: u64) -> Result<Reservation> {
        if bytes > self.limit {
            return Err(ToolError::Invalid("操作本身超过内存安全预算".into()));
        }
        let mut current = self.in_use();
        loop {
            let Some(total) = current
                .checked_add(bytes)
                .filter(|total| *total <= self.limit)
            else {
                return Err(ToolError::ResourceBusy(
                    "当前分配已占用预算，请等待正在运行的操作完成".into(),
                ));
            };
            match self
                .used
                .compare_exchange(current, total, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => {
                    self.peak.fetch_max(total, Ordering::SeqCst);
                    return Ok(Reservation {
                        budget: self.clone(),
                        bytes,
                    });
                }
                Err(updated) => current = updated,
            }
        }
    }
}
static GLOBAL: LazyLock<Arc<Budget>> = LazyLock::new(|| Budget::new(super::OPERATION_BUDGET));
#[derive(Debug)]
pub struct Reservation {
    budget: Arc<Budget>,
    bytes: u64,
}
impl Reservation {
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn resize(&mut self, bytes: u64) -> Result<()> {
        if bytes <= self.bytes {
            return self.shrink(bytes);
        }
        let mut added = self.budget.reserve(bytes - self.bytes)?;
        self.bytes += added.bytes;
        added.bytes = 0;
        Ok(())
    }
    pub fn split(&mut self, bytes: u64) -> Result<Self> {
        if bytes > self.bytes {
            return Err(ToolError::Invalid("预算分配错误：输出超过预留".into()));
        }
        self.bytes -= bytes;
        Ok(Self {
            budget: self.budget.clone(),
            bytes,
        })
    }
    pub fn shrink(&mut self, bytes: u64) -> Result<()> {
        if bytes > self.bytes {
            return Err(ToolError::Invalid("预算分配错误：实际容量超过预留".into()));
        }
        self.budget
            .used
            .fetch_sub(self.bytes - bytes, Ordering::SeqCst);
        self.bytes = bytes;
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}
pub fn reserve(bytes: u64) -> Result<Reservation> {
    GLOBAL.reserve(bytes)
}
pub fn in_use() -> u64 {
    GLOBAL.in_use()
}
pub fn peak() -> u64 {
    GLOBAL.peak()
}
#[derive(Debug)]
pub struct Snapshot {
    bytes: Vec<u8>,
    _reservation: Reservation,
}
impl Snapshot {
    pub fn from_reserved(bytes: Vec<u8>, mut reservation: Reservation) -> Result<Self> {
        reservation.shrink(bytes.capacity() as u64)?;
        Ok(Self {
            bytes,
            _reservation: reservation,
        })
    }
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        let reservation = reserve(bytes.capacity() as u64)?;
        Self::from_reserved(bytes, reservation)
    }
}
impl Deref for Snapshot {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}
impl AsRef<[u8]> for Snapshot {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Debug)]
pub struct Pixels {
    image: image::RgbaImage,
    _reservation: Reservation,
    display: OnceLock<Arc<PreparedDisplay>>,
}
pub const DISPLAY_TILE_EDGE: u32 = 2048;

#[derive(Debug)]
pub struct DisplayTile {
    pub x: u32,
    pub y: u32,
    pub image: Arc<eframe::egui::ColorImage>,
}

/// CPU display data prepared by decode/edit workers and shared with egui without copying.
#[derive(Debug)]
pub struct PreparedDisplay {
    pub tiles: Vec<DisplayTile>,
    pub has_alpha: bool,
    _reservation: Reservation,
}
impl Pixels {
    pub fn from_reserved(image: image::RgbaImage, reservation: &mut Reservation) -> Result<Self> {
        let owned = reservation.split(image.as_raw().capacity() as u64)?;
        Ok(Self {
            image,
            _reservation: owned,
            display: OnceLock::new(),
        })
    }
    pub fn from_image(image: image::RgbaImage) -> Result<Self> {
        let mut r = reserve(image.as_raw().capacity() as u64)?;
        Self::from_reserved(image, &mut r)
    }

    pub fn prepare_display(&self, cancel: &crate::Cancellation) -> Result<&Arc<PreparedDisplay>> {
        self.prepare_display_with_alpha(cancel, None)
    }

    pub fn prepare_display_with_alpha(
        &self,
        cancel: &crate::Cancellation,
        known_alpha: Option<bool>,
    ) -> Result<&Arc<PreparedDisplay>> {
        cancel.check()?;
        if let Some(display) = self.display.get() {
            return Ok(display);
        }
        let reservation = self
            ._reservation
            .budget
            .reserve(self.image.as_raw().len() as u64)?;
        let mut tiles = Vec::new();
        let mut has_alpha = known_alpha.unwrap_or(false);
        for y in (0..self.height()).step_by(DISPLAY_TILE_EDGE as usize) {
            for x in (0..self.width()).step_by(DISPLAY_TILE_EDGE as usize) {
                let w = DISPLAY_TILE_EDGE.min(self.width() - x);
                let h = DISPLAY_TILE_EDGE.min(self.height() - y);
                let mut colors = Vec::with_capacity((w * h) as usize);
                for row in y..y + h {
                    cancel.check()?;
                    let start =
                        ((u64::from(row) * u64::from(self.width()) + u64::from(x)) * 4) as usize;
                    for p in self.as_raw()[start..start + w as usize * 4]
                        .as_chunks::<4>()
                        .0
                    {
                        if known_alpha.is_none() {
                            has_alpha |= p[3] < 255;
                        }
                        colors.push(eframe::egui::Color32::from_rgba_unmultiplied(
                            p[0], p[1], p[2], p[3],
                        ));
                    }
                }
                tiles.push(DisplayTile {
                    x,
                    y,
                    image: Arc::new(eframe::egui::ColorImage::new(
                        [w as usize, h as usize],
                        colors,
                    )),
                });
            }
        }
        cancel.check()?;
        // Concurrent preparations may race; the losing value drops its own reservation.
        let _ = self.display.set(Arc::new(PreparedDisplay {
            tiles,
            has_alpha,
            _reservation: reservation,
        }));
        Ok(self.display.get().expect("prepared display published"))
    }
}
impl Deref for Pixels {
    type Target = image::RgbaImage;
    fn deref(&self) -> &image::RgbaImage {
        &self.image
    }
}
impl PartialEq for Pixels {
    fn eq(&self, other: &Self) -> bool {
        self.image == other.image
    }
}
impl PartialEq<image::RgbaImage> for Pixels {
    fn eq(&self, other: &image::RgbaImage) -> bool {
        self.image == *other
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_display_tiles_preserve_rgba_and_share_owned_color_buffers() {
        let image = image::RgbaImage::from_fn(2050, 2, |x, y| {
            image::Rgba([x as u8, y as u8, 80, if x % 3 == 0 { 127 } else { 255 }])
        });
        let budget = Budget::new(100_000);
        let mut reservation = budget.reserve(image.as_raw().len() as u64).unwrap();
        let pixels = Pixels::from_reserved(image, &mut reservation).unwrap();
        let cancel = crate::Cancellation::default();
        let display = pixels.prepare_display(&cancel).unwrap().clone();
        assert!(display.has_alpha);
        assert_eq!(display.tiles.len(), 2);
        assert_eq!(display.tiles[1].image.size, [2, 2]);
        for tile in &display.tiles {
            for y in 0..tile.image.size[1] {
                for x in 0..tile.image.size[0] {
                    let p = pixels.get_pixel(tile.x + x as u32, tile.y + y as u32);
                    assert_eq!(
                        tile.image[(x, y)],
                        eframe::egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])
                    );
                }
            }
        }
        assert!(Arc::ptr_eq(
            &display,
            pixels.prepare_display(&cancel).unwrap()
        ));
        drop(pixels);
        assert_eq!(budget.in_use(), 2050 * 2 * 4);
        drop(display);
        assert_eq!(budget.in_use(), 0);
    }
    #[test]
    fn cancelled_display_preparation_does_not_publish_or_reserve() {
        let pixels = Pixels::from_image(image::RgbaImage::new(3, 2)).unwrap();
        let cancel = crate::Cancellation::default();
        cancel.cancel();
        assert!(matches!(
            pixels.prepare_display(&cancel),
            Err(ToolError::Cancelled)
        ));
        assert!(pixels.display.get().is_none());
    }
    #[test]
    fn actual_snapshot_and_pixel_buffers_keep_capacity_owned_across_queued_moves() {
        let budget = Budget::new(4096);
        let bytes = vec![7u8; 256];
        let expected = bytes.capacity() as u64;
        let snapshot =
            Arc::new(Snapshot::from_reserved(bytes, budget.reserve(512).unwrap()).unwrap());
        assert_eq!(budget.in_use(), expected);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(snapshot.clone()).unwrap();
        drop(snapshot);
        assert_eq!(budget.in_use(), expected);
        assert_eq!(rx.recv().unwrap().len(), 256);
        assert_eq!(budget.in_use(), 0);
        let mut operation = budget.reserve(1000).unwrap();
        let image = image::RgbaImage::new(4, 3);
        let size = image.as_raw().capacity() as u64;
        let pixels = Arc::new(Pixels::from_reserved(image, &mut operation).unwrap());
        drop(operation);
        let undo = pixels.clone();
        drop(pixels);
        assert_eq!(budget.in_use(), size);
        assert_eq!(undo.dimensions(), (4, 3));
        drop(undo);
        assert_eq!(budget.in_use(), 0);
    }
    #[test]
    fn concurrent_reservations_cannot_overbook_and_failed_resize_preserves_accounting() {
        let budget = Budget::new(1000);
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let b = budget.clone();
            let gate = barrier.clone();
            threads.push(std::thread::spawn(move || {
                let held = b.reserve(150);
                gate.wait();
                drop(held);
            }));
        }
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(budget.peak() <= 1000);
        assert_eq!(budget.in_use(), 0);
        let mut held = budget.reserve(300).unwrap();
        held.resize(900).unwrap();
        assert_eq!(budget.in_use(), 900);
        assert!(held.resize(1001).is_err());
        assert_eq!(budget.in_use(), 900);
        held.resize(100).unwrap();
        assert_eq!(budget.in_use(), 100);
        drop(held);
        assert_eq!(budget.in_use(), 0);
    }
    #[test]
    fn split_queue_and_last_arc_reference_keep_budget_until_actual_drop() {
        let budget = Budget::new(1000);
        let mut peak = budget.reserve(800).unwrap();
        let owned = Arc::new(peak.split(300).unwrap());
        drop(peak);
        assert_eq!(budget.in_use(), 300);
        let (tx, rx) = std::sync::mpsc::channel();
        let queued = owned.clone();
        std::thread::spawn(move || tx.send(queued).unwrap())
            .join()
            .unwrap();
        drop(owned);
        assert_eq!(budget.in_use(), 300);
        let consumed = rx.recv().unwrap();
        let history = consumed.clone();
        drop(consumed);
        assert_eq!(budget.in_use(), 300);
        drop(history);
        assert_eq!(budget.in_use(), 0);
        assert_eq!(budget.peak(), 800);
    }
    #[test]
    fn dropped_channel_and_cancelled_result_release_their_ownership() {
        let budget = Budget::new(1000);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(budget.reserve(300).unwrap()).unwrap();
        drop(rx);
        drop(tx);
        assert_eq!(budget.in_use(), 0);
        let held = budget.reserve(900).unwrap();
        assert!(budget.reserve(101).is_err());
        drop(held);
        assert!(budget.reserve(1000).is_ok());
        assert_eq!(budget.in_use(), 0);
    }
}
