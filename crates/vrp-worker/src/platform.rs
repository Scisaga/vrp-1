use std::io;
use std::process::{Child, Command};

#[cfg(windows)]
mod implementation {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    pub struct Containment {
        job: OwnedHandle,
    }
    pub fn configure(command: &mut Command, _: Option<u64>) -> io::Result<()> {
        command.creation_flags(CREATE_NO_WINDOW);
        Ok(())
    }
    pub fn exited(child: &mut Child) -> io::Result<bool> {
        // The owned Windows process handle keeps the process object alive.
        Ok(child.try_wait()?.is_some())
    }
    impl Containment {
        pub fn attach(child: &Child, memory: Option<u64>) -> io::Result<Self> {
            // SAFETY: null names/security select the documented unnamed default job.
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: this function owns the newly created handle exactly once.
            let job = unsafe { OwnedHandle::from_raw_handle(raw) };
            // SAFETY: a zeroed limit record has valid zero flags, scalars and counters.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if let Some(bytes) = memory {
                limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
                limits.JobMemoryLimit = usize::try_from(bytes).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "memory limit exceeds usize")
                })?;
            }
            // SAFETY: both handles are live; the record size and class match its type.
            if unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of_val(&limits) as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            // Child waits for its first IPC frame, which is only sent after attachment.
            if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { job })
        }
        pub fn terminate(&self) {
            // SAFETY: owned live job; terminating an already empty job is harmless.
            unsafe {
                TerminateJobObject(self.job.as_raw_handle(), 1);
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod implementation {
    use super::*;
    use std::os::unix::process::CommandExt;
    pub struct Containment {
        group: libc::pid_t,
    }
    pub fn exited(child: &mut Child) -> io::Result<bool> {
        // Keep the child as a zombie until its group is terminated, preventing
        // PID/group-ID reuse between detecting exit and cleaning up descendants.
        // SAFETY: initialized siginfo buffer, owned child PID, no reaping (WNOWAIT).
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }
    pub fn configure(command: &mut Command, memory: Option<u64>) -> io::Result<()> {
        command.process_group(0);
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: pre_exec calls only libc system-call wrappers, no allocation or locks.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::Error::from_raw_os_error(libc::ESRCH));
                }
                let core_dump = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::setrlimit(libc::RLIMIT_CORE, &core_dump) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if let Some(bytes) = memory {
                    let limit = libc::rlimit {
                        rlim_cur: bytes as libc::rlim_t,
                        rlim_max: bytes as libc::rlim_t,
                    };
                    if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        Ok(())
    }
    impl Containment {
        pub fn attach(child: &Child, _: Option<u64>) -> io::Result<Self> {
            Ok(Self {
                group: child.id() as libc::pid_t,
            })
        }
        pub fn terminate(&self) {
            // SAFETY: positive child group ID, isolated by process_group(0).
            unsafe {
                libc::kill(-self.group, libc::SIGKILL);
            }
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod implementation {
    use super::*;
    pub struct Containment;
    pub fn exited(_: &mut Child) -> io::Result<bool> {
        unreachable!()
    }
    pub fn configure(_: &mut Command, _: Option<u64>) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process isolation is supported on Windows and Linux",
        ))
    }
    impl Containment {
        pub fn attach(_: &Child, _: Option<u64>) -> io::Result<Self> {
            unreachable!()
        }
        pub fn terminate(&self) {}
    }
}
pub use implementation::*;
