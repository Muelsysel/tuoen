//! 一次性诊断：把我写的 junction 与 `mklink /J` 造的真品逐字段对比。
//!
//! 跑法：`cargo run -p tuoen-platform --example junction_probe`

use std::path::Path;

use tuoen_platform::FileSystem;

fn main() {
    let root = std::env::temp_dir().join("tuoen-junction-probe");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let v1 = root.join("versions").join("v1");
    std::fs::create_dir_all(&v1).unwrap();
    std::fs::write(v1.join("marker.txt"), b"one").unwrap();

    let mine = root.join("mine");
    let theirs = root.join("theirs");

    match tuoen_platform::repoint_junction(&mine, &v1) {
        Ok(outcome) => println!("我的实现：{outcome:?}"),
        Err(error) => println!("我的实现失败：{error}"),
    }
    let status = std::process::Command::new("cmd")
        .args([
            "/c",
            "mklink",
            "/J",
            &theirs.display().to_string(),
            &v1.display().to_string(),
        ])
        .output()
        .expect("跑 mklink");
    println!(
        "mklink：out={:?} err={:?}",
        String::from_utf8_lossy(&status.stdout).trim(),
        String::from_utf8_lossy(&status.stderr).trim()
    );

    println!("\n--- 我的实现：谁能访问它 ---");
    probe(&mine);
    println!("\n--- mklink 的真品 ---");
    probe(&theirs);

    println!("\n=== fsutil reparsepoint query：我的 ===");
    fsutil(&mine);
    println!("\n=== fsutil reparsepoint query：mklink 的 ===");
    fsutil(&theirs);
}

fn probe(link: &Path) {
    println!("  is_dir()          = {}", link.is_dir());
    println!("  is_symlink()      = {}", link.is_symlink());
    println!("  read_link()       = {:?}", std::fs::read_link(link));
    println!(
        "  read_dir 数量      = {:?}",
        std::fs::read_dir(link).map(|d| d.count())
    );
    println!(
        "  读 marker.txt      = {:?}",
        std::fs::read_to_string(link.join("marker.txt"))
    );
    println!(
        "  inspect.reparse   = {:?}",
        tuoen_platform::RealFileSystem
            .inspect(link)
            .reparse
            .to_string()
    );
}

fn fsutil(path: &Path) {
    let out = std::process::Command::new("fsutil")
        .args(["reparsepoint", "query", &path.display().to_string()])
        .output()
        .expect("跑 fsutil");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // 只打印头几行（含 Reparse Data Length），不打印整块 hex。
    for line in text.lines().take(12) {
        println!("{line}");
    }
}
