//! Asynchronous ESP-DL inference that never holds a camera DMA frame.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::Context;
use esp_idf_hal::{cpu::Core, task::thread::ThreadSpawnConfiguration};

use crate::camera::{self, CameraFrame, Detection};

/// Runs one inference at a time while the application continues rendering the camera preview.
pub struct DetectionWorker {
    sender: Option<SyncSender<camera::DetectionInput>>,
    available: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<Vec<Detection>>>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DetectionWorker {
    /// Start the detector on its own FreeRTOS-backed Rust thread.
    pub fn start(detector: camera::Detector) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let available = Arc::new(AtomicBool::new(true));
        let latest = Arc::new(Mutex::new(None));
        let stopping = Arc::new(AtomicBool::new(false));

        let worker_available = Arc::clone(&available);
        let worker_latest = Arc::clone(&latest);
        let worker_stopping = Arc::clone(&stopping);
        // ESP-IDF applies this configuration to the next pthread created by this task. Pinning
        // inference gives the UI task a core on which it can keep dequeuing and drawing frames.
        let task_config = ThreadSpawnConfiguration {
            stack_size: 16 * 1024,
            pin_to_core: Some(Core::Core1),
            inherit: false,
            ..Default::default()
        };
        task_config
            .set()
            .context("failed to configure ESP-DL detection task")?;

        let thread_result = std::thread::Builder::new()
            .name("detector".into())
            // ESP-DL is safe on the app's 8 KiB stack, but this isolated task also needs room
            // for its C++ call chain and error reporting.
            .stack_size(16 * 1024)
            .spawn(move || {
                run_worker(
                    detector,
                    receiver,
                    worker_available,
                    worker_latest,
                    worker_stopping,
                )
            });
        ThreadSpawnConfiguration::default()
            .set()
            .context("failed to restore the application thread configuration")?;
        let thread = thread_result.context("failed to start ESP-DL detection task")?;

        Ok(Self {
            sender: Some(sender),
            available,
            latest,
            stopping,
            thread: Some(thread),
        })
    }

    /// Submit the latest preview frame only when the worker has finished its previous inference.
    ///
    /// There is deliberately no backlog: an older camera view is never useful once a newer one
    /// is ready, and queueing would make the boxes lag seconds behind the live preview.
    pub fn submit(&self, frame: &CameraFrame<'_>) -> anyhow::Result<()> {
        if self
            .available
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let input = match frame.detection_input() {
            Ok(input) => input,
            Err(error) => {
                self.available.store(true, Ordering::Release);
                return Err(error);
            }
        };
        let sender = self
            .sender
            .as_ref()
            .expect("detection worker sender outlives submit");
        match sender.try_send(input) {
            Ok(()) => Ok(()),
            // The availability flag should make this unreachable, but retaining it protects
            // against future changes to the worker handoff without growing a stale queue.
            Err(TrySendError::Full(_)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => {
                self.available.store(true, Ordering::Release);
                anyhow::bail!("ESP-DL detection task stopped")
            }
        }
    }

    /// The boxes produced by the most recently completed inference, if any.
    pub fn latest(&self) -> anyhow::Result<Option<Vec<Detection>>> {
        self.latest
            .lock()
            .map(|detections| detections.clone())
            .map_err(|_| anyhow::anyhow!("ESP-DL detection results lock is poisoned"))
    }
}

impl Drop for DetectionWorker {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        // Disconnecting wakes a worker which is waiting for its next frame. If it is currently
        // inferring, joining waits for that one frame so its C++ model is released safely.
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                log::error!("ESP-DL detection task panicked");
            }
        }
    }
}

fn run_worker(
    mut detector: camera::Detector,
    receiver: mpsc::Receiver<camera::DetectionInput>,
    available: Arc<AtomicBool>,
    latest: Arc<Mutex<Option<Vec<Detection>>>>,
    stopping: Arc<AtomicBool>,
) {
    while !stopping.load(Ordering::Acquire) {
        let input = match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(input) => input,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        let started = Instant::now();
        match detector.detect(&input) {
            Ok(detections) => {
                log::info!(
                    "ESP-DL detection: {} result(s) in {} ms",
                    detections.len(),
                    started.elapsed().as_millis()
                );
                if let Some(detection) = detections.first() {
                    log::info!(
                        "ESP-DL best match: {} ({:.0}%)",
                        detection.label(),
                        detection.score * 100.0
                    );
                }
                if let Ok(mut current) = latest.lock() {
                    *current = Some(detections);
                } else {
                    log::error!("ESP-DL detection results lock is poisoned");
                }
            }
            Err(error) => log::error!("ESP-DL inference failed: {error:#}"),
        }
        available.store(true, Ordering::Release);
    }
}
