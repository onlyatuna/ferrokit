// SPDX-License-Identifier: LGPL-3.0-only
//! Cost and latency of the pipe reader threads of `Process`.
//!
//! Run in release mode: `cargo run --release -p ferrokit-core --example process_reader_cost`.
//! The example starts itself as the child process (`child <mode> ...`). Three things are measured:
//!
//! * `idle CPU`: the CPU time this process spends, and on Unix the voluntary context switches
//!   (thread wake-ups) it makes, while `IDLE_PROCESSES` children run silently and so every reader
//!   thread is waiting for data that never comes. The same period with no `Process` is the floor.
//! * `data latency`: from the parent writing a line to a child that echoes it until the echoed
//!   bytes can be read with `read_all_stdout`, after the readers have been idle for a while.
//! * `stop latency`: how much longer than `READER_DRAIN` (1 s) `wait_for_finished` takes when the
//!   child exited but a grandchild still holds its pipes open, so the readers have to be stopped.
//!   It includes the child's exit and the 5 ms poll of `wait_for_finished`, which a change of the
//!   reader threads does not touch.
use ferrokit_core::io::process::Process;
use std::io::{BufRead, Write};
use std::process::Command;
use std::time::{Duration, Instant};

const IDLE_PROCESSES: usize = 16;
const IDLE_WINDOW: Duration = Duration::from_secs(5);
const LATENCY_SAMPLES: usize = 40;
const STOP_SAMPLES: usize = 10;

/// What the child does. `idle MS` sleeps; `echo` answers each stdin line with `x\n` until stdin
/// closes; `hold MS` starts an `idle MS` grandchild that inherits the pipes, says `spawned` and
/// exits at once.
fn child(mode: &str, arg: Option<&str>) {
    match mode {
        "idle" => std::thread::sleep(Duration::from_millis(arg.unwrap().parse().unwrap())),
        "echo" => {
            let stdin = std::io::stdin();
            let mut out = std::io::stdout();
            for _ in stdin.lock().lines() {
                out.write_all(b"x\n").unwrap();
                out.flush().unwrap();
            }
        }
        "hold" => {
            let exe = std::env::current_exe().unwrap();
            // The grandchild is left running on purpose: it holds the pipes after this child is gone.
            #[allow(clippy::zombie_processes)]
            Command::new(exe)
                .args(["child", "idle", arg.unwrap()])
                .spawn()
                .unwrap();
            let mut out = std::io::stdout();
            out.write_all(b"spawned\n").unwrap();
            out.flush().unwrap();
        }
        _ => panic!("unknown child mode {mode}"),
    }
}

fn spawn(mode: &str, arg: Option<&str>) -> Process {
    let mut p = Process::new();
    p.set_program(
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    p.add_argument("child");
    p.add_argument(mode);
    if let Some(arg) = arg {
        p.add_argument(arg);
    }
    p.start().unwrap();
    p
}

/// CPU time of this process in milliseconds and its voluntary context switches (Unix only).
#[cfg(unix)]
fn cpu() -> (f64, Option<u64>) {
    // SAFETY: `mem::zeroed::<libc::rusage>()` is valid because `rusage` is a plain C struct of
    // integers and `timeval`s, for which all-zero is a valid value; `&mut usage` is a live local
    // valid for the write of one `rusage`, which is what `getrusage` fills in. `RUSAGE_SELF` is a
    // valid `who`; the return code is ignored, so on failure the zeroed values are used.
    let usage = unsafe {
        let mut usage = std::mem::zeroed::<libc::rusage>();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    let ms = |t: libc::timeval| t.tv_sec as f64 * 1e3 + t.tv_usec as f64 / 1e3;
    (
        ms(usage.ru_utime) + ms(usage.ru_stime),
        Some(usage.ru_nvcsw as u64),
    )
}

#[cfg(windows)]
fn cpu() -> (f64, Option<u64>) {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: `GetCurrentProcess` returns the pseudo handle of the calling process, which needs no
    // closing and is valid for `GetProcessTimes` (Win32 docs); the four `FILETIME` out-parameters
    // are live locals, each valid for the write of one `FILETIME`. The return value is ignored,
    // so a failure leaves the zeroed values in place.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    let ticks =
        |t: FILETIME| ((t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64) as f64 / 1e4;
    (ticks(kernel) + ticks(user), None)
}

fn measure_window(label: &str) {
    let (cpu0, sw0) = cpu();
    std::thread::sleep(IDLE_WINDOW);
    let (cpu1, sw1) = cpu();
    let switches = match (sw0, sw1) {
        (Some(a), Some(b)) => format!(
            "{:>8} wake-ups/s",
            (b - a) as f64 / IDLE_WINDOW.as_secs_f64()
        ),
        _ => "(no wake-up counter)".to_string(),
    };
    println!(
        "  {label:<34} {:>8.1} ms CPU / {} s   {switches}",
        cpu1 - cpu0,
        IDLE_WINDOW.as_secs()
    );
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn summary(label: &str, mut samples: Vec<f64>) {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "  {label:<34} median {:>7.2} ms   p90 {:>7.2} ms   min {:>7.2} ms   max {:>7.2} ms",
        percentile(&samples, 0.5),
        percentile(&samples, 0.9),
        samples[0],
        samples[samples.len() - 1],
    );
}

fn idle_cpu() {
    println!(
        "idle CPU ({IDLE_PROCESSES} processes = {} reader threads):",
        IDLE_PROCESSES * 2
    );
    measure_window("no Process");
    let idle_ms = (IDLE_WINDOW.as_millis() * 3).to_string();
    let mut procs: Vec<Process> = (0..IDLE_PROCESSES)
        .map(|_| spawn("idle", Some(&idle_ms)))
        .collect();
    // Let the children start and the readers settle.
    std::thread::sleep(Duration::from_millis(1500));
    measure_window("readers waiting");
    for p in &mut procs {
        p.kill().unwrap();
    }
}

fn data_latency() {
    println!("data latency (reader idle before each line):");
    let mut p = spawn("echo", None);
    let mut samples = Vec::new();
    for i in 0..LATENCY_SAMPLES {
        // Different idle gaps, so the line does not always meet the same phase of a poll period.
        std::thread::sleep(Duration::from_millis(300 + (i as u64 * 7) % 23));
        let sent = Instant::now();
        p.write_stdin(b"go\n").unwrap();
        let mut got = Vec::new();
        while got.is_empty() {
            got = p.read_all_stdout();
            if got.is_empty() {
                std::thread::yield_now();
            }
            assert!(sent.elapsed() < Duration::from_secs(10), "no echo");
        }
        samples.push(sent.elapsed().as_secs_f64() * 1e3);
    }
    p.close_stdin();
    p.wait_for_finished(Some(Duration::from_secs(10)));
    summary("write_stdin -> read_all_stdout", samples);
}

fn stop_latency() {
    println!("stop latency (grandchild holds the pipes; READER_DRAIN = 1000 ms):");
    let mut samples = Vec::new();
    for _ in 0..STOP_SAMPLES {
        let mut p = spawn("hold", Some("2500"));
        // The child has started its grandchild once it says so; it exits right after.
        while p.read_all_stdout().is_empty() {
            std::thread::yield_now();
        }
        let started = Instant::now();
        assert!(p.wait_for_finished(Some(Duration::from_secs(10))));
        samples.push(started.elapsed().as_secs_f64() * 1e3 - 1000.0);
    }
    summary("wait_for_finished - 1000 ms", samples);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("child") {
        child(&args[2], args.get(3).map(String::as_str));
        return;
    }
    idle_cpu();
    data_latency();
    stop_latency();
}
