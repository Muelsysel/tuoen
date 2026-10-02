//! Ctrl-C 对照实验的**执行者**：必须在自己的（私有、不可见）控制台里跑。
//!
//! 由 `ctrlc_host.exe` 用 `CREATE_NEW_CONSOLE | SW_HIDE` 起起来，然后：
//!
//! 1. 先跑**反例**（`naive_forwarder.exe`，一个少写一行 `SetConsoleCtrlHandler` 的转发器）
//! 2. 再跑**我们的 shim**
//!
//! 每个都是：起它（目标是 `argdump --ignore-ctrlc 1500`）→ 等 400ms 让它就位 →
//! `GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)`（组 0 = 本控制台上的所有进程）→
//! 用 8 秒上限等它结束 → 记录耗时与退出码。
//!
//! # 为什么必须另起一个二进制，而不是让探针自己重跑自己
//!
//! 第一版是"父模式 fork 自己"，它**递归了**：`wide()` 已经带了 NUL 结尾，
//! 而我把参数拼在了那个 NUL **后面** —— 于是 `CreateProcessW` 看到的命令行只有
//! 可执行文件路径、一个参数都没有，子进程回到父模式又 fork 一次，堆出 409 个进程
//! （每个还带着一个不可见控制台和继承来的 stdout 管道）。
//!
//! 教训不是"下次记得别在 NUL 后面拼字符串"，而是：**一个会启动自己进程的程序，
//! 一旦参数传递出错就会无限放大**。所以现在拆成两个二进制，
//! 执行者**永远不会启动自己**（它只启动被测程序），宿主也**永远不会启动自己**
//! （它只启动执行者，而且启动前断言过路径不同）。

#![cfg(windows)]
#![allow(
    unsafe_code,
    reason = "这个探针的全部内容就是 Win32 控制台 API 的调用；它不进入任何发布产物"
)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Console::{
    CTRL_C_EVENT, GenerateConsoleCtrlEvent, GetConsoleWindow, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, GetExitCodeProcess, PROCESS_INFORMATION,
    STARTUPINFOW, WaitForSingleObject,
};

/// 唯一一处构造命令行的地方。
///
/// **参数必须在结尾的 NUL 之前** —— 第一版的 409 进程事故就是错在这里。
/// 现在整条命令行走同一个函数，调用方拿不到"半成品 + 一个 NUL"这种东西。
fn command_line(exe: &str, args: &[&str]) -> Vec<u16> {
    let mut text = format!("\"{exe}\"");
    for arg in args {
        text.push(' ');
        text.push_str(arg);
    }
    OsStr::new(&text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

struct Report(String);

impl Report {
    fn new() -> Report {
        Report(String::new())
    }
    fn line(&mut self, text: impl AsRef<str>) {
        self.0.push_str(text.as_ref());
        self.0.push_str("\r\n");
    }
}

/// 起一个进程，返回 (pid, 进程句柄)。
fn spawn(app: &str, args: &[&str]) -> Result<(u32, *mut std::ffi::c_void), u32> {
    let mut line = command_line(app, args);
    let app_w: Vec<u16> = OsStr::new(app)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: 两个 POD 结构体全零是文档规定的合法初值；`cb` 必须填大小。
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // SAFETY: 同上；成功时由 `CreateProcessW` 填满。
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: 两个缓冲都以 NUL 结尾且活到调用结束；其余指针为 NULL；
    // 句柄可继承（被测程序要继承这个控制台）。
    let ok = unsafe {
        CreateProcessW(
            app_w.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            std::ptr::null(),
            &raw const si,
            &raw mut pi,
        )
    };
    if ok == 0 {
        // SAFETY: 无参数，读本线程最后一个错误码；必须紧接失败调用。
        return Err(unsafe { GetLastError() });
    }
    // SAFETY: `pi.hThread` 是刚创建、由我们拥有的句柄；主线程句柄不需要。
    unsafe { CloseHandle(pi.hThread) };
    Ok((pi.dwProcessId, pi.hProcess))
}

/// 一次试验，`argv` 是交给被测程序的完整参数表。
///
/// **注意两者的形状不同**：我们的 shim 目标写死在二进制里，所以它只该收到目标程序的
/// 参数；而 `naive_forwarder` 需要把目标路径**作为第一个参数**传进去。
/// 第一版没区分这件事，反例收到的是 `--ignore-ctrlc` 当目标路径 →
/// `CreateProcessW` 失败 → 立刻退 9009，于是"反例"什么都没证明。
fn trial(report: &mut Report, label: &str, under_test: &Path, argv: &[&str], sleep_ms: u64) {
    report.line(format!("── {label}"));
    report.line(format!("   被测：{}", under_test.display()));
    report.line(format!("   交给它的参数：{argv:?}"));

    let started = spawn(&under_test.to_string_lossy(), argv);
    let (pid, handle) = match started {
        Ok(pair) => pair,
        Err(code) => {
            report.line(format!("   !! 起不来，GetLastError={code}"));
            return;
        }
    };
    report.line(format!(
        "   已起 pid={pid}，等 400ms 让它到达「正在睡」的状态"
    ));
    std::thread::sleep(Duration::from_millis(400));

    // SAFETY: 探针自己必须先忽略 Ctrl-C，否则它会被自己发的信号打死。
    unsafe {
        SetConsoleCtrlHandler(None, 1);
    }
    let event_at = Instant::now();
    // SAFETY: 组号 0 = "本控制台里的所有进程"。本进程已忽略 Ctrl-C，
    // 被测进程与它的子进程都在这个**私有**控制台上。
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) };
    // SAFETY: 无参数。
    let err = unsafe { GetLastError() };
    report.line(format!(
        "   GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) = {sent}，GetLastError={err}"
    ));
    if sent == 0 {
        report.line("   !! 信号没送出去，本次试验无效");
        // SAFETY: 唯一的所有者。
        unsafe { CloseHandle(handle) };
        return;
    }

    // 用 8 秒上限等它结束时，一个"转发器死了而子进程还活着"的局面不会挂住探针。
    // SAFETY: 句柄有效。
    let waited = unsafe { WaitForSingleObject(handle, 8000) };
    let elapsed = event_at.elapsed().as_secs_f64() * 1000.0;
    let mut code = 0u32;
    if waited == WAIT_OBJECT_0 {
        // SAFETY: 同上。
        unsafe { GetExitCodeProcess(handle, &raw mut code) };
    }
    // SAFETY: 唯一的所有者。
    unsafe { CloseHandle(handle) };

    if waited == WAIT_TIMEOUT {
        report.line(format!(
            "   8 秒内没有结束（已过 {elapsed:.0}ms）—— 这一条本身就不正常"
        ));
        return;
    }
    report.line(format!(
        "   它在信号之后 {elapsed:.0}ms 结束，退出码 {code:#010X}"
    ));
    report.line(format!("   子进程名义上还要睡 {sleep_ms}ms"));

    let verdict = if code == 0xC000_013A {
        "**转发器先死了**（0xC000013A = STATUS_CONTROL_C_EXIT）。调用者现在拿回提示符了，\
         而子进程还在后台跑 —— 这正是要避免的"
    } else if code == 7 {
        if elapsed >= (sleep_ms as f64 - 400.0) {
            "**转发器活到了最后**，等完了子进程，并把它的退出码（7）交了出来 —— 这就是要的行为"
        } else {
            "退出码对但**提前返回**了 —— 时间对不上，可疑"
        }
    } else {
        "没见过的结果，需要人看一眼"
    };
    report.line(format!("   判决：{verdict}"));
    report.line("");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // **参数不对就退出，绝不"从某个默认值继续"** —— 这个探针的每一次误判
    // 代价都是一串进程。
    if args.len() != 4 {
        eprintln!(
            "用法：ctrlc_runner.exe <报告路径> <被测shim.exe> <反例转发器.exe> <夹具.exe>\n\
             实际收到 {} 个参数。这是给 ctrlc_host.exe 调用的内部程序，不要手跑。",
            args.len()
        );
        std::process::exit(2);
    }
    let (report_path, shim, naive, argdump) = (
        Path::new(&args[0]),
        Path::new(&args[1]),
        Path::new(&args[2]),
        Path::new(&args[3]),
    );

    let mut report = Report::new();
    report.line("=== Ctrl-C 对照实验（在私有控制台里跑）===");
    report.line("");
    // SAFETY: 无参数。
    let console = unsafe { GetConsoleWindow() };
    report.line(format!(
        "私有控制台窗口句柄：{console:p}（0 表示没有控制台，实验无效）"
    ));
    report.line("");
    if console.is_null() {
        report.line("!! 没有控制台 —— 这个实验什么都没证明");
        let _ = std::fs::write(report_path, report.0.as_bytes());
        std::process::exit(1);
    }

    // 反例需要显式拿到目标；我们的 shim 的目标烘在二进制里。
    let sleep = "1500".to_owned();
    let argdump_text = argdump.to_string_lossy().into_owned();
    trial(
        &mut report,
        "反例：少写一行的转发器",
        naive,
        &[&argdump_text, "--ignore-ctrlc", &sleep],
        1500,
    );
    std::thread::sleep(Duration::from_millis(400));
    trial(
        &mut report,
        "我们的 shim",
        shim,
        &["--ignore-ctrlc", &sleep],
        1500,
    );

    if let Err(e) = std::fs::write(report_path, report.0.as_bytes()) {
        eprintln!("写报告失败 {}：{e}", report_path.display());
        std::process::exit(1);
    }
    std::process::exit(0);
}
