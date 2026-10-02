//! 测试夹具：**参数照相机**。
//!
//! shim 的全部正确性都归结为一句话：*参数有没有原样到达目标*。而这件事**只能观察**。
//! 所以契约测试的目标不是 `node.exe`，而是这个把自己的 `argv` 打成一行 JSON 的小程序。
//!
//! 它同时兼任 shim 的"目标"与"被测对象"：测试生成一个指向它的 shim，跑那个 shim，
//! 再解析它打出来的 JSON 与期望逐条比对。
//!
//! 模式：
//!
//! - 无参数：打印 `ARGV <json>` 并退出 0
//! - `--exit <码>`：以给定码退出（支持 `0x` 前缀）
//! - `--crash`：空指针解引用 —— 真的 `0xC0000005`
//! - `--ignore-ctrlc <毫秒>`：自己装一个"已处理"的 Ctrl-C 处理器，睡够再退出 7
//! - `--sleep <毫秒>`：先打一行再睡，然后退出 —— 用来验证转发器**真的在等**
//!   （不等的话通过 shim 调用会秒回）
//! - `--mirror-stdio`：把 stdin 读到 EOF 再原样写到 stdout，同时往 stderr 写一行
//! - `--noop`：立刻退出 0，什么都不打印（量启动成本地板用）

#![cfg(windows)]
#![allow(
    unsafe_code,
    reason = "测试夹具**故意**做两件需要 unsafe 的事：为了造出真的 0xC0000005 去空指针 \
              解引用，以及自己装一个 Ctrl-C 处理器来验证转发器不会先死。这两件事在 \
              production 代码里一件都不会出现。"
)]

use std::io::{Read as _, Write as _};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--noop") => std::process::exit(0),
        Some("--exit") => {
            let spec = args.get(2).map(String::as_str).unwrap_or("0");
            let code = match spec.strip_prefix("0x") {
                Some(hex) => u32::from_str_radix(hex, 16).unwrap_or(0),
                None => spec.parse::<u32>().unwrap_or(0),
            };
            std::process::exit(code as i32);
        }
        Some("--crash") => {
            let _ = std::io::stdout().flush();
            // SAFETY: 故意的。这一行的全部目的就是造出一个真的访问违例，
            // 用来验证 shim 会不会把 0xC0000005 一位不差地透传出去。
            unsafe {
                std::ptr::read_volatile(std::ptr::null::<u32>());
            }
            unreachable!("空指针读居然没炸");
        }
        Some("--ignore-ctrlc") => {
            let millis: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1000);
            // SAFETY: 传入一个签名正确的处理器；它只返回 TRUE。
            unsafe {
                windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1);
            }
            println!("argdump: 已忽略 Ctrl-C，睡 {millis}ms");
            let _ = std::io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(millis));
            println!("argdump: 睡醒了，我还活着");
            let _ = std::io::stdout().flush();
            std::process::exit(7);
        }
        Some("--sleep") => {
            let millis: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(500);
            println!("argdump: 开始睡 {millis}ms");
            let _ = std::io::stdout().flush();
            std::thread::sleep(std::time::Duration::from_millis(millis));
            println!("argdump: 睡完了");
            let _ = std::io::stdout().flush();
            std::process::exit(0);
        }
        Some("--mirror-stdio") => {
            let mut input = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut input);
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(b"STDOUT:");
            let _ = stdout.write_all(&input);
            let _ = stdout.flush();
            let _ = std::io::stderr().write_all(b"STDERR:reached\n");
            std::process::exit(0);
        }
        _ => {}
    }

    let wide: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    println!("ARGV {}", encode(&wide));
    let _ = std::io::stdout().flush();
}

// SAFETY: 签名与 `PHANDLER_ROUTINE` 一致；只返回 TRUE 表示"我处理了，别结束我"。
unsafe extern "system" fn handler(_ctrl_type: u32) -> i32 {
    1
}

/// 极简 JSON 数组编码 —— 不引入依赖，且转义规则够用（测试只比对解析后的值）。
fn encode(values: &[String]) -> String {
    let mut out = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        for c in value.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
    }
    out.push(']');
    out
}
