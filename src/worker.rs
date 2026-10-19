//! Main-image adapter over the same bounded, owned worker used by browsing.
use crate::{
    Cancellation, Result,
    browsing::{Request, Worker},
    pipeline::{self, LoadedImage},
};
use std::path::PathBuf;

pub use crate::browsing::ShutdownReport;

pub struct LoadRequest {
    pub generation: u64,
    pub path: PathBuf,
    pub cancel: Cancellation,
}
pub struct LoadResponse {
    pub generation: u64,
    pub result: Result<LoadedImage>,
    pub siblings: Vec<PathBuf>,
}
pub struct Loader {
    worker: Worker<LoadedImage>,
}
impl Loader {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        Self {
            worker: Worker::new("image-loader", ctx, pipeline::load),
        }
    }
    pub fn request(&self, request: LoadRequest) {
        self.worker.request(Request {
            generation: request.generation,
            path: request.path,
            cancel: request.cancel,
        });
    }
    pub fn take_result(&self) -> Option<LoadResponse> {
        self.worker.take_result().map(|response| LoadResponse {
            generation: response.generation,
            result: response.result,
            // Directory scanning stays independent of successfully decoded pixels.
            siblings: vec![response.path],
        })
    }
    pub fn cancel(&self) {
        self.worker.cancel();
    }
    pub fn is_busy(&self) -> bool {
        self.worker.is_busy()
    }
    pub fn shutdown(&mut self) -> ShutdownReport {
        self.worker.shutdown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_result(loader: &Loader) -> LoadResponse {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(response) = loader.take_result() {
                return response;
            }
            assert!(Instant::now() < deadline, "loader result deadline");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn loader_recovers_from_bad_file_and_preserves_selected_path_and_generation() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.png");
        let image = dir.path().join("图片2.png");
        image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]))
            .save(&image)
            .unwrap();
        let mut loader = Loader::new(eframe::egui::Context::default());
        loader.request(LoadRequest {
            generation: 1,
            path: missing.clone(),
            cancel: Cancellation::default(),
        });
        let error = wait_result(&loader);
        assert_eq!(error.generation, 1);
        assert_eq!(error.siblings, vec![missing]);
        assert!(error.result.is_err());
        loader.request(LoadRequest {
            generation: 2,
            path: image.clone(),
            cancel: Cancellation::default(),
        });
        let response = wait_result(&loader);
        assert_eq!(response.generation, 2);
        assert_eq!(response.siblings, vec![image]);
        let loaded = response.result.unwrap();
        assert_eq!(loaded.pixels.dimensions(), (2, 3));
        assert!(!loader.is_busy());
        let report = loader.shutdown();
        assert!(!report.join_panicked);
        assert_eq!(loader.shutdown(), report);
        let token = Cancellation::default();
        loader.request(LoadRequest {
            generation: 3,
            path: dir.path().join("unused.png"),
            cancel: token.clone(),
        });
        assert!(token.check().is_err());
        assert!(loader.take_result().is_none());
    }
}
