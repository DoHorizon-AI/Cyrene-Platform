// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/execution/sandboxd/src/sys.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Low-level system, file I/O, process, and pidfd helpers.
//! 中文：底层系统、文件 I/O、进程和 pidfd 辅助函数。

use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use std::{
    ffi::{CString, OsString},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::ffi::OsStrExt,
    },
};

#[cfg(target_os = "linux")]
use cy_kernel_api::{LaunchPlan, ProviderError};

pub(crate) fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

pub(crate) fn read_key_values(path: impl AsRef<Path>) -> BTreeMap<String, u64> {
    read_trimmed(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ')?;
            Some((key.to_string(), value.parse().ok()?))
        })
        .collect()
}

pub fn read_oom_kill_count(cgroup_path: &Path) -> u64 {
    read_key_values(cgroup_path.join("memory.events.local"))
        .get("oom_kill")
        .copied()
        .unwrap_or(0)
}

pub(crate) fn wait_until_empty(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if read_trimmed(path.join("cgroup.procs"))
            .map(|value| value.is_empty())
            .unwrap_or(false)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn open_pidfd(pid: u32) -> Option<OwnedFd> {
    // SAFETY: pidfd_open has no pointer arguments. The returned descriptor is
    // immediately transferred into OwnedFd, which closes it exactly once.
    // 中文：安全性：pidfd_open 不接收指针参数。返回的文件描述符会立即交给 OwnedFd 管理，由其恰好关闭一次。
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    (fd >= 0).then(|| {
        // SAFETY: fd is a newly returned, owned descriptor from pidfd_open.
        // 中文：安全性：fd 是 pidfd_open 新返回且由当前代码拥有的文件描述符。
        unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) }
    })
}

pub(crate) fn pidfd_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        open_pidfd(std::process::id()).is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct GatedProcess {
    pid: u32,
    gate: fs::File,
    exec_error: fs::File,
    stdin: Option<fs::File>,
    stdout: Option<fs::File>,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct ForkedProcess {
    pub(crate) pid: u32,
    pub(crate) stdin: Option<fs::File>,
    pub(crate) stdout: Option<fs::File>,
}

#[cfg(target_os = "linux")]
fn pipe(flags: libc::c_int) -> Result<(fs::File, fs::File), ProviderError> {
    let mut descriptors = [0; 2];
    // SAFETY: `descriptors` points to two writable integers owned by this
    // function; the kernel initializes both ends or returns an error.
    // 中文：安全性：descriptors 指向此函数拥有的两个可写整数；内核会初始化两端，或者返回错误。
    let result = unsafe { libc::pipe2(descriptors.as_mut_ptr(), flags) };
    if result != 0 {
        return Err(ProviderError::new(
            "linux-process-runtime",
            "PIPE_CREATE_FAILED",
            &std::io::Error::last_os_error().to_string(),
        ));
    }
    // SAFETY: successful pipe2 returns two owned descriptors exactly once.
    // 中文：安全性：pipe2 成功后会恰好返回两个由当前代码拥有的文件描述符。
    Ok(unsafe {
        (
            fs::File::from_raw_fd(descriptors[0]),
            fs::File::from_raw_fd(descriptors[1]),
        )
    })
}

#[cfg(target_os = "linux")]
fn c_string(bytes: &[u8], field: &str) -> Result<CString, ProviderError> {
    CString::new(bytes).map_err(|_| {
        ProviderError::new(
            "linux-process-runtime",
            "SPAWN_ARGUMENT_INVALID",
            &format!("{field} contains an embedded NUL byte"),
        )
    })
}

#[cfg(target_os = "linux")]
fn command_environment(
    requested: &BTreeMap<String, String>,
) -> Result<Vec<CString>, ProviderError> {
    let mut environment = std::env::vars_os().collect::<BTreeMap<OsString, OsString>>();
    for (key, value) in requested {
        environment.insert(OsString::from(key), OsString::from(value));
    }
    environment
        .into_iter()
        .map(|(key, value)| {
            let mut entry = key.as_os_str().as_bytes().to_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_os_str().as_bytes());
            c_string(&entry, "environment")
        })
        .collect()
}

#[cfg(target_os = "linux")]
/// Forks a child that cannot execute the requested program until the parent
/// attaches it to the target cgroup and releases the gate.
///
/// The child performs only async-signal-safe operations after `fork`: stdio
/// setup, `prctl`, `setpgid`, a one-byte gate read, and `execve`/`execvpe`.
/// This closes the previous spawn-then-attach window for user code.
/// 中文：fork 一个子进程；在父进程将其加入目标 cgroup 并释放 gate 之前，子进程不能执行请求的程序。fork 后，子进程只执行异步信号安全操作：stdio 设置、prctl、setpgid、读取一个字节的 gate，以及 execve/execvpe。这消除了先 spawn 再 attach 期间用户代码提前运行的窗口。
pub(crate) fn spawn_gated_process(
    plan: &LaunchPlan,
    environment: &BTreeMap<String, String>,
    with_transport: bool,
) -> Result<GatedProcess, ProviderError> {
    let executable = c_string(plan.executable.as_os_str().as_bytes(), "executable")?;
    let mut arguments = vec![executable.clone()];
    arguments.extend(
        plan.args
            .iter()
            .map(|argument| c_string(argument.as_bytes(), "argument"))
            .collect::<Result<Vec<_>, _>>()?,
    );
    let mut argument_pointers = arguments
        .iter()
        .map(|argument| argument.as_ptr())
        .collect::<Vec<_>>();
    argument_pointers.push(std::ptr::null());
    let environment = command_environment(environment)?;
    let mut environment_pointers = environment
        .iter()
        .map(|entry| entry.as_ptr())
        .collect::<Vec<_>>();
    environment_pointers.push(std::ptr::null());
    let working_directory = plan
        .working_dir
        .as_ref()
        .map(|path| c_string(path.as_os_str().as_bytes(), "working directory"))
        .transpose()?;

    let (gate_read, gate_write) = pipe(0)?;
    let (error_read, error_write) = pipe(libc::O_CLOEXEC)?;
    let (parent_stdin, child_stdin, parent_stdout, child_stdout) = if with_transport {
        let (stdin_read, stdin_write) = pipe(0)?;
        let (stdout_read, stdout_write) = pipe(0)?;
        (
            Some(stdin_write),
            Some(stdin_read),
            Some(stdout_read),
            Some(stdout_write),
        )
    } else {
        (None, None, None, None)
    };

    // SAFETY: The child path below uses only async-signal-safe libc calls and
    // immediately execs or exits. All heap-backed argument vectors are built
    // before fork and remain alive until the parent returns from this helper.
    // 中文：安全性：下方子进程路径只调用异步信号安全的 libc 函数，并立即 exec 或退出。所有依赖堆内存的参数向量都在 fork 前构造，并一直存活到父进程从该辅助函数返回。
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(ProviderError::new(
            "linux-process-runtime",
            "FORK_FAILED",
            &std::io::Error::last_os_error().to_string(),
        ));
    }
    if pid == 0 {
        let fail = |error_fd: RawFd| -> ! {
            let errno = std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO)
                .to_ne_bytes();
            // SAFETY: `errno` is a four-byte stack value and `error_fd` is the
            // child-owned write end of the close-on-exec error pipe.
            // 中文：安全性：errno 是一个四字节栈变量，error_fd 是子进程拥有的 close-on-exec 错误管道写端。
            unsafe {
                let _ = libc::write(error_fd, errno.as_ptr().cast(), errno.len());
                libc::_exit(127);
            }
        };
        let close = |file: &fs::File| {
            // SAFETY: each descriptor is owned by the child after fork and is
            // closed at most once on this path.
            // 中文：安全性：fork 后每个文件描述符都由子进程拥有；此路径最多关闭一次。
            unsafe { libc::close(file.as_raw_fd()) };
        };

        close(&gate_write);
        close(&error_read);
        if let Some(file) = &parent_stdin {
            close(file);
        }
        if let Some(file) = &parent_stdout {
            close(file);
        }

        let stdin_fd = child_stdin.as_ref().map(AsRawFd::as_raw_fd).unwrap_or(-1);
        let stdout_fd = child_stdout.as_ref().map(AsRawFd::as_raw_fd).unwrap_or(-1);
        if stdin_fd >= 0 {
            // SAFETY: both descriptors are valid child-owned pipe ends.
            // 中文：安全性：这两个文件描述符都是有效且由子进程拥有的管道端。
            if unsafe { libc::dup2(stdin_fd, libc::STDIN_FILENO) } < 0 {
                fail(error_write.as_raw_fd());
            }
            close(child_stdin.as_ref().expect("stdin descriptor exists"));
        } else {
            // SAFETY: `/dev/null` is a trusted fixed path and the returned fd
            // is immediately duplicated into stdin.
            // 中文：安全性：/dev/null 是可信的固定路径，返回的 fd 会立即复制到 stdin。
            let null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY) };
            if null < 0 || unsafe { libc::dup2(null, libc::STDIN_FILENO) } < 0 {
                fail(error_write.as_raw_fd());
            }
            if null != libc::STDIN_FILENO {
                // SAFETY: `null` was returned by open and is owned by child.
                // 中文：安全性：null 由 open 返回并归子进程所有。
                unsafe { libc::close(null) };
            }
        }
        if stdout_fd >= 0 {
            // SAFETY: both descriptors are valid child-owned pipe ends.
            // 中文：安全性：这两个文件描述符都是有效且由子进程拥有的管道端。
            if unsafe { libc::dup2(stdout_fd, libc::STDOUT_FILENO) } < 0 {
                fail(error_write.as_raw_fd());
            }
            close(child_stdout.as_ref().expect("stdout descriptor exists"));
        } else {
            // SAFETY: `/dev/null` is a trusted fixed path and the returned fd
            // is immediately duplicated into stdout.
            // 中文：安全性：/dev/null 是可信的固定路径，返回的 fd 会立即复制到 stdout。
            let null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
            if null < 0 || unsafe { libc::dup2(null, libc::STDOUT_FILENO) } < 0 {
                fail(error_write.as_raw_fd());
            }
            if null != libc::STDOUT_FILENO {
                // SAFETY: `null` was returned by open and is owned by child.
                // 中文：安全性：null 由 open 返回并归子进程所有。
                unsafe { libc::close(null) };
            }
        }

        // SAFETY: These calls affect only the forked child and use scalar
        // arguments. Failure is reported through the pre-exec error pipe.
        // 中文：安全性：这些调用只影响 fork 出来的子进程，且只使用标量参数。失败信息通过 pre-exec 错误管道报告。
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
            fail(error_write.as_raw_fd());
        }
        if unsafe { libc::setpgid(0, 0) } != 0 {
            fail(error_write.as_raw_fd());
        }
        let mut gate_byte = [0_u8; 1];
        // SAFETY: the gate descriptor points to a one-byte pipe buffer owned by
        // this child. The read blocks before user code is executable.
        // 中文：安全性：gate 描述符指向该子进程拥有的单字节管道缓冲区。读取会在用户代码可执行前阻塞。
        if unsafe {
            libc::read(
                gate_read.as_raw_fd(),
                gate_byte.as_mut_ptr().cast(),
                gate_byte.len(),
            )
        } != 1
        {
            fail(error_write.as_raw_fd());
        }
        close(&gate_read);
        if let Some(path) = working_directory.as_ref() {
            // SAFETY: `path` is a NUL-terminated immutable CString prepared
            // before fork.
            // 中文：安全性：path 是 fork 前构造的、以 NUL 结尾且不可变的 CString。
            if unsafe { libc::chdir(path.as_ptr()) } != 0 {
                fail(error_write.as_raw_fd());
            }
        }
        // SAFETY: all argv/envp pointers refer to immutable CStrings prepared
        // before fork and remain valid until exec replaces the image.
        // 中文：安全性：所有 argv/envp 指针都指向 fork 前构造的不可变 CStrings，并在 exec 替换进程映像前保持有效。
        let result = if executable.as_bytes().contains(&b'/') {
            unsafe {
                libc::execve(
                    executable.as_ptr(),
                    argument_pointers.as_ptr(),
                    environment_pointers.as_ptr(),
                )
            }
        } else {
            unsafe {
                libc::execvpe(
                    executable.as_ptr(),
                    argument_pointers.as_ptr(),
                    environment_pointers.as_ptr(),
                )
            }
        };
        let _ = result;
        fail(error_write.as_raw_fd());
    }

    drop(gate_read);
    drop(error_write);
    drop(child_stdin);
    drop(child_stdout);
    Ok(GatedProcess {
        pid: pid as u32,
        gate: gate_write,
        exec_error: error_read,
        stdin: parent_stdin,
        stdout: parent_stdout,
    })
}

#[cfg(target_os = "linux")]
impl GatedProcess {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn abort(self) {
        // SAFETY: the PID came directly from fork and is still owned by this
        // parent. The wait prevents leaving a blocked child as a zombie.
        // 中文：安全性：PID 直接来自 fork，仍由该父进程拥有。此处等待可避免留下被阻塞的僵尸子进程。
        unsafe {
            let _ = libc::kill(self.pid as libc::pid_t, libc::SIGKILL);
            let mut status = 0;
            let _ = libc::waitpid(self.pid as libc::pid_t, &mut status, 0);
        }
    }

    pub(crate) fn release(mut self) -> Result<ForkedProcess, ProviderError> {
        self.gate.write_all(&[1]).map_err(|error| {
            ProviderError::new(
                "linux-process-runtime",
                "PROCESS_GATE_RELEASE_FAILED",
                &error.to_string(),
            )
        })?;
        drop(self.gate);
        let mut error_bytes = Vec::new();
        self.exec_error
            .read_to_end(&mut error_bytes)
            .map_err(|error| {
                ProviderError::new(
                    "linux-process-runtime",
                    "SPAWN_RESULT_READ_FAILED",
                    &error.to_string(),
                )
            })?;
        if !error_bytes.is_empty() {
            let errno = error_bytes
                .get(..std::mem::size_of::<i32>())
                .and_then(|bytes| bytes.try_into().ok())
                .map(i32::from_ne_bytes)
                .unwrap_or(libc::EIO);
            // SAFETY: the child belongs to this parent and has reported an
            // exec/setup failure through its close-on-exec error pipe.
            // 中文：安全性：该子进程属于当前父进程，并已通过 close-on-exec 错误管道报告 exec/setup 失败。
            unsafe {
                let mut status = 0;
                let _ = libc::waitpid(self.pid as libc::pid_t, &mut status, 0);
            }
            return Err(ProviderError::new(
                "linux-process-runtime",
                "SPAWN_EXEC_FAILED",
                &std::io::Error::from_raw_os_error(errno).to_string(),
            ));
        }
        Ok(ForkedProcess {
            pid: self.pid,
            stdin: self.stdin,
            stdout: self.stdout,
        })
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn try_wait_pid(pid: u32) -> Option<Option<i32>> {
    let mut status = 0;
    // SAFETY: `pid` is a child PID previously returned by fork or a test child
    // owned by this process; WNOHANG only observes its status.
    // 中文：安全性：pid 是 fork 返回的子进程 PID，或由当前进程拥有的测试子进程 PID；WNOHANG 只检查其状态。
    let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    if result == 0 {
        return None;
    }
    if result == pid as libc::pid_t {
        return Some(wait_status_code(status));
    }
    Some(None)
}

#[cfg(target_os = "linux")]
pub(crate) fn wait_pid(pid: u32) -> Option<i32> {
    let mut status = 0;
    // SAFETY: `pid` is a child PID owned by this process and this call reaps it
    // exactly once after the pidfd or bounded wait says it has exited.
    // 中文：安全性：pid 是当前进程拥有的子进程 PID；此调用在 pidfd 或有界等待确认其退出后恰好 reap 一次。
    let result = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
    (result == pid as libc::pid_t)
        .then(|| wait_status_code(status))
        .flatten()
}

#[cfg(target_os = "linux")]
fn wait_status_code(status: libc::c_int) -> Option<i32> {
    ((status & 0x7f) == 0).then_some((status >> 8) & 0xff)
}

pub(crate) fn proc_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()?
            .split_whitespace()
            .nth(21)?
            .parse()
            .ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}
