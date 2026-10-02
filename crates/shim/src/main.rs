//! `tuoen-shim` 的入口 —— 转发器本体。
//!
//! **这个文件在关键路径上**：每次敲 `node` / `npm` / `python` 都会跑一遍。
//! 所以它刻意做三件事以外什么都不做，而且**一个文件都不读**：
//!
//! 1. 解出烘在二进制里的前缀（一次内存读取）
//! 2. 忽略 Ctrl-C（这样父 shell 不会在子进程还活着的时候拿回提示符）
//! 3. `CreateProcessW` 转发 + 等它结束 + 原样退出
//!
//! 引号化**不在这里**：前缀在生成时就引号化好了，运行时只做字符串拼接。
//! 于是"参数被重新引号化"这个 bug 面在整个设计里不存在（决策 52）。
//!
//! 不知道的事都在 `docs/acceptance/L0-07-shim.md` 里实测过，别再靠推理：
//! `.cmd` 为什么被拒（§2）、退出码含 `0xC0000005` 怎么透传（§4）、
//! 参数原文怎么到达（§3）、Ctrl-C 的两种命运（§5）。

#![cfg(windows)]
#![allow(
    unsafe_code,
    reason = "Win32 FFI 的调用点集中在本文件；每个 unsafe 块都带 SAFETY 注释。\
              本 crate 的业务逻辑（槽位、引号化、命令行切片）全在 lib.rs 里，一行 unsafe 都没有"
)]

use std::path::PathBuf;

use tuoen_shim::{
    EXIT_LAUNCH_FAILED, SLOT_BYTES, TEMPLATE_MARKER, baked_slot, decode_slot, split_argv0_wide,
};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::Environment::GetCommandLineW;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, GetExitCodeProcess, INFINITE, PROCESS_INFORMATION, STARTUPINFOW,
    WaitForSingleObject,
};

/// 烘进去的目标与前缀参数。
///
/// **它就在这个二进制里**，所以运行时读取它不需要打开任何文件。
/// 内容是 [`baked_slot()`]，也就是一个"还没烘过"的槽位；`tuoen shim add`
/// 复制这个二进制并就地改写这 [`SLOT_BYTES`] 个字节。
///
/// `#[used]` 是防将来的重构：只要有人把 `baked_bytes()` 改没了，
/// 这个 static 就会被优化掉，而**症状是生成出来的 shim 说自己是模板** ——
/// 那种 bug 很难查，所以这里明确钉住。
///
/// 注意 `#[used]` **只保证字节被写进目标文件，不保证有人真的去读它** ——
/// 那件事由 [`baked_bytes`] 负责，理由见那里。
#[used]
static SLOT: [u8; SLOT_BYTES] = baked_slot();

/// 把烘进二进制的槽位读出来。
///
/// # 为什么必须是 `read_volatile`（这是实测换来的，不是防御性编程）
///
/// 第一版写的是 `decode_slot(&SLOT)`。**debug 全绿，release（LTO）全错**：
/// `SLOT` 是一个只读 `static`，LLVM 有权把"读一个从不被写入的 static"
/// 连同 `decode_slot` 一起**常量折叠**掉 —— 于是运行时用的是**编译时**的值
/// （一个未烘过的模板），文件里的字节改得再准也没用。
///
/// 这个 bug 的症状极具误导性，值得逐条记下来：
///
/// - 生成器的文件自检**全部通过**（它读的是文件，文件确实是对的）；
/// - `pristine_slot_offsets` 说"一处不剩"，PE 节表说槽位稳稳落在 `.rdata`
///   的 raw data 窗口里；
/// - 生成的 shim 却打印"这是一个**模板**"。
///
/// 抓住它的是启动开销探针里的**器材自检**：它发现"经过 shim 的 `node --version`
/// 比直接跑还快 17ms"，而那是不可能的（一个不能失败的测量不是测量）。
///
/// `read_volatile` 是文档给了硬保证的那件工具："will not be elided or reordered"。
/// 代价是一次 2076 字节的拷贝（纳秒级，摊在 5ms 的进程启动里看不见）。
fn baked_bytes() -> [u8; SLOT_BYTES] {
    // SAFETY: `SLOT` 是一个存在的 `[u8; SLOT_BYTES]` 静态数组，对齐为 1，
    // 因此这个指针对 `SLOT_BYTES` 字节的读是有效的；`read_volatile` 只做一次读，
    // 返回的数组是 `Copy` 的，不涉及别名或生命周期问题。
    unsafe { std::ptr::read_volatile(&raw const SLOT) }
}

fn main() {
    let prefix = match decode_slot(&baked_bytes()) {
        Some(prefix) => prefix,
        None => die_not_baked(),
    };

    // ── 第四道保险丝：拒绝把自己当目标 ────────────────────────────────
    //
    // 生成时已经有一道同名守卫（`ShimError::DestinationIsTarget`），这里是
    // **第二道**，防的是"字节被手改过"或"由有 bug 的旧版本生成的"。
    //
    // 为什么要为此付出关键路径上的成本：这个失效模式的代价不对称 ——
    // 一个自指的 shim 运行一次就会无限自我启动（本机实测堆到 20580 个进程，
    // 内存吃干、整机失去响应）。多花几十微秒换掉这种可能性是划算的。
    //
    // 解析失败（`prefix_target` 返回 `None`）时**一律放行**：这道保险丝
    // 只该拦住能确定的自指，绝不该变成"正常程序启动不了"的原因。
    if let Some(target) = tuoen_shim::prefix_target(&prefix)
        && let Ok(me) = std::env::current_exe()
        && same_program(target, &me.to_string_lossy())
    {
        die_self_reference(target);
    }

    // SAFETY: 参数是 (NULL, TRUE)。`NULL` 处理器 + `add = TRUE` 是文档定义的
    // "让本进程忽略 Ctrl+C"，它只改本进程的标志位，不碰任何指针。
    // 返回值我们**故意不检查**：即使这行失败（极端情况下没有控制台），
    // 正确的做法也是继续转发，而不是拒绝启动用户的程序。
    unsafe {
        SetConsoleCtrlHandler(None, 1);
    }

    let command_line = raw_command_line();
    let rest = split_argv0_wide(&command_line);

    // 前缀 + 参数**原文**。这是本 crate 的核心不变量：
    // `rest` 是从 `GetCommandLineW` 上切下来的切片，一个字符都没被动过。
    let mut line: Vec<u16> = prefix.encode_utf16().collect();
    line.extend_from_slice(rest);
    line.push(0);

    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // SAFETY: `line` 是我们自己的、以 NUL 结尾的可变缓冲区（`CreateProcessW` 允许
    // 就地改写它）；其余指针要么是 NULL，要么指向上面两个已初始化的结构体。
    // `bInheritHandles = TRUE` 且不设 `STARTF_USESTDHANDLES`，于是子进程原样继承
    // 我们的标准句柄 —— 管道与重定向因此照常工作（验收 §6 有断言）。
    // `lpApplicationName = NULL`：程序名取命令行的第一个 token，也就是前缀里的
    // `"<目标>"`，所以路径解析与 `argv[0]` 天然一致。
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            0,
            std::ptr::null(),
            std::ptr::null(),
            &raw const startup,
            &raw mut info,
        )
    };

    if created == 0 {
        // SAFETY: `GetLastError` 无参数，读的是本线程的最后一个错误码。
        // 必须**紧接**在失败调用之后读，中间不能插入任何可能设置错误码的调用。
        let code = unsafe { GetLastError() };
        die_launch_failed(&prefix, code);
    }

    // SAFETY: `info` 是 `CreateProcessW` 成功时填好的；两个句柄都有效且由我们拥有。
    let waited = unsafe { WaitForSingleObject(info.hProcess, INFINITE) };
    let mut exit_code = 0u32;
    // SAFETY: 同上，句柄有效，`exit_code` 是本地变量。
    let got_code = unsafe { GetExitCodeProcess(info.hProcess, &raw mut exit_code) };
    // SAFETY: 两个句柄都还没关，各关一次。
    unsafe {
        CloseHandle(info.hProcess);
        CloseHandle(info.hThread);
    }

    if waited != WAIT_OBJECT_0 || got_code == 0 {
        // 等不下去或拿不到退出码 —— 不能假装成功。
        eprintln!(
            "tuoen-shim: 目标程序已启动，但等它结束/取退出码失败（wait={waited}, \
             GetExitCodeProcess={got_code}）。\n  命令行：{}",
            String::from_utf16_lossy(&line[..line.len() - 1])
        );
        std::process::exit(EXIT_LAUNCH_FAILED);
    }

    // **原样退出**：`exit_code` 是 DWORD，`as i32` 保留全部 32 位，
    // 所以 `0xC0000005`（访问违例）这类异常码也能一位不差地透传给调用者。
    std::process::exit(exit_code as i32);
}

/// 本进程的原始命令行，**按 UTF-16 原样取出**。
///
/// 不在这一步转成 `String`：原始命令行里理论上可以有落单的代理项，
/// 那样转换会丢字节，而我们要的是**逐字节转发**。
fn raw_command_line() -> Vec<u16> {
    // SAFETY: `GetCommandLineW` 返回指向进程环境块里那条只读字符串的指针，
    // 生命周期是整个进程，永不返回 NULL。
    let pointer = unsafe { GetCommandLineW() };
    if pointer.is_null() {
        return Vec::new();
    }
    // SAFETY: 按 Windows 的约定，它指以一个 NUL 结尾的 UTF-16 串。
    // 我们在遇到第一个 NUL 时停下，绝不越过它。
    let mut len = 0isize;
    unsafe {
        while *pointer.offset(len) != 0 {
            len += 1;
        }
        std::slice::from_raw_parts(pointer, len as usize).to_vec()
    }
}

/// 两个路径是不是同一个程序。**故意宽松**：大小写不敏感、`/` 与 `\` 等价 ——
/// 这道保险丝宁可拦不住，也不能误伤。
fn same_program(a: &str, b: &str) -> bool {
    let fold = |s: &str| s.replace('/', "\\").to_lowercase();
    fold(a) == fold(b)
}

fn die_self_reference(target: &str) -> ! {
    eprintln!(
        "tuoen-shim: 拒绝启动 —— **这个 shim 的目标是它自己**。\n  \
         目标：{target}\n\
         那是无限自我启动，一次调用就会把整台机器的进程数吃干。\n\
         正常生成的 shim 不会长成这样；这个文件要么是被手改过，要么来自一个有 bug 的版本。\n\
         请把它删掉并重新生成：`tuoen shim add <工具>`。"
    );
    std::process::exit(EXIT_LAUNCH_FAILED);
}

fn die_not_baked() -> ! {
    let me = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tuoen-shim.exe"));
    eprintln!(
        "tuoen-shim: 这是一个**模板**（{TEMPLATE_MARKER}），不是一个能用的 shim。\n\
         它的槽位里还没有目标路径，所以它不知道该转发到哪里。\n\
         它应该由 `tuoen shim add` 复制到 shim 目录并就地烘入目标路径之后才被使用。\n\
         现在这个文件：{}\n\
         如果你是从源码目录直接跑它的，那是预期的 —— 模板本身不可运行。",
        me.display()
    );
    std::process::exit(EXIT_LAUNCH_FAILED);
}

fn die_launch_failed(prefix: &str, code: u32) -> ! {
    eprintln!(
        "tuoen-shim: 无法启动目标程序。\n  \
         目标与参数（烘进 shim 的前缀）：{prefix}\n  \
         Win32 错误：{code}（{}）\n\
         这个 shim 是由 tuoen 生成的。目标不存在通常意味着那个版本已经被卸载，\
         而当前版本的联接还指着它 —— 用 `tuoen list` 看一眼，再用 `tuoen use` 切到还在的版本。",
        describe_win32(code)
    );
    std::process::exit(EXIT_LAUNCH_FAILED);
}

/// 给最常见的几个错误码一句中文。**不匹配错误文本**，只按码查表。
fn describe_win32(code: u32) -> &'static str {
    match code {
        2 => "找不到指定的文件",
        3 => "找不到指定的路径",
        5 => "拒绝访问",
        32 => "文件正被另一个进程使用",
        87 => "参数不正确",
        193 => "不是有效的 Win32 应用程序",
        267 => "目录名无效",
        740 => "请求的操作需要提升",
        _ => "详见 Win32 错误码表",
    }
}
