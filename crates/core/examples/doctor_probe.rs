//! 真机事实探针：把 `doctor` 采集到的事实打出来，**一个字节都不写机器**。
//!
//! 它存在的理由与 `path_probe` 一样：检查项是纯函数，但**事实必须先在真机上对一遍** ——
//! 否则"检查项全绿"可能只是因为事实是空的（而空事实永远不报问题）。
//!
//! 跑法（`cargo run` 会先构建，不需要手写 PATH 前缀之外的准备）：
//!
//! ```text
//! cargo run --release -p tuoen-core --example doctor_probe
//! ```

use std::time::Duration;

use tuoen_core::detect::DetectContext;
use tuoen_core::doctor::{DoctorOptions, facts};

fn main() {
    let fs = tuoen_platform::RealFileSystem;
    let registry = tuoen_platform::RealRegistry;
    let env = tuoen_platform::RealEnvBlock::new(registry, fs);
    let process_env = tuoen_platform::RealProcessEnv::new();
    let runner = tuoen_platform::SystemProcessRunner;
    let managed = tuoen_platform::RealManagedStore {
        root: std::path::PathBuf::from(
            std::env::var("LOCALAPPDATA").unwrap_or_default() + r"\tuoen",
        ),
    };
    let scan_roots = tuoen_core::detect::engine::default_scan_roots(&process_env);

    let ctx = DetectContext {
        fs: &fs,
        registry: &registry,
        env: &env,
        process_env: &process_env,
        runner: &runner,
        managed: &managed,
        probe_timeout: Duration::from_secs(5),
        // `doctor` 只关心结构不关心版本（`DetectContext::probe_versions` 的文档）。
        probe_versions: false,
        scan_roots,
    };

    let opts = DoctorOptions::new()
        .with_tuoen_root(std::env::var("LOCALAPPDATA").unwrap_or_default() + r"\tuoen")
        .probing_prefixes();

    let facts = facts::collect_facts(&ctx, &opts);

    println!("=== system ===");
    println!("elevated        = {:?}", facts.system.elevated);
    println!("developer_mode  = {:?}", facts.system.developer_mode);
    println!("long_paths      = {:?}", facts.system.long_paths);

    println!(
        "=== path（{} 条 / effective {} 条）===",
        facts.path.entry.len(),
        facts.path.effective.len()
    );
    println!(
        "budget: raw_user={} raw_machine={} effective={} level={}",
        facts.path.budget.raw_user_chars,
        facts.path.budget.raw_machine_chars,
        facts.path.budget.effective_chars,
        facts.path.budget.level
    );
    let dups = facts
        .path
        .entry
        .iter()
        .filter(|row| row.dup_index > 0)
        .count();
    let missing = facts
        .path
        .entry
        .iter()
        .filter(|row| !row.empty && row.exists == tuoen_core::capture::Existence::No)
        .count();
    let usernames = facts
        .path
        .entry
        .iter()
        .filter(|row| row.has_username)
        .count();
    let empty = facts.path.entry.iter().filter(|row| row.empty).count();
    println!("dup_index>0={dups} missing={missing} username={usernames} empty={empty}");

    println!("=== env（{} 条）===", facts.env.var.len());
    let mut by_target = std::collections::BTreeMap::new();
    for row in &facts.env.var {
        *by_target
            .entry(format!("{:?}", row.target_exists))
            .or_insert(0usize) += 1;
    }
    println!("target_exists: {by_target:?}");

    println!("=== tools（{} 行）===", facts.tools.tool.len());
    let mut by_confidence = std::collections::BTreeMap::new();
    for row in &facts.tools.tool {
        *by_confidence
            .entry(row.confidence.clone())
            .or_insert(0usize) += 1;
    }
    println!("confidence: {by_confidence:?}");

    println!("=== resolution（{} 条命令）===", facts.resolution.len());
    let mut by_tool: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for row in &facts.resolution {
        if let Some(dir) = &row.directory {
            by_tool
                .entry(row.tool_id.as_str())
                .or_default()
                .push(format!("{}={}", row.command, dir));
        }
    }
    for (tool, dirs) in &by_tool {
        println!("  {tool}: {}", dirs.join(" | "));
    }

    println!("=== global_prefix（{} 条）===", facts.global_prefix.len());
    for row in &facts.global_prefix {
        println!(
            "  {} = {}  来源={:?} reparse={} 目标={:?} 版本成分={:?}",
            row.tool_id,
            row.prefix,
            row.origin,
            row.inside_reparse,
            row.link_target,
            row.version_component
        );
    }

    println!("=== dev_roots（{} 条）===", facts.dev_roots.len());
    for row in &facts.dev_roots {
        println!(
            "  {}  （{} 个子条目；{}）",
            row.path, row.children, row.looks_like
        );
    }

    println!("=== shims ===");
    println!(
        "dir={:?} on_path={} commands={:?}",
        facts.shims.dir, facts.shims.on_path, facts.shims.commands
    );

    println!(
        "SUMMARY path_entries={} env_vars={} tool_rows={} distros={} shims={} resolved={}",
        facts.path.entry.len(),
        facts.env.var.len(),
        facts.tools.tool.len(),
        facts.wsl.distribution.len(),
        facts.shims.commands.len(),
        facts.resolution.len()
    );

    // ── 诊断（纯函数，吃上面那批事实） ──────────────────────────────
    println!("=== findings ===");
    let report = tuoen_core::doctor::run(&ctx, &opts);
    for finding in &report.findings {
        println!(
            "[{}] {}  (source={} confidence={:?})",
            finding.severity.as_str(),
            finding.id,
            finding.source,
            finding.confidence
        );
        for line in &finding.evidence {
            println!("      {line}");
        }
    }
    println!(
        "SUMMARY counts error={} warn={} info={} findings={}",
        report.counts.error,
        report.counts.warn,
        report.counts.info,
        report.findings.len()
    );
}
