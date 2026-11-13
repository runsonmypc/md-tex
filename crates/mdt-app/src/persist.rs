//! Saves of the settings and state files, off the UI thread: each is an atomic save with
//! fsync, which Zoom's key repeat or opening a window would otherwise wait on. One thread
//! runs them in the order they were asked for, so the last change made is the one kept.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{OnceLock, mpsc};

type Job = Box<dyn FnOnce() + Send>;

/// Saves asked for and not finished yet.
static PENDING: AtomicUsize = AtomicUsize::new(0);

fn queue() -> &'static mpsc::Sender<Job> {
    static QUEUE: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("md-tex-persist".into())
            .spawn(move || {
                for job in rx {
                    // A panicking save must not stop the ones after it.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                    PENDING.fetch_sub(1, Ordering::AcqRel);
                }
            })
            .expect("spawn the save thread");
        tx
    })
}

/// Runs `save` on the save thread, after every save asked for before it.
pub fn run(save: impl FnOnce() + Send + 'static) {
    PENDING.fetch_add(1, Ordering::AcqRel);
    if queue().send(Box::new(save)).is_err() {
        PENDING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Whether saves are queued or running.
pub fn pending() -> bool {
    PENDING.load(Ordering::Acquire) > 0
}

/// Waits until every save asked for so far has finished (at quit, and in tests).
pub fn flush() {
    let (tx, rx) = mpsc::channel();
    run(move || {
        let _ = tx.send(());
    });
    let _ = rx.recv();
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    #[test]
    fn saves_run_in_order_off_the_calling_thread() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let caller = std::thread::current().id();
        for i in 0..100 {
            let seen = seen.clone();
            super::run(move || {
                assert_ne!(std::thread::current().id(), caller);
                seen.lock().unwrap().push(i);
            });
        }
        super::run(|| panic!("a failing save"));
        super::flush();
        assert_eq!(*seen.lock().unwrap(), (0..100).collect::<Vec<_>>());
    }
}
