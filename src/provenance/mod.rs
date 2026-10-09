pub mod report;
use crate::{Cancellation, Result, ToolError, pipeline};
pub use report::*;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(windows)]
mod windows_job {
    use std::{os::windows::io::AsRawHandle, process::Child};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::JobObjects::*,
    };
    pub struct Job(HANDLE);
    impl Job {
        pub fn assign(child: &Child) -> std::io::Result<Self> {
            // Own and close the handle; a job limits this single worker's resources and lifetime.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let job = Self(handle);
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags =
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
                limits.ProcessMemoryLimit = 512 * 1024 * 1024;
                if SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of_val(&limits) as u32,
                ) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if AssignProcessToJobObject(handle, child.as_raw_handle() as HANDLE) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(job)
            }
        }
    }
    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn worker_path() -> Result<PathBuf> {
    let current = std::env::current_exe()?;
    let parent = current
        .parent()
        .ok_or_else(|| ToolError::Invalid("无法定位 worker".into()))?;
    let name = if cfg!(windows) {
        "gloamere-image-worker.exe"
    } else {
        "gloamere-image-worker"
    };
    let candidate = parent.join(name);
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(ToolError::Invalid(
            "离线来源 worker 未安装；查看与编辑仍可使用".into(),
        ))
    }
}

pub fn inspect(path: &Path, expected_hash: &str, cancel: &Cancellation) -> Result<EvidenceReport> {
    let snapshot = pipeline::read_snapshot(path, cancel)?;
    let hash = format!("{:x}", Sha256::digest(&snapshot));
    if hash != expected_hash {
        return Err(ToolError::Invalid(
            "文件已变化，请重新打开后核验；不会复用原文件的来源结果".into(),
        ));
    }
    inspect_owned(snapshot, &worker_path()?, cancel)
}

pub fn inspect_snapshot(
    snapshot: Vec<u8>,
    worker: &Path,
    cancel: &Cancellation,
) -> Result<EvidenceReport> {
    inspect_owned(
        crate::memory::Snapshot::from_bytes(snapshot)?,
        worker,
        cancel,
    )
}
fn inspect_owned(
    snapshot: crate::memory::Snapshot,
    worker: &Path,
    cancel: &Cancellation,
) -> Result<EvidenceReport> {
    cancel.check()?;
    if snapshot.is_empty() || snapshot.len() as u64 > MAX_INPUT_BYTES {
        return Err(ToolError::Invalid("来源快照超过输入限制".into()));
    }
    let expected_hash = format!("{:x}", Sha256::digest(&snapshot));
    let mut command = Command::new(worker);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    #[cfg(windows)]
    let _job = match windows_job::Job::assign(&child) {
        Ok(j) => j,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ToolError::Invalid(format!("worker 资源限制未能建立：{e}")));
        }
    };
    let mut input = child.stdin.take().unwrap();
    let (input_tx, input_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = input
            .write_all(&(snapshot.len() as u64).to_le_bytes())
            .and_then(|_| input.write_all(&snapshot));
        drop(input);
        let _ = input_tx.send(result);
    });
    let output = child.stdout.take().unwrap();
    let (output_tx, output_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = output
            .take(MAX_REPORT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = output_tx.send(result);
    });
    let started = Instant::now();
    let status = loop {
        if cancel.check().is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ToolError::Cancelled);
        }
        if started.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ToolError::Invalid(
                "来源核验超时（20 秒）；结果不完整".into(),
            ));
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        return Err(ToolError::Invalid(format!(
            "来源 worker 失败（{status}）；不能视为无凭证"
        )));
    }
    input_rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_| ToolError::Invalid("worker 输入未完成".into()))??;
    let output = output_rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_| ToolError::Invalid("worker 返回未完成".into()))??;
    if output.len() > MAX_REPORT_BYTES {
        return Err(ToolError::Invalid("来源报告超过大小限制".into()));
    }
    let report: EvidenceReport = serde_json::from_slice(&output)
        .map_err(|e| ToolError::Invalid(format!("来源报告无效：{e}")))?;
    if report.file_sha256 != expected_hash
        || report.policy_version != POLICY_VERSION
        || report.trust_snapshot != TRUST_SNAPSHOT
    {
        return Err(ToolError::Invalid("来源报告身份或策略不匹配".into()));
    }
    cancel.check()?;
    Ok(report)
}

#[derive(Default)]
pub struct EvidenceCache {
    entries: VecDeque<(EvidenceReport, usize)>,
    bytes: usize,
}
impl EvidenceCache {
    pub fn get(&self, hash: &str) -> Option<EvidenceReport> {
        self.entries
            .iter()
            .rev()
            .find(|(r, _)| {
                r.file_sha256 == hash
                    && r.policy_version == POLICY_VERSION
                    && r.trust_snapshot == TRUST_SNAPSHOT
            })
            .map(|(r, _)| r.clone())
    }
    pub fn insert(&mut self, report: EvidenceReport) {
        let size = serde_json::to_vec(&report)
            .map(|j| j.len())
            .unwrap_or(MAX_REPORT_BYTES + 1);
        if size > MAX_REPORT_BYTES {
            return;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|(r, _)| r.key() == report.key())
            && let Some((_, old)) = self.entries.remove(index)
        {
            self.bytes -= old;
        }
        while self.bytes + size > 1024 * 1024 {
            if let Some((_, old)) = self.entries.pop_front() {
                self.bytes -= old;
            } else {
                break;
            }
        }
        self.bytes += size;
        self.entries.push_back((report, size));
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}
