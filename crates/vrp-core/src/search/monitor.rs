//! In-memory progress only; the host owns polling and all external I/O.
use crate::Score;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct SearchProgress {
    pub elapsed: Duration,
    pub best_found: Duration,
    pub score: Score,
    pub evaluated_moves: u64,
    pub finished: bool,
}

/// Actual incumbent events, opt-in for offline evaluation. No I/O in the core.
#[derive(Clone, Copy, Debug)]
pub struct SearchEvent {
    pub elapsed: Duration,
    pub score: Score,
    pub assigned: usize,
}

struct Current {
    started: Instant,
    best_found: Duration,
    score: Score,
    finished: Option<Duration>,
}

/// One monitor per solve; a supervisor may poll it without blocking search on I/O.
#[derive(Default)]
pub struct SearchMonitor {
    current: Mutex<Option<Current>>,
    moves: AtomicU64,
    capture_history: bool,
    history: Mutex<Vec<SearchEvent>>,
}

impl SearchMonitor {
    pub fn with_history() -> Self {
        Self {
            capture_history: true,
            ..Self::default()
        }
    }

    /// Drain events without holding the search's incumbent lock during host I/O.
    pub fn take_history(&self) -> Vec<SearchEvent> {
        std::mem::take(&mut *self.history.lock().expect("history poisoned"))
    }

    pub fn snapshot(&self) -> Option<SearchProgress> {
        let current = self.current.lock().expect("progress poisoned");
        let c = current.as_ref()?;
        Some(SearchProgress {
            elapsed: c.finished.unwrap_or_else(|| c.started.elapsed()),
            best_found: c.best_found,
            score: c.score,
            evaluated_moves: self.moves.load(Ordering::Relaxed),
            finished: c.finished.is_some(),
        })
    }

    pub(super) fn start(&self, started: Instant, score: Score, assigned: usize) {
        self.moves.store(0, Ordering::Relaxed);
        *self.current.lock().expect("progress poisoned") = Some(Current {
            started,
            best_found: Duration::ZERO,
            score,
            finished: None,
        });
        if self.capture_history {
            self.history
                .lock()
                .expect("history poisoned")
                .push(SearchEvent {
                    elapsed: started.elapsed(),
                    score,
                    assigned,
                });
        }
    }

    pub(super) fn improve(&self, score: Score, best_found: Duration, assigned: usize) {
        if let Some(c) = self.current.lock().expect("progress poisoned").as_mut() {
            c.score = score;
            c.best_found = best_found;
            if self.capture_history {
                self.history
                    .lock()
                    .expect("history poisoned")
                    .push(SearchEvent {
                        elapsed: best_found,
                        score,
                        assigned,
                    });
            }
        }
    }

    pub(super) fn finish(&self, elapsed: Duration) {
        if let Some(c) = self.current.lock().expect("progress poisoned").as_mut() {
            c.finished = Some(elapsed);
        }
    }
}

pub(super) struct MoveReport<'a> {
    monitor: Option<&'a SearchMonitor>,
    published: u64,
}
impl<'a> MoveReport<'a> {
    pub fn new(monitor: Option<&'a SearchMonitor>) -> Self {
        Self {
            monitor,
            published: 0,
        }
    }
    pub fn publish(&mut self, total: u64) {
        if let Some(monitor) = self.monitor {
            monitor
                .moves
                .fetch_add(total - self.published, Ordering::Relaxed);
        }
        self.published = total;
    }
}
