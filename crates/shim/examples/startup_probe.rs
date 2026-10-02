//! 实测 shim 的启动开销 —— 这是本票的一条**验收标准**，不能凭感觉。
//!
//! 做法：同一个目标程序跑两遍，一遍直接跑、一遍经过 shim，取多轮的中位数相减。
//! 两遍都走 `std::process::Command`，所以 `Command` 自己的开销在两边是同一份，
//! 相减之后剩下的就是**转发器的增量**。
//!
//! 同时量一个地板：`argdump --noop`（一个什么都不做、只启动就退出的 Rust 进程）。
//! 地板决定了"增量"的理论下限 —— 转发器至少要多起一个进程。
//!
//! 用法：
//!
//! ```text
//! cargo build --release -p tuoen-shim
//! cargo run --release -p tuoen-shim --example startup_probe
//! ```
//!
//! 若要顺便量一个真实工具（本机验收用的就是它）：
//!
//! ```text
//! TUOEN_SHIM_NODE=C:\path\to\node.exe cargo run --release --example startup_probe
//! ```
//! 不设这个变量时会在 `PATH` 上找 `node.exe`；找不到就跳过那一节。

#![cfg(windows)]
#![allow(
    clippy::cast_precision_loss,
    reason = "统计输出的毫秒值只有展示用途；用 f64 表示耗时是标准做法"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use tuoen_platform::test_support::TempDir;
use tuoen_shim::{ShimSpec, write_shim};

/// 每个用例跑多少轮（前 [`WARMUP`] 轮不计）。
const ROUNDS: usize = 40;
const WARMUP: usize = 6;

#[derive(Debug, Clone, Copy)]
struct Stats {
    min: f64,
    p50: f64,
    p90: f64,
    max: f64,
}

impl Stats {
    fn of(mut samples: Vec<f64>) -> Stats {
        samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let at = |q: f64| {
            let index = ((samples.len() as f64 - 1.0) * q).round() as usize;
            samples[index.min(samples.len() - 1)]
        };
        Stats {
            min: samples[0],
            p50: at(0.5),
            p90: at(0.9),
            max: samples[samples.len() - 1],
        }
    }
}

fn bench(program: &Path, args: &[&str]) -> Stats {
    let mut samples = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        let started = Instant::now();
        let status = Command::new(program)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap_or_else(|e| panic!("启动 {} 失败：{e}", program.display()));
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        assert!(status.success() || status.code().is_some(), "没正常结束");
        if round >= WARMUP {
            samples.push(elapsed);
        }
    }
    Stats::of(samples)
}

fn line(label: &str, stats: Stats) {
    println!(
        "  {label:<44} 最小 {:6.1}  中位 {:6.1}  p90 {:6.1}  最大 {:6.1}  (ms)",
        stats.min, stats.p50, stats.p90, stats.max
    );
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
    if !template.is_file() {
        eprintln!(
            "找不到模板 {}。先跑 `cargo build --release -p tuoen-shim`，\
             并且**用 --release 跑这个例子**（否则会去 debug 目录里找却找不到）。",
            template.display()
        );
        std::process::exit(2);
    }
    let fixture = here.join("argdump.exe");
    if !fixture.is_file() {
        eprintln!(
            "找不到夹具 {}；用 `cargo build --release --examples -p tuoen-shim` 构建。",
            fixture.display()
        );
        std::process::exit(2);
    }

    println!("=== shim 启动开销实测 ===");
    println!("模板：{}", template.display());
    println!(
        "模板大小：{} 字节",
        std::fs::metadata(&template).map(|m| m.len()).unwrap_or(0)
    );
    println!("轮数：每例 {ROUNDS} 轮，前 {WARMUP} 轮预热不计；单位毫秒");
    println!("release profile：lto=true codegen-units=1 strip=true panic=abort（根 Cargo.toml）");
    println!();

    let dir = TempDir::new("shim-startup");
    let shim = dir.join("argdump-shim.exe");
    let outcome = write_shim(
        &template,
        &shim,
        &ShimSpec {
            name: "argdump-shim".to_owned(),
            target: fixture.clone(),
            prefix_args: Vec::new(),
        },
    )
    .expect("生成 shim");
    println!(
        "生成的 shim：{} 字节（改写槽位 {} 处）",
        outcome.bytes, outcome.slot_count
    );
    println!();

    println!("--- 一、纯转发增量（目标是一个什么都不做的小程序）---");
    line("argdump --noop 直接跑", bench(&fixture, &["--noop"]));
    line("argdump --noop 经过 shim", bench(&shim, &["--noop"]));
    println!();

    // ── 器材自检：这个探针自己必须先证明它在量的是对的东西 ──────────────
    //
    // 第一版跑出来一个**不可能**的数字：经过 shim 的 `node --version` 比直接跑还快
    // 17ms。一个转发器不可能让它转发的东西变快，所以那一定是"shim 根本没在转发"。
    // 从此这一段先验证再计时 —— 一条不能失败的测量不是测量。
    println!("--- 一·自检：转发器真的在转发、真的在等吗 ---");
    let slept = Instant::now();
    let status = Command::new(&shim)
        .args(["--sleep", "400"])
        .stdout(Stdio::null())
        .status()
        .expect("启动");
    let waited = slept.elapsed().as_secs_f64() * 1000.0;
    assert!(
        waited >= 400.0,
        "shim 提前返回了（{waited:.0}ms < 400ms）—— 它没有等子进程。\
         这意味着它也没法把退出码交出来"
    );
    assert!(status.success(), "自检调用应当成功");
    println!("  shim → argdump --sleep 400 用了 {waited:.0}ms：**确实在等**");

    let direct = Command::new(&fixture)
        .args(["-a", "b c"])
        .output()
        .expect("启动");
    let through = Command::new(&shim)
        .args(["-a", "b c"])
        .output()
        .expect("启动");
    assert_eq!(
        direct.stdout, through.stdout,
        "经过 shim 的 stdout 与直接跑不一致 —— 转发是坏的"
    );
    assert_eq!(direct.status.code(), through.status.code());
    println!(
        "  shim 转发的 stdout 与直接跑**逐字节一致**（{}）",
        String::from_utf8_lossy(&through.stdout).trim()
    );
    println!();

    println!("--- 二、转发器要真的转发参数 ---");
    line("argdump -a -b 直接跑", bench(&fixture, &["-a", "-b"]));
    line("argdump -a -b 经过 shim", bench(&shim, &["-a", "-b"]));
    println!();

    let node = std::env::var_os("TUOEN_SHIM_NODE")
        .map(PathBuf::from)
        .or_else(|| find_on_path("node.exe"));
    if let Some(node) = node.filter(|p| p.is_file()) {
        println!("--- 三、真实工具：{} ---", node.display());
        let node_shim = dir.join("node.exe");
        write_shim(
            &template,
            &node_shim,
            &ShimSpec {
                name: "node".to_owned(),
                target: node.clone(),
                prefix_args: Vec::new(),
            },
        )
        .expect("生成 node shim");
        // 先证明它真的在转发 node，再谈快慢。
        let direct_out = Command::new(&node)
            .arg("--version")
            .output()
            .expect("直接跑");
        let shim_out = Command::new(&node_shim)
            .arg("--version")
            .output()
            .expect("经过 shim");
        if shim_out.stdout != direct_out.stdout || !shim_out.status.success() {
            eprintln!(
                "!! node shim 没有正确转发：\n   直接跑：status={:?} stdout={:?}\n   经 shim：status={:?} stdout={:?}\n   经 shim stderr={:?}",
                direct_out.status.code(),
                String::from_utf8_lossy(&direct_out.stdout),
                shim_out.status.code(),
                String::from_utf8_lossy(&shim_out.stdout),
                String::from_utf8_lossy(&shim_out.stderr),
            );
            std::process::exit(3);
        }
        println!(
            "  自检：两条路径都给出 {:?}",
            String::from_utf8_lossy(&shim_out.stdout).trim()
        );
        let direct = bench(&node, &["--version"]);
        let through = bench(&node_shim, &["--version"]);
        line("node.exe --version 直接跑", direct);
        line("node.exe --version 经过 shim", through);
        println!(
            "  → 增量：中位 {:.1}ms（{:.1} → {:.1}），即 {:.0}%",
            through.p50 - direct.p50,
            direct.p50,
            through.p50,
            (through.p50 / direct.p50 - 1.0) * 100.0
        );
        println!();
        println!("（Scoop 的原生 shim 报 33–36ms；那是**总量**口径，不是增量。）");
    } else {
        println!("--- 三、跳过：PATH 上没有 node.exe，也没设 TUOEN_SHIM_NODE ---");
    }

    println!();
    println!("=== 结束 ===");
}

/// 在 `PATH` 上找一个可执行文件。找不到返回 `None`（**不报错**：这一节是可选的）。
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}
