use crate::{Result, ToolError};
use std::{
    ops::Deref,
    sync::{
        Arc, LazyLock,
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
}
impl Pixels {
    pub fn from_reserved(image: image::RgbaImage, reservation: &mut Reservation) -> Result<Self> {
        let owned = reservation.split(image.as_raw().capacity() as u64)?;
        Ok(Self {
            image,
            _reservation: owned,
        })
    }
    pub fn from_image(image: image::RgbaImage) -> Result<Self> {
        let mut r = reserve(image.as_raw().capacity() as u64)?;
        Self::from_reserved(image, &mut r)
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
