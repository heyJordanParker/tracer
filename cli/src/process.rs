//! The one place trace starts a child process, and what ends trace when its
//! caller is gone.
//!
//! A trace whose caller timed out or went away is reparented to process 1 and
//! keeps building indexes at several cores, with nobody to read its answer.
//! So every child trace starts is registered while it runs, and trace ends,
//! with its children, the moment its caller is gone:
//!
//! - `SIGTERM`, `SIGINT`, and `SIGHUP` kill every registered child, then exit.
//! - A panic kills every registered child before the process aborts.
//! - A watchdog thread checks once a second whether trace's parent changed,
//!   whether a piped stdout's reader closed its end, and whether the Claude
//!   Code process `CLAUDE_PID` names is gone. Claude Code runs each Bash call
//!   in its own shell with its output in a file, so a session that ends
//!   leaves that shell, trace's parent, alive and its stdout a file:
//!   `CLAUDE_PID` is the one condition that sees it. A detached `trace cache
//!   build` watches `CLAUDE_PID` alone.
//! - Every trace stops, with its children, once it spends `CPU_LIMIT` of CPU
//!   time, so no call runs away whoever waits on it.
//!
//! `detach` starts the one child that outlives its call: a hook's background
//! index build, at most one per user at a time, at the lowest priority.
//!
//! The registry is a fixed array of atomic process ids, because a signal
//! handler may only do async-signal-safe work: it reads the array and calls
//! `kill`. `SIGKILL` can't be caught, so a caller that `SIGKILL`s trace alone
//! leaves a child that was running to finish on its own.
//!
//! `trace hook --deadline-ms` is a hard limit for the whole process: at the
//! deadline every registered child is killed and trace ends, because the host
//! throws away an answer that arrives after its own timeout.
//!
//! Extra context never holds up an answer: an index it needs that isn't
//! ready by `context_limit` is left out, and the call prints what it has.
//! A whole output, `--json`, waits for its indexes instead.

use std::io;
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

static DEADLINE: OnceLock<Instant> = OnceLock::new();

static STARTED: OnceLock<Instant> = OnceLock::new();

/// How long a call's extra context waits on an index that isn't ready. A
/// warm index answers in milliseconds, and a cold one keeps the files it
/// extracted, so later calls finish it.
const CONTEXT_BUDGET: Duration = Duration::from_secs(1);

/// What a hook keeps of its deadline to render and print what it has.
const RENDER_RESERVE: Duration = Duration::from_millis(1500);

/// Set once the call's output is whole, as `--json` is, so its extra context
/// waits for every index the way an answer does.
static WHOLE: AtomicBool = AtomicBool::new(false);

/// Makes the rest of this call's output whole: its extra context is never
/// left out.
pub fn whole() {
    WHOLE.store(true, Ordering::Relaxed);
}

/// When extra context stops waiting on an index: `CONTEXT_BUDGET` after the
/// call started, or earlier when a hook's deadline is nearer. `None` for a
/// whole output, which waits for every index.
pub fn context_limit() -> Option<Instant> {
    (!WHOLE.load(Ordering::Relaxed)).then(|| warm_limit().min(*STARTED.get_or_init(Instant::now) + CONTEXT_BUDGET))
}

/// When work that warms indexes for later calls must stop, such as the
/// primer a session starts with: the deadline less `RENDER_RESERVE`, or
/// `context_limit` for a call without one.
pub fn warm_limit() -> Instant {
    match DEADLINE.get() {
        Some(deadline) => deadline.checked_sub(RENDER_RESERVE).unwrap_or(*deadline),
        None => *STARTED.get_or_init(Instant::now) + CONTEXT_BUDGET,
    }
}

/// Ends trace, with every registered child, `limit` from now.
pub fn deadline(limit: Duration) {
    let at = Instant::now() + limit;
    if DEADLINE.set(at).is_err() {
        return;
    }
    let _ = std::thread::Builder::new().name("deadline".into()).spawn(move || {
        std::thread::sleep(at.saturating_duration_since(Instant::now()));
        abandon(ABANDONED);
    });
}

/// Whether the call's deadline, if it has one, is still ahead, so what it
/// printed reaches its host.
pub fn before_deadline() -> bool {
    DEADLINE.get().is_none_or(|at| Instant::now() < *at)
}

const SLOTS: usize = 64;
static CHILDREN: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];

/// The exit status of a trace whose caller is gone: nobody reads it, so it
/// only has to differ from success.
const ABANDONED: i32 = 1;

fn register(pid: u32) {
    let pid = pid as i32;
    for slot in &CHILDREN {
        if slot.compare_exchange(0, pid, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            return;
        }
    }
    report(&format!("trace: more than {SLOTS} child processes at once; process {pid} is not stopped with this call"));
}

fn unregister(pid: u32) {
    let pid = pid as i32;
    for slot in &CHILDREN {
        if slot.compare_exchange(pid, 0, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            return;
        }
    }
}

/// Set by `exit`, `abandon`, and a panic that aborts before they kill the
/// children. A panic that unwinds, as in a test build, leaves it unset,
/// because it ends only its own thread.
static ENDING: AtomicBool = AtomicBool::new(false);

/// Whether trace is ending, so a child that died now was killed with it and
/// says nothing about its input.
fn ending() -> bool {
    ENDING.load(Ordering::Acquire)
}

/// Kills every registered child. Async-signal-safe: atomics and `kill` only.
fn kill_children() {
    for slot in &CHILDREN {
        let pid = slot.load(Ordering::Acquire);
        if pid > 0 {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

/// A failure trace reports as `message` on stderr, exiting with `code`: 2
/// for a bad argument or input, 1 for a missing or failing dependency. An
/// empty message prints nothing, for a command that reported on stdout.
#[derive(Debug)]
pub struct TraceError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TraceError {}

/// The error a command returns for a failure `main` exits with `code` on.
pub fn fail(code: i32, message: impl Into<String>) -> anyhow::Error {
    TraceError { code, message: message.into() }.into()
}

static REPORTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Names a failure on stderr and lets the run go on, answering everything
/// else; the run's exit status is 2 when it ends. A missing path argument
/// and a `ctags` that fails both take this path, the way ripgrep reports a
/// missing path and searches the rest.
pub fn report(message: &str) {
    eprintln!("{message}");
    REPORTED.lock().unwrap_or_else(PoisonError::into_inner).push(message.to_string());
}

/// Whether the run reported a failure, so it exits 2.
pub fn reported() -> bool {
    !REPORTED.lock().unwrap_or_else(PoisonError::into_inner).is_empty()
}

/// The failures reported so far, taken by a caller that answers them itself,
/// so the run exits as if none were reported.
pub fn take_reported() -> Vec<String> {
    std::mem::take(&mut *REPORTED.lock().unwrap_or_else(PoisonError::into_inner))
}

/// The one way trace exits: every registered child is killed, stdout is
/// flushed, and the process ends with `code`.
pub fn exit(code: i32) -> ! {
    ENDING.store(true, Ordering::Release);
    kill_children();
    std::process::exit(code)
}

/// Ends a trace nobody waits on: every registered child is killed, and
/// nothing is flushed, because stdout may be a closed pipe or held by the
/// thread doing the work.
fn abandon(code: i32) -> ! {
    ENDING.store(true, Ordering::Release);
    kill_children();
    unsafe { libc::_exit(code) }
}

extern "C" fn on_signal(signal: libc::c_int) {
    abandon(128 + signal)
}

/// The CPU time one trace process may spend, every thread counted. A cold
/// build of a 9,000-file repository spends under a minute, so only a call
/// that went wrong reaches it.
const CPU_LIMIT: Duration = Duration::from_secs(600);

/// Lowers the soft `RLIMIT_CPU` to `CPU_LIMIT`, or keeps a lower limit the
/// caller set, so the kernel sends `SIGXCPU` once trace spends it.
fn limit_cpu() {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_CPU, &mut limit) } != 0 {
        return;
    }
    limit.rlim_cur = limit.rlim_cur.min(CPU_LIMIT.as_secs() as libc::rlim_t);
    unsafe {
        libc::setrlimit(libc::RLIMIT_CPU, &limit);
    }
}

extern "C" fn on_cpu_limit(signal: libc::c_int) {
    const STOPPED: &[u8] = b"trace: stopped, because it spent its limit of CPU time\n";
    unsafe {
        libc::write(libc::STDERR_FILENO, STOPPED.as_ptr().cast(), STOPPED.len());
    }
    abandon(128 + signal)
}

/// Marks every descriptor inherited above stderr close-on-exec, installs the
/// signal handlers and the panic hook, limits the CPU time, and starts the
/// watchdog. `trace cache build` passes `watch_caller` false, because a
/// person or a hook detaches it on purpose, so only the end of its Claude
/// Code session ends it.
pub fn guard(watch_caller: bool) {
    STARTED.get_or_init(Instant::now);
    close_inherited_on_exec();
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe {
            libc::signal(signal, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
    }
    unsafe {
        libc::signal(libc::SIGXCPU, on_cpu_limit as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
    limit_cpu();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if cfg!(panic = "abort") {
            ENDING.store(true, Ordering::Release);
        }
        kill_children();
        previous(info);
    }));
    watch(watch_caller);
}

/// A descriptor trace inherited stays open in every child it starts, so a
/// detached build would hold a pipe its caller's caller waits on. Every
/// descriptor trace opens itself is already close-on-exec.
fn close_inherited_on_exec() {
    let Ok(entries) = std::fs::read_dir("/dev/fd") else {
        return;
    };
    let inherited: Vec<libc::c_int> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .filter(|fd| *fd > libc::STDERR_FILENO)
        .collect();
    for fd in inherited {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

/// Ends trace once the Claude Code process `CLAUDE_PID` names is gone and,
/// with `watch_caller`, once its parent changes or a piped stdout's reader
/// closes its end.
fn watch(watch_caller: bool) {
    let parent = std::os::unix::process::parent_id();
    let claude = std::env::var("CLAUDE_PID").ok().and_then(|pid| pid.parse::<i32>().ok());
    if !watch_caller && claude.is_none() {
        return;
    }
    let piped = watch_caller && stdout_is_pipe();
    let _ = std::thread::Builder::new().name("watchdog".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if (watch_caller && std::os::unix::process::parent_id() != parent)
            || claude.is_some_and(|pid| !alive(pid))
            || (piped && hung_up(libc::STDOUT_FILENO))
        {
            abandon(ABANDONED);
        }
    });
}

/// The niceness of a detached command: the lowest scheduling priority, so it
/// runs on the CPU nothing else is waiting for. macOS's background band
/// would also coalesce its timers and stall the watchdog that ends it.
const LOWEST_PRIORITY: libc::c_int = 19;

/// Starts `command` apart from this call: in its own process group, with no
/// stdio and no registry slot, so it outlives this call and no signal sent to
/// this call reaches it. The child takes `lock`'s exclusive `flock` before it
/// runs and holds it to its end, so one lock file runs one such command at a
/// time; a start that finds it held fails with `WouldBlock`. It runs at
/// `LOWEST_PRIORITY`, which every process it starts inherits.
pub fn detach(command: &mut Command, lock: &std::path::Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    let lock = std::ffi::CString::new(lock.as_os_str().as_bytes())?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    // Between fork and exec only async-signal-safe calls run: `open` and
    // `flock` on a path prepared before the fork, and `setpriority`. The
    // descriptor is not close-on-exec, so the command holds the lock.
    unsafe {
        command.pre_exec(move || {
            let fd = libc::open(lock.as_ptr(), libc::O_RDWR | libc::O_CREAT, 0o600);
            if fd < 0 || libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) != 0 {
                return Err(io::Error::last_os_error());
            }
            libc::setpriority(libc::PRIO_PROCESS, 0, LOWEST_PRIORITY);
            Ok(())
        });
    }
    command.spawn().map(drop)
}

fn alive(pid: i32) -> bool {
    let signalled = unsafe { libc::kill(pid, 0) == 0 };
    signalled || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether stdout is a pipe or a socket, the two outputs whose reader can go
/// away while trace's parent lives on, as `trace … | head` does.
fn stdout_is_pipe() -> bool {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let kind = (unsafe { libc::fstat(libc::STDOUT_FILENO, &mut stat) } == 0).then_some(stat.st_mode & libc::S_IFMT);
    matches!(kind, Some(libc::S_IFIFO | libc::S_IFSOCK))
}

/// Whether the reader of the pipe `fd` writes to closed its end, so nothing
/// written to it is read. macOS reports `POLLHUP` only when `POLLOUT` is
/// requested, and Linux reports `POLLERR`.
fn hung_up(fd: libc::c_int) -> bool {
    let mut pipe = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
    let ready = unsafe { libc::poll(&mut pipe, 1, 0) };
    ready > 0 && pipe.revents & (libc::POLLHUP | libc::POLLERR) != 0
}

/// Runs `command` to completion and collects its output, registered while it
/// runs. Like `Command::output`, the child's stdin is closed.
pub fn output(command: &mut Command) -> io::Result<Output> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = spawn(command)?;
    child.wait_with_output()
}

/// `output`, ended at `limit`: `None` when the child ran out of time and was
/// killed. A thread waits on the child while this one waits on the thread,
/// so a child that finishes in time costs no extra latency.
pub fn output_within(command: &mut Command, limit: Duration) -> io::Result<Option<Output>> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut running = spawn(command)?;
    let child = running.0.take().expect("child present until waited");
    let pid = child.id();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });
    let result = match receiver.recv_timeout(limit) {
        Ok(output) => output.map(Some),
        Err(_) => {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
            let _ = receiver.recv();
            Ok(None)
        }
    };
    unregister(pid);
    settled(result)
}

/// A waited child's result, handed back only while trace is not ending. A
/// child killed because trace is ending failed on nothing it was given, so
/// the waiting thread parks until the exit instead of letting its caller
/// read, or store, that failure.
fn settled<T>(result: T) -> T {
    while ending() {
        std::thread::park();
    }
    result
}

/// Starts `command` with the pipes the caller configured, registered while it
/// runs. Dropping the handle before it is waited kills the child, so a reader
/// that stops early never leaves it running.
pub fn spawn(command: &mut Command) -> io::Result<Running> {
    let child = command.spawn()?;
    register(child.id());
    Ok(Running(Some(child)))
}

/// A registered child. It derefs to the `std::process::Child`, so its pipes
/// are taken as usual; waiting through it unregisters the child.
pub struct Running(Option<Child>);

impl Running {
    pub fn wait(mut self) -> io::Result<std::process::ExitStatus> {
        let mut child = self.0.take().expect("child present until waited");
        let status = child.wait();
        unregister(child.id());
        settled(status)
    }

    pub fn wait_with_output(mut self) -> io::Result<Output> {
        let child = self.0.take().expect("child present until waited");
        let pid = child.id();
        let output = child.wait_with_output();
        unregister(pid);
        settled(output)
    }
}

impl std::ops::Deref for Running {
    type Target = Child;
    fn deref(&self) -> &Child {
        self.0.as_ref().expect("child present until waited")
    }
}

impl std::ops::DerefMut for Running {
    fn deref_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("child present until waited")
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
            unregister(child.id());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::hung_up;

    fn pipe() -> [libc::c_int; 2] {
        let mut ends = [0; 2];
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        ends
    }

    #[test]
    fn a_pipe_whose_reader_closed_is_hung_up() {
        let [reader, writer] = pipe();
        unsafe { libc::close(reader) };
        assert!(hung_up(writer));
        unsafe { libc::close(writer) };
    }

    #[test]
    fn a_pipe_with_its_reader_open_is_not_hung_up() {
        let [reader, writer] = pipe();
        assert!(!hung_up(writer));
        unsafe { libc::close(reader) };
        unsafe { libc::close(writer) };
    }
}
