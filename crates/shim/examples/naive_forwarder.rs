//! **反例**：一个"看起来对"的转发器，用来演示 Ctrl-C 的坑。
//!
//! 它和真正的 shim 做同样的事 —— 启动目标、等它结束、原样退出 ——
//! **只少一行** `SetConsoleCtrlHandler(NULL, TRUE)`。
//!
//! 没有那一行会发生什么：用户在子进程还活着的时候按 Ctrl-C，控制台把
//! `CTRL_C_EVENT` 发给**控制台上所有进程**（实测：连发信号的进程自己都会收到），
//! 于是转发器先死（`0xC000013A`），而子进程按自己的处理器决定继续活着。
//! 调用者的表现是：**shell 立刻回到提示符、退出码是那个异常码，而程序还在后台跑**。
//! 这就是 `node` REPL 里 Ctrl-C 之后"提示符回来了但 node 没停"的成因。
//!
//! 它**不是组件**，不会被发布，也不该被复制到别处：存在的唯一目的，是让
//! `docs/acceptance/L0-07-shim.md` 里的那句"我们删掉的是这个失败模式"有据可查。
//!
//! 用法：`naive_forwarder.exe <目标.exe> [参数…]`

#![cfg(windows)]
#![allow(
    unsafe_code,
    reason = "反例必须是一段真实可运行的 Win32 调用，包括它**故意省略**的那一行"
)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, GetExitCodeProcess, INFINITE, PROCESS_INFORMATION,
    STARTUPINFOW, WaitForSingleObject,
};

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 把可执行文件与参数拼成一条命令行。
///
/// **参数必须在结尾那个 NUL 之前。** 这个反例的第一版把参数拼在了 `wide()` 自带的
/// NUL **后面**，于是 `CreateProcessW` 只看到一个可执行文件路径、零个参数 ——
/// 同一个错误在 `ctrlc_probe.rs` 里造成了 409 个进程的事故（那个是宿主 fork 自己，
/// 参数丢了就无限递归）。所以这里也只留一个构造点。
fn command_line(exe: &str, args: &[String]) -> Vec<u16> {
    let mut text = format!("\"{exe}\"");
    for arg in args {
        text.push(' ');
        text.push_str(arg);
    }
    wide(&text)
}

fn main() {
    let mut argv = std::env::args_os();
    let _ = argv.next();
    let Some(target) = argv.next() else {
        eprintln!("用法：naive_forwarder.exe <目标.exe> [参数…]");
        std::process::exit(2);
    };
    let target = target.to_string_lossy().into_owned();

    // ── 真正的 shim 在这里有一行 `SetConsoleCtrlHandler(None, 1)`。
    //    本文件**刻意没有** —— 它就是被演示的那个失败模式。
    //
    // unsafe { SetConsoleCtrlHandler(None, 1) };

    let rest: Vec<String> = argv.map(|a| a.to_string_lossy().into_owned()).collect();
    let mut line = command_line(&target, &rest);
    let app = wide(&target);
    // SAFETY: 两个 POD 结构体全零是文档规定的合法初值；`cb` 必须填大小。
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // SAFETY: 同上；成功时由 `CreateProcessW` 填满。
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: 两个缓冲都以 NUL 结尾且活到调用结束；其余指针为 NULL。
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        eprintln!("naive_forwarder: 启动目标失败");
        std::process::exit(9009);
    }
    unsafe {
        WaitForSingleObject(pi.hProcess, INFINITE);
        let mut code = 0u32;
        GetExitCodeProcess(pi.hProcess, &mut code);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
        std::process::exit(code as i32);
    }
}
