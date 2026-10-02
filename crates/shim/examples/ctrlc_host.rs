//! Ctrl-C 对照实验的**宿主**：造一个不可见的私有控制台，把 `ctrlc_runner.exe`
//! 放进去跑，然后把它写出来的报告读回来打印。
//!
//! # 为什么要在意 Ctrl-C
//!
//! 用户在 `node` REPL 里按 Ctrl-C 时，控制台把 `CTRL_C_EVENT` 发给**控制台上的
//! 所有进程**（实测：连发信号的进程自己都会收到）。一个不处理这件事的转发器会
//! 先死（`0xC000013A`），而子进程按自己的处理继续活着 —— 调用者看到的是
//! **提示符回来了、退出码是异常码，而程序还在后台跑**。
//! 我们的 shim 在 `main` 里有一行 `SetConsoleCtrlHandler(NULL, TRUE)` 就是为它。
//!
//! 这个实验同时跑**反例**（`naive_forwarder.exe`，只少那一行）与真 shim，
//! 只证明前者会死、后者会活，才算把这条验收做实。
//!
//! # 为什么宿主与执行者是两个二进制
//!
//! 第一版是"宿主 fork 自己"，它递归了（`wide()` 自带 NUL，参数被拼到了 NUL 后面，
//! 于是子进程收到零个参数、回到宿主模式又 fork 一次）—— 堆出 **409 个进程**。
//! 现在的结构里**没有任何一个程序会启动自己**：宿主动执行者，执行者动被测程序。
//! 宿主在动手之前还会断言"执行者不是我"。
//!
//! 用法：
//!
//! ```text
//! cargo build --release --examples -p tuoen-shim
//! target\release\examples\ctrlc_host.exe
//! ```

#![cfg(windows)]
#![allow(
    unsafe_code,
    reason = "这个探针的全部内容就是 Win32 控制台 API 的调用；它不进入任何发布产物"
)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, GetExitCodeProcess,
    PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
};

use tuoen_platform::test_support::TempDir;
use tuoen_shim::{ShimSpec, write_shim};

/// `STARTUPINFO.dwFlags` 的一位，来自 `winbase.h`。
///
/// **故意写成本地常量**：为了两个常量去开 `Win32_UI_WindowsAndMessaging` 这个 feature
/// 不划算，而这两个值从 Windows 95 起就没变过。
const STARTF_USESHOWWINDOW: u32 = 0x0000_0001;
/// `ShowWindow` 的取值，来自 `winuser.h`。
const SW_HIDE: u16 = 0;

/// 唯一一处构造命令行的地方 —— 参数必须在结尾的 NUL **之前**。
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

fn main() {
    let here = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("examples 目录")
        .to_path_buf();
    let template = here
        .parent()
        .expect("target/<profile>")
        .join("tuoen-shim.exe");
    let runner = here.join("ctrlc_runner.exe");
    let naive = here.join("naive_forwarder.exe");
    let argdump = here.join("argdump.exe");
    for (label, path) in [
        ("模板", &template),
        ("执行者", &runner),
        ("反例", &naive),
        ("夹具", &argdump),
    ] {
        if !path.is_file() {
            eprintln!(
                "找不到{label} {}。先跑 `cargo build --release --examples -p tuoen-shim`，\
                 并且**用 --release 跑这个探针**。",
                path.display()
            );
            std::process::exit(2);
        }
    }

    // **结构性护栏**：宿主绝不启动自己。第一版就是栽在这里（递归出 409 个进程）。
    // 万一将来有人把 `runner` 改成 `current_exe()`，这两行会立刻拦下来。
    let me = std::env::current_exe().expect("current_exe");
    assert!(
        runner.canonicalize().ok() != me.canonicalize().ok(),
        "执行者不能是宿主自己 —— 那会递归"
    );

    let dir = TempDir::new("shim-ctrlc");
    let shim = dir.join("probe-shim.exe");
    write_shim(
        &template,
        &shim,
        &ShimSpec {
            name: "probe-shim".to_owned(),
            target: argdump.clone(),
            prefix_args: Vec::new(),
        },
    )
    .expect("生成被测 shim");
    let report_path = dir.join("report.txt");

    // `CREATE_NEW_CONSOLE` 给执行者一个**自己的**控制台（组 0 里只有它和它之后起的
    // 进程），`SW_HIDE` 让那个窗口不闪出来。宿主自己的 std 句柄完全不受影响 ——
    // 这正是把实验放进另一个进程的理由：`AllocConsole` 会把标准句柄换成新控制台的。
    //
    // **没有** `CREATE_NEW_PROCESS_GROUP`：实测它会**关掉**整个新进程组的 Ctrl-C，
    // 而那正是本实验要观测的东西。
    let mut line = command_line(
        &runner.to_string_lossy(),
        &[
            &report_path.to_string_lossy(),
            &shim.to_string_lossy(),
            &naive.to_string_lossy(),
            &argdump.to_string_lossy(),
        ],
    );
    let app = OsStr::new(&runner)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();
    // SAFETY: 两个 POD 结构体全零是文档规定的合法初值。
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    si.dwFlags = STARTF_USESHOWWINDOW;
    si.wShowWindow = SW_HIDE;
    // SAFETY: 同上。
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: 两个缓冲都以 NUL 结尾且活到调用结束；其余指针为 NULL。
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NEW_CONSOLE,
            std::ptr::null(),
            std::ptr::null(),
            &raw const si,
            &raw mut pi,
        )
    };
    if ok == 0 {
        // SAFETY: 无参数。
        eprintln!("起不了执行者，GetLastError={}", unsafe {
            GetLastError()
        });
        std::process::exit(2);
    }

    // 上限 60 秒：执行者自己两次试验最多各约 9 秒。
    // SAFETY: 句柄由我们拥有。
    let waited = unsafe { WaitForSingleObject(pi.hProcess, 60_000) };
    let mut code = 0u32;
    // SAFETY: 同上。
    unsafe {
        GetExitCodeProcess(pi.hProcess, &raw mut code);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }
    assert_eq!(
        waited, WAIT_OBJECT_0,
        "执行者 60 秒没结束 —— 先看看是不是又有残留进程"
    );

    println!("=== shim 的 Ctrl-C 对照实验 ===");
    println!("被测 shim：{}", shim.display());
    println!();
    match std::fs::read(&report_path) {
        Ok(bytes) => print!("{}", String::from_utf8_lossy(&bytes)),
        Err(e) => {
            println!("读不到报告 {}：{e}", report_path.display());
            std::process::exit(1);
        }
    }
    println!("执行者退出码：{code}");
    std::thread::sleep(Duration::from_millis(200));
    std::process::exit(i32::from(code != 0));
}
