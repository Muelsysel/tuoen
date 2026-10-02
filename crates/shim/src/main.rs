//! `tuoen-shim` 的入口。
//!
//! 这一票（ticket #2）只让 crate 存在并可编译 —— 骨架必须端到端能构建。
//! 真正的转发逻辑（切 `GetCommandLineW`、退出码透传含 `0xC0000005`、Ctrl-C 透传、
//! job-object 子进程清理）在 ticket #7 落地。

fn main() {
    // 故意不是 `todo!()`：那会 panic 并在 release 下留下一个会崩的二进制。
    // 现在它明确地说"还没实现"，并以非零码退出。
    eprintln!(
        "tuoen-shim: 转发逻辑尚未实现（ticket #7）。\n\
         这个二进制在 v0.1 之前不应被安装到 PATH 上。"
    );
    std::process::exit(1);
}
