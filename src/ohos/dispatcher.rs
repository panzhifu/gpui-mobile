//! HarmonyOS task dispatcher.
//!
//! Mirrors the two-queue model used by `AndroidDispatcher` / `IosDispatcher`:
//!
//! * **Foreground / main-thread tasks** — queued locally and drained from the
//!   main thread's frame loop (driven by `OH_NativeVSync`, see `vsync.rs` /
//!   `napi.rs`).  Enqueuing a task invokes an optional *waker* so the main
//!   thread schedules an extra frame and drains the queue promptly instead of
//!   waiting for the next vsync tick.
//!
//! * **Background tasks** — dispatched onto a fixed-size Rust thread-pool
//!   backed by `std::thread`.
//!
//! ## Why not `uv_async`?
//!
//! The canonical way to wake the JS/native main thread from another thread is
//! `uv_async_send` on the loop returned by `napi_get_uv_event_loop`.  However
//! `uv_async_t` is an opaque, libuv-version-specific struct that must be
//! allocated with the exact size the runtime expects — which we cannot do
//! without the OpenHarmony headers.  Driving the queue from the vsync frame
//! callback (which runs on the main thread and is always requested
//! continuously) avoids that hazard entirely.
//!
//! `OhosDispatcher` implements `gpui::PlatformDispatcher` so it can construct
//! `BackgroundExecutor` / `ForegroundExecutor` instances.

#![allow(unsafe_code)]

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::ThreadId,
    time::{Duration, Instant},
};

use gpui::{PlatformDispatcher, Priority, RunnableVariant, ThreadTaskTimings};
use parking_lot::Mutex;

// ── task queue ────────────────────────────────────────────────────────────────

type BoxedTask = Box<dyn FnOnce() + Send + 'static>;

/// A callback that nudges the main thread to run pending work (e.g. by
/// requesting an extra vsync frame).
pub type MainThreadWaker = Box<dyn Fn() + Send + Sync + 'static>;

struct MainQueue {
    tasks: VecDeque<BoxedTask>,
    waker: Option<Arc<MainThreadWaker>>,
}

// ── thread-pool ───────────────────────────────────────────────────────────────

/// A minimal fixed-size thread-pool for background tasks.
struct ThreadPool {
    sender: std::sync::mpsc::Sender<BoxedTask>,
}

impl ThreadPool {
    fn new(threads: usize) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel::<BoxedTask>();
        let receiver = Arc::new(Mutex::new(receiver));

        for i in 0..threads {
            let rx = Arc::clone(&receiver);
            std::thread::Builder::new()
                .name(format!("gpui-ohos-bg-{}", i))
                .spawn(move || loop {
                    let task = {
                        let lock = rx.lock();
                        lock.recv()
                    };
                    match task {
                        Ok(f) => f(),
                        Err(_) => break, // channel closed
                    }
                })
                .expect("failed to spawn background thread");
        }

        ThreadPool { sender }
    }

    fn dispatch(&self, task: BoxedTask) {
        let _ = self.sender.send(task);
    }
}

// ── delayed task queue ────────────────────────────────────────────────────────

struct DelayedTask {
    due: Instant,
    task: BoxedTask,
}

// ── OhosDispatcher ────────────────────────────────────────────────────────────

/// GPUI dispatcher for HarmonyOS.
pub struct OhosDispatcher {
    main_queue: Arc<Mutex<MainQueue>>,
    /// Thread that owns the main queue.  Used by `is_main_thread`.
    main_thread: ThreadId,
    pool: ThreadPool,
    delayed: Mutex<Vec<DelayedTask>>,
    shutdown: AtomicBool,
}

impl OhosDispatcher {
    /// Create a dispatcher bound to the **current** thread as the main thread.
    ///
    /// Must be called on the OHOS UI / NAPI thread (the one that receives
    /// XComponent and vsync callbacks) so that `is_main_thread()` is correct.
    pub fn new() -> Arc<Self> {
        let pool_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .max(2);

        log::debug!(
            "OhosDispatcher created (pool_threads={}, main_thread={:?})",
            pool_threads,
            std::thread::current().id()
        );

        Arc::new(Self {
            main_queue: Arc::new(Mutex::new(MainQueue {
                tasks: VecDeque::new(),
                waker: None,
            })),
            main_thread: std::thread::current().id(),
            pool: ThreadPool::new(pool_threads),
            delayed: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
        })
    }

    /// Install a waker invoked whenever a foreground task is enqueued.
    ///
    /// `napi.rs` installs a waker that requests a vsync frame so the main
    /// thread drains the queue promptly.
    pub fn set_main_thread_waker<F>(&self, waker: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.main_queue.lock().waker = Some(Arc::new(Box::new(waker)));
    }

    /// Returns `true` if the calling thread is the main/UI thread.
    pub fn is_main_thread(&self) -> bool {
        std::thread::current().id() == self.main_thread
    }

    /// Enqueue a task to run on the main (foreground) thread.
    pub fn dispatch_on_main_thread<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let waker = {
            let mut q = self.main_queue.lock();
            q.tasks.push_back(Box::new(f));
            q.waker.clone()
        };
        if let Some(waker) = waker {
            waker();
        }
    }

    /// Enqueue a task on the background thread-pool.
    pub fn dispatch<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        self.pool.dispatch(Box::new(f));
    }

    /// Enqueue a task on the background thread-pool after `delay`.
    pub fn dispatch_after<F>(&self, delay: Duration, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let due = Instant::now() + delay;
        let mut delayed = self.delayed.lock();
        delayed.push(DelayedTask {
            due,
            task: Box::new(f),
        });
        delayed.sort_by_key(|d| d.due);
    }

    /// Process any delayed background tasks whose due time has passed, then
    /// drain any pending main-thread tasks.
    ///
    /// Called from the main-thread frame loop on every tick.
    pub fn tick(&self) {
        // 1. Promote due delayed tasks onto the pool.
        let now = Instant::now();
        let mut ready: Vec<BoxedTask> = Vec::new();
        {
            let mut delayed = self.delayed.lock();
            while delayed.first().map(|d| d.due <= now).unwrap_or(false) {
                ready.push(delayed.remove(0).task);
            }
        }
        for task in ready {
            self.pool.dispatch(task);
        }

        // 2. Drains the main-thread queue in place.
        self.drain_main_thread_tasks();
    }

    /// Drain all pending main-thread tasks synchronously.
    pub fn drain_main_thread_tasks(&self) {
        loop {
            let task = {
                let mut q = self.main_queue.lock();
                q.tasks.pop_front()
            };
            match task {
                Some(f) => f(),
                None => break,
            }
        }
    }

    /// Alias retained for parity with `AndroidDispatcher::flush_main_thread_tasks`.
    pub fn flush_main_thread_tasks(&self) {
        self.drain_main_thread_tasks();
    }

    /// Stop accepting new tasks.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

impl Drop for OhosDispatcher {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

// ── impl PlatformDispatcher ───────────────────────────────────────────────────

impl PlatformDispatcher for OhosDispatcher {
    fn get_all_timings(&self) -> Vec<ThreadTaskTimings> {
        // Task-level profiling is not implemented on HarmonyOS.
        Vec::new()
    }

    fn get_current_thread_timings(&self) -> ThreadTaskTimings {
        ThreadTaskTimings {
            thread_name: std::thread::current().name().map(|n| n.to_string()),
            thread_id: std::thread::current().id(),
            timings: Vec::new(),
            total_pushed: 0,
        }
    }

    fn is_main_thread(&self) -> bool {
        OhosDispatcher::is_main_thread(self)
    }

    fn dispatch(&self, runnable: RunnableVariant, _priority: Priority) {
        self.pool.dispatch(Box::new(move || {
            runnable.run();
        }));
    }

    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, _priority: Priority) {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let waker = {
            let mut q = self.main_queue.lock();
            q.tasks.push_back(Box::new(move || {
                runnable.run();
            }));
            q.waker.clone()
        };
        if let Some(waker) = waker {
            waker();
        }
    }

    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant) {
        if self.shutdown.load(Ordering::Relaxed) {
            return;
        }
        let due = Instant::now() + duration;
        let mut delayed = self.delayed.lock();
        delayed.push(DelayedTask {
            due,
            task: Box::new(move || {
                runnable.run();
            }),
        });
        delayed.sort_by_key(|d| d.due);
    }

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        std::thread::Builder::new()
            .name("gpui-ohos-realtime".to_string())
            .spawn(f)
            .expect("failed to spawn realtime thread");
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_tasks_run() {
        let pool = ThreadPool::new(2);
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        for _ in 0..10 {
            let c = Arc::clone(&counter);
            pool.dispatch(Box::new(move || {
                c.fetch_add(1, Ordering::Relaxed);
            }));
        }

        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(counter.load(Ordering::Relaxed), 10);
    }

    #[test]
    fn main_thread_tasks_drain() {
        let dispatcher = OhosDispatcher::new();
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        for _ in 0..5 {
            let c = Arc::clone(&counter);
            dispatcher.dispatch_on_main_thread(move || {
                c.fetch_add(1, Ordering::Relaxed);
            });
        }

        // Waker is not installed, so nothing has run yet.
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        dispatcher.tick();
        assert_eq!(counter.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn is_main_thread_true_on_creator() {
        let dispatcher = OhosDispatcher::new();
        assert!(dispatcher.is_main_thread());
    }

    #[test]
    fn is_main_thread_false_off_thread() {
        let dispatcher = OhosDispatcher::new();
        let d2 = Arc::clone(&dispatcher);
        let is_main = std::thread::spawn(move || d2.is_main_thread())
            .join()
            .unwrap();
        assert!(!is_main);
    }

    #[test]
    fn delayed_tasks_not_early() {
        let dispatcher = OhosDispatcher::new();
        let ran = Arc::new(AtomicBool::new(false));
        let r2 = Arc::clone(&ran);

        dispatcher.dispatch_after(Duration::from_secs(60), move || {
            r2.store(true, Ordering::Relaxed);
        });

        dispatcher.tick();
        assert!(!ran.load(Ordering::Relaxed));
    }
}
