use crate::{
    Cancellation, Result,
    pipeline::{self, LoadedImage},
};
use std::{
    path::PathBuf,
    sync::{Arc, Condvar, Mutex},
    thread,
};

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
#[derive(Default)]
struct State {
    pending: Option<LoadRequest>,
    shutdown: bool,
}
pub struct Loader {
    shared: Arc<(Mutex<State>, Condvar)>,
    result: Arc<Mutex<Option<LoadResponse>>>,
}
impl Loader {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let result = Arc::new(Mutex::new(None));
        let input = shared.clone();
        let output = result.clone();
        thread::Builder::new()
            .name("image-loader".into())
            .spawn(move || {
                loop {
                    let request = {
                        let (lock, wake) = &*input;
                        let mut state = lock.lock().unwrap();
                        while state.pending.is_none() && !state.shutdown {
                            state = wake.wait(state).unwrap();
                        }
                        if state.shutdown {
                            break;
                        }
                        state.pending.take().unwrap()
                    };
                    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        pipeline::load(&request.path, &request.cancel)
                    }))
                    .unwrap_or_else(|_| {
                        Err(crate::ToolError::Invalid("解码器异常，原文件未修改".into()))
                    });
                    // Directory scanning is independent: a slow/inaccessible folder
                    // must never withhold a successfully decoded image.
                    let siblings = vec![request.path.clone()];
                    *output.lock().unwrap() = Some(LoadResponse {
                        generation: request.generation,
                        result: decoded,
                        siblings,
                    });
                    ctx.request_repaint();
                }
            })
            .expect("image worker creation");
        Self { shared, result }
    }
    pub fn request(&self, request: LoadRequest) {
        let (lock, wake) = &*self.shared;
        // Exactly one pending request: rapid browsing replaces queued work instead of spawning threads.
        if let Some(old) = lock.lock().unwrap().pending.replace(request) {
            old.cancel.cancel();
        }
        wake.notify_one();
    }
    pub fn take_result(&self) -> Option<LoadResponse> {
        self.result.lock().unwrap().take()
    }
}
impl Drop for Loader {
    fn drop(&mut self) {
        let (lock, wake) = &*self.shared;
        let mut state = lock.lock().unwrap();
        state.shutdown = true;
        if let Some(pending) = state.pending.take() {
            pending.cancel.cancel();
        }
        wake.notify_one();
    }
}
