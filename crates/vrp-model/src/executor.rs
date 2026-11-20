//! 同一进程内共享的有界执行入口。先取得全部线程额度，再编译和求解。
use crate::{ModelError, ThreadBudget, compile, resolve_options, solve};
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;
use vrp_proto::engine::v1 as pb;

#[derive(Debug)]
pub enum ExecutionError {
    Model(ModelError),
    QueueFull,
    /// 在排队期间或获得额度之前取消；尚未开始搜索，没有部分结果。
    Cancelled,
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(e) => e.fmt(f),
            Self::QueueFull => f.write_str("solver execution queue is full"),
            Self::Cancelled => f.write_str("solver request was cancelled before execution"),
        }
    }
}
impl std::error::Error for ExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Model(e) => Some(e),
            _ => None,
        }
    }
}
impl From<ModelError> for ExecutionError {
    fn from(value: ModelError) -> Self {
        Self::Model(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionStatus {
    pub total_threads: u32,
    /// 包括编译、搜索、结果解释期间持有的额度。
    pub reserved_threads: u32,
    pub running_jobs: usize,
    pub queued_jobs: usize,
}

#[derive(Default)]
struct Queue {
    reserved_threads: u32,
    running_jobs: usize,
    next_id: u64,
    waiting: VecDeque<u64>,
}

/// 可由多个调用线程共享（例如放进 Arc），所有任务共同消耗同一预算。
///
/// 同步阻塞接口，不自行创建排队线程；宿主应限制提交并发数。
/// 多进程部署仍需共享外部额度，或先给各进程分配互不重叠的预算。
pub struct Executor {
    budget: ThreadBudget,
    max_queued: usize,
    queue: Mutex<Queue>,
    changed: Condvar,
}

impl Executor {
    /// max_queued=0 表示仅接收能立即执行的任务；队列满时显式返回 QueueFull。
    pub fn new(budget: ThreadBudget, max_queued: usize) -> Result<Self, ModelError> {
        if budget.total_threads == 0 {
            return Err(ModelError(
                "executor requires a positive thread budget".into(),
            ));
        }
        Ok(Self {
            budget,
            max_queued,
            queue: Mutex::new(Queue::default()),
            changed: Condvar::new(),
        })
    }

    /// 只在取得额度后编译大矩阵，排队不消耗求解时限。
    /// 搜索开始后取消则正常返回带 cancelled 停止原因的最佳方案。
    pub fn solve(
        &self,
        request: &pb::SolveRequest,
        cancelled: &AtomicBool,
    ) -> Result<pb::SolveResult, ExecutionError> {
        let options = resolve_options(request.options.as_ref())?;
        let _permit = self.reserve(&options, cancelled)?;
        let compiled = compile(request)?;
        Ok(solve(&compiled, cancelled)?)
    }

    pub fn status(&self) -> ExecutionStatus {
        let queue = self.queue.lock().expect("executor queue poisoned");
        ExecutionStatus {
            total_threads: self.budget.total_threads,
            reserved_threads: queue.reserved_threads,
            running_jobs: queue.running_jobs,
            queued_jobs: queue.waiting.len(),
        }
    }

    /// Reserve the same budget for an isolated child process. Keep the permit
    /// alive until the child and its I/O threads have been reaped.
    pub fn reserve(
        &self,
        options: &crate::ResolvedSolveOptions,
        cancelled: &AtomicBool,
    ) -> Result<ExecutionPermit<'_>, ExecutionError> {
        self.budget.validate_request(options)?;
        self.acquire(options.threads, cancelled)
    }

    /// Reserve a shared allowance for a non-solver task, such as geographic
    /// preparation. The same permit may subsequently run a reserved solver.
    pub fn reserve_threads(
        &self,
        threads: u32,
        cancelled: &AtomicBool,
    ) -> Result<ExecutionPermit<'_>, ExecutionError> {
        if threads == 0 || threads > self.budget.total_threads {
            return Err(
                ModelError("task threads must fit the positive executor budget".into()).into(),
            );
        }
        self.acquire(threads, cancelled)
    }

    fn acquire(
        &self,
        threads: u32,
        cancelled: &AtomicBool,
    ) -> Result<ExecutionPermit<'_>, ExecutionError> {
        debug_assert!(threads > 0 && threads <= self.budget.total_threads);
        let mut queue = self.queue.lock().expect("executor queue poisoned");
        if cancelled.load(Ordering::Acquire) {
            return Err(ExecutionError::Cancelled);
        }
        if queue.waiting.is_empty() && threads <= self.budget.total_threads - queue.reserved_threads
        {
            queue.reserved_threads += threads;
            queue.running_jobs += 1;
            return Ok(ExecutionPermit {
                executor: self,
                threads,
            });
        }
        if queue.waiting.len() >= self.max_queued {
            return Err(ExecutionError::QueueFull);
        }
        let id = queue.next_id;
        queue.next_id = id
            .checked_add(1)
            .ok_or_else(|| ModelError("executor queue sequence exhausted".into()))?;
        queue.waiting.push_back(id);
        loop {
            if cancelled.load(Ordering::Acquire) {
                queue.waiting.retain(|&waiting| waiting != id);
                self.changed.notify_all();
                return Err(ExecutionError::Cancelled);
            }
            if queue.waiting.front() == Some(&id)
                && threads <= self.budget.total_threads - queue.reserved_threads
            {
                queue.waiting.pop_front();
                queue.reserved_threads += threads;
                queue.running_jobs += 1;
                // 队首已变更，仍有余量时让下一个请求继续申领。
                self.changed.notify_all();
                return Ok(ExecutionPermit {
                    executor: self,
                    threads,
                });
            }
            // AtomicBool 本身不唤醒 Condvar，最多 25 ms 复查一次排队取消。
            queue = self
                .changed
                .wait_timeout(queue, Duration::from_millis(25))
                .expect("executor queue poisoned")
                .0;
        }
    }
}

#[must_use = "keep the permit until the task has completely stopped"]
pub struct ExecutionPermit<'a> {
    executor: &'a Executor,
    threads: u32,
}
impl ExecutionPermit<'_> {
    /// Verify a reservation belongs to this executor and covers the task.
    pub fn covers(&self, executor: &Executor, threads: u32) -> bool {
        std::ptr::eq(self.executor, executor) && threads > 0 && threads <= self.threads
    }
}
impl Drop for ExecutionPermit<'_> {
    fn drop(&mut self) {
        let mut queue = self
            .executor
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        queue.reserved_threads -= self.threads;
        queue.running_jobs -= 1;
        self.executor.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    fn wait_for_queue(executor: &Executor, count: usize) {
        let start = Instant::now();
        while executor.status().queued_jobs != count {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "queue did not reach {count}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn fifo_reserves_all_requested_threads_and_rejects_a_full_queue() {
        let executor = Executor::new(ThreadBudget { total_threads: 3 }, 2).unwrap();
        let cancelled = AtomicBool::new(false);
        let first = executor.acquire(2, &cancelled).unwrap();
        let (started, received) = mpsc::channel();
        let (release, released) = mpsc::channel();
        std::thread::scope(|scope| {
            let executor = &executor;
            let cancelled = &cancelled;
            let started_a = started.clone();
            scope.spawn(move || {
                let _permit = executor.acquire(3, cancelled).unwrap();
                started_a.send("a").unwrap();
                released.recv().unwrap();
            });
            wait_for_queue(executor, 1);
            scope.spawn(move || {
                let _permit = executor.acquire(1, cancelled).unwrap();
                started.send("b").unwrap();
            });
            wait_for_queue(executor, 2);
            // 虽然还有 1 个额度，也不能越过队首的 3 线程任务。
            assert!(matches!(
                executor.acquire(1, cancelled),
                Err(ExecutionError::QueueFull)
            ));
            assert_eq!(executor.status().reserved_threads, 2);
            drop(first);
            assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), "a");
            assert_eq!(executor.status().reserved_threads, 3);
            assert_eq!(executor.status().queued_jobs, 1);
            release.send(()).unwrap();
            assert_eq!(received.recv_timeout(Duration::from_secs(5)).unwrap(), "b");
        });
        assert_eq!(
            executor.status(),
            ExecutionStatus {
                total_threads: 3,
                reserved_threads: 0,
                running_jobs: 0,
                queued_jobs: 0
            }
        );
    }

    #[test]
    fn cancelling_a_queued_request_removes_it_without_leaking_capacity() {
        let executor = Executor::new(ThreadBudget { total_threads: 2 }, 1).unwrap();
        let cancelled = AtomicBool::new(false);
        let first = executor.acquire(1, &cancelled).unwrap();
        let queued_cancel = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let queued = scope.spawn(|| executor.acquire(2, &queued_cancel).map(drop));
            wait_for_queue(&executor, 1);
            queued_cancel.store(true, Ordering::Release);
            assert!(matches!(
                queued.join().unwrap(),
                Err(ExecutionError::Cancelled)
            ));
            assert_eq!(executor.status().queued_jobs, 0);
            assert_eq!(executor.status().reserved_threads, 1);
            let _second = executor.acquire(1, &cancelled).unwrap();
            assert_eq!(executor.status().reserved_threads, 2);
        });
        drop(first);
        assert_eq!(executor.status().reserved_threads, 0);
    }

    #[test]
    fn permits_return_on_error_and_unwind_and_pre_cancelled_requests_do_not_run() {
        let executor = Executor::new(ThreadBudget { total_threads: 2 }, 0).unwrap();
        let cancelled = AtomicBool::new(false);
        let unwind = std::panic::catch_unwind(|| {
            let _permit = executor.acquire(2, &cancelled).unwrap();
            panic!("simulated task failure outside the queue lock");
        });
        assert!(unwind.is_err());
        assert_eq!(executor.status().reserved_threads, 0);
        // options 合法、problem 缺失：在拿到额度后编译失败，也必须释放。
        assert!(matches!(
            executor.solve(&pb::SolveRequest::default(), &cancelled),
            Err(ExecutionError::Model(_))
        ));
        assert_eq!(executor.status().reserved_threads, 0);
        cancelled.store(true, Ordering::Release);
        assert!(matches!(
            executor.solve(&pb::SolveRequest::default(), &cancelled),
            Err(ExecutionError::Cancelled)
        ));
        assert_eq!(executor.status().running_jobs, 0);
    }

    #[test]
    fn an_oversized_request_is_rejected_instead_of_queued_forever() {
        assert!(Executor::new(ThreadBudget { total_threads: 0 }, 1).is_err());
        let executor = Executor::new(ThreadBudget { total_threads: 2 }, 1).unwrap();
        let request = pb::SolveRequest {
            options: Some(pb::SolveOptions {
                threads: 3,
                ..Default::default()
            }),
            ..Default::default()
        };
        let error = executor
            .solve(&request, &AtomicBool::new(false))
            .unwrap_err();
        assert!(error.to_string().contains("exceeds total thread budget"));
        assert_eq!(executor.status().queued_jobs, 0);
    }
}
