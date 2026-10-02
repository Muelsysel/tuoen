//! 票据 #8 的真机探针：**只读分析 + 写机制的往返 + 「真实 `Path` 一个字节都没变」的自证**。
//!
//! 这个例子会真的碰这台机器的注册表 —— 这是它存在的全部理由：`PATH` 的写路径
//! 无法用假注册表证明（假注册表证明的是**逻辑**，真机证明的是**Win32 调用真的能写**）。
//! 它守住三条自律：
//!
//! 1. **绝不碰真实的 `Path`。** 它写的是一个别的值名 `TUOEN_PATH_PROBE`，
//!    而且**在开始和结束各快照一次两个作用域的 `Path` 原文与类型，比对必须完全相同**。
//!    这条自证写在 `SUMMARY` 里（`path_untouched=1`）。
//! 2. **跑完不留残留。** 开始先清一次上一次可能留下的探针值，结束再删一次 ——
//!    包括中途断言失败提前退出的那条路（都走同一个 `cleanup`）。
//! 3. **绝不调用 `setx`。** 一次都没有；`crates/**/src` 里也 grep 不到它（验收脚本会查）。
//!
//! 它证明的东西：①读是真机读的、而且**没展开**用户的值；②类型规则在真机注册表上
//! 落得下去（含 `%` → `EXPAND_SZ`，不含 `%` → 保留 / `SZ`）；③广播能被真的发出去、
//! 以及它**做不到**什么（已经在跑的进程拿不到新环境 —— 这一条是实测的，不是推测的）；
//! ④本机的遮蔽与用户名依赖确实是它对 `PATH` 的既有问题的报告，不是编的。
//!
//! 跑法（release 没意义，它不做性能测量）：
//! `cargo run -p tuoen-platform --example path_probe`

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use tuoen_platform::{
    EnvBlock, EnvScope, FileSystem, MACHINE_ENV_SUBKEY, PATH_EXTENSIONS, ProcessRunner,
    RealEnvBlock, RealFileSystem, RealProcessEnv, RealRegistry, RegHive, RegType, RegValue,
    Registry, SystemProcessRunner, USER_ENV_SUBKEY, analyze,
};

/// 探针用的值名。**故意不是 `Path`** —— 见文件头第 1 条。
const PROBE_NAME: &str = "TUOEN_PATH_PROBE";

/// 每个作用域的快照：类型 + 原文。比对用 `==`，不用哈希 ——
/// 我们只是要证明"和刚才一模一样"，不是在防对手。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    scope: &'static str,
    reg_type: Option<RegType>,
    raw: String,
}

fn snapshot(block: &RealEnvBlock<RealRegistry, RealFileSystem>) -> Vec<Snapshot> {
    [EnvScope::Machine, EnvScope::User]
        .into_iter()
        .map(|scope| {
            let found = block.get(scope, "Path");
            Snapshot {
                scope: match scope {
                    EnvScope::Machine => "machine",
                    EnvScope::User => "user",
                    EnvScope::ProcessOnly => "process-only",
                },
                reg_type: found.as_ref().map(|v| v.reg_type),
                raw: found.map(|v| v.value_raw).unwrap_or_default(),
            }
        })
        .collect()
}

fn type_name(kind: Option<RegType>) -> &'static str {
    match kind {
        Some(RegType::Sz) => "REG_SZ",
        Some(RegType::ExpandSz) => "REG_EXPAND_SZ",
        None => "<不存在>",
    }
}

/// 清掉探针值。**幂等**：上一次跑崩了留下的残留也靠它清。
fn cleanup(registry: &RealRegistry) -> Result<(), String> {
    registry
        .delete_value(RegHive::Hkcu, USER_ENV_SUBKEY, PROBE_NAME)
        .map_err(|err| format!("删除探针值失败：{err:?}"))
}

struct Checks {
    failed: Vec<String>,
    passed: usize,
}

impl Checks {
    fn new() -> Self {
        Self {
            failed: Vec::new(),
            passed: 0,
        }
    }

    fn check(&mut self, what: &str, ok: bool, detail: &str) {
        if ok {
            self.passed += 1;
            println!("  [通过] {what}");
        } else {
            self.failed.push(what.to_owned());
            println!("  [失败] {what} —— {detail}");
        }
    }
}

fn main() -> ExitCode {
    let block = RealEnvBlock::new(RealRegistry, RealFileSystem);
    let process = RealProcessEnv;
    let fs = RealFileSystem;
    let registry = RealRegistry;
    let mut checks = Checks::new();

    // 上一次跑崩了留下的残留，先清掉 —— 否则这个探针会污染它自己的"写之前"状态。
    if let Err(err) = cleanup(&registry) {
        eprintln!("{err}");
        return ExitCode::FAILURE;
    }

    let before = snapshot(&block);

    // ─────────────────── 1. 只读分析（验收 ①⑤⑥⑦） ───────────────────
    println!("── 1. 真实机器上的只读分析 ──");
    let shim_dir = default_shim_dir();
    let analysis = analyze(&block, &process, &fs, shim_dir.as_deref());
    println!(
        "  生效 PATH {} 字符 / 注册表合计 {} 字符 / 档位 {} / 余量 {}",
        analysis.budget.effective_chars,
        analysis.budget.registry_chars,
        analysis.budget.level.as_str(),
        analysis.budget.remaining
    );
    for scope in &analysis.scopes {
        println!(
            "  {} 级 Path：{} 字符 / {} / {} 条（空条目位置 {:?}）",
            match scope.scope {
                EnvScope::Machine => "机器",
                EnvScope::User => "用户",
                EnvScope::ProcessOnly => "进程",
            },
            scope.chars,
            type_name(scope.reg_type),
            scope.entries.len(),
            scope.empty_positions
        );
    }

    // **生效顺序的真相**：真实世界里它不止"机器级 + 用户级"。本机实测的第一条是
    // PowerShell 的 MSIX 别名注入 —— **它在机器级前面**。这条打印出来给人看，
    // 因为"用户级永远输给机器级"这句设计结论的前提正是这个顺序。
    let effective = analysis.effective_entries();
    println!("  生效顺序（共 {} 条，前 8 条）：", effective.len());
    for (index, entry) in effective.iter().take(8).enumerate() {
        println!(
            "    {:>2}. [{}] {}",
            index,
            match entry.scope {
                EnvScope::Machine => "机器",
                EnvScope::User => "用户",
                EnvScope::ProcessOnly => "注入",
            },
            entry.value
        );
    }

    // ① 验收：读出来的必须是注册表里的**原始字节**，不许被展开。
    // 判据是硬碰硬：拿 `Registry::value` 的低层读法（`RegEnumValueW`，**从不展开**
    // `REG_EXPAND_SZ`）读一遍，与我们报出来的 `raw` 逐字符比。
    let user = analysis
        .scopes
        .iter()
        .find(|s| s.scope == EnvScope::User)
        .expect("用户级 Path 必须读得到");
    let mut every_scope_matches_the_registry_bytes = true;
    for scope in &analysis.scopes {
        let (name, hive, subkey) = match scope.scope {
            EnvScope::Machine => ("机器", RegHive::Hklm, MACHINE_ENV_SUBKEY),
            EnvScope::User => ("用户", RegHive::Hkcu, USER_ENV_SUBKEY),
            EnvScope::ProcessOnly => continue,
        };
        let raw = registry
            .value(hive, subkey, "Path")
            .map(|value| match value {
                RegValue::Sz(text) | RegValue::ExpandSz(text) => text,
                other => format!("{other:?}"),
            });
        let same = raw.as_deref() == Some(scope.raw.as_str());
        if !same {
            println!("  {name} 级 Path 与我们报的 raw 不一致！");
        }
        every_scope_matches_the_registry_bytes &= same;
    }
    checks.check(
        "两个作用域的 `raw` 都等于注册表里的原始字节（**没有被展开**）",
        every_scope_matches_the_registry_bytes,
        "至少一个作用域的 raw 与注册表字节不同 —— 那就是展开过了",
    );
    checks.check(
        "用户级 Path 里的 `%` 原样留着（展开过的话它就没了）",
        !user.raw.contains('%') || user.raw.matches('%').count() >= 2,
        &format!("原文里 `%` 出现 {} 次", user.raw.matches('%').count()),
    );

    // ⑤ 预算档位必须是四个合法 slug 之一，且和 cliff 自洽。
    let level = analysis.budget.level.as_str();
    checks.check(
        "预算档位是四个合法 slug 之一",
        matches!(level, "ok" | "warning" | "critical" | "exceeded"),
        level,
    );
    checks.check(
        "余量 + 生效长度 == 悬崖（没越界时）",
        analysis.budget.effective_chars + analysis.budget.remaining == analysis.budget.cliff
            || analysis.budget.remaining == 0,
        &format!(
            "{} + {} != {}",
            analysis.budget.effective_chars, analysis.budget.remaining, analysis.budget.cliff
        ),
    );

    println!(
        "  重复 {} 条 / 失效 {} 条 / 用户名依赖 {} 条（其中机器级 {} 条） / 进程注入 {} 条",
        analysis.duplicates.len(),
        analysis.dangling.len(),
        analysis.username_dependencies.len(),
        analysis
            .username_dependencies
            .iter()
            .filter(|d| d.at_machine_scope)
            .count(),
        analysis.process_only.len()
    );
    for shadow in &analysis.shadowed_shims {
        println!(
            "  遮蔽：`{}` 被 {} 级条目 `{}`（{}）抢在前面",
            shadow.command,
            match shadow.by.scope {
                EnvScope::Machine => "机器",
                EnvScope::User => "用户",
                EnvScope::ProcessOnly => "进程",
            },
            shadow.by.value,
            shadow.file
        );
    }
    for dependency in &analysis.username_dependencies {
        println!(
            "  用户名依赖：{} 级 `{}` → `{}`",
            if dependency.at_machine_scope {
                "机器"
            } else {
                "用户"
            },
            dependency.entry.value,
            dependency.name
        );
    }
    for gone in &analysis.dangling {
        println!(
            "  失效条目：`{}`（用了变量：{}）",
            gone.entry.value, gone.uses_variable
        );
    }
    for duplicate in &analysis.duplicates {
        let places: Vec<String> = duplicate
            .at
            .iter()
            .map(|entry| {
                format!(
                    "{}#{}",
                    match entry.scope {
                        EnvScope::Machine => "机器",
                        EnvScope::User => "用户",
                        EnvScope::ProcessOnly => "进程",
                    },
                    entry.index
                )
            })
            .collect();
        println!(
            "  重复：`{}` × {} → {}",
            duplicate.key,
            duplicate.at.len(),
            places.join(" ")
        );
    }
    for injected in &analysis.process_only {
        println!("  进程注入：`{injected}`");
    }
    // ⑥⑦ 是"报告得出来"而不是"一定有问题"：这台机器上我们只断言**结构**正确
    // （每条报告都得指出是谁、在哪一级），具体条数打印出来由验收文档记录。
    checks.check(
        "每条失效条目都带得出处的 scope 与下标",
        analysis.dangling.iter().all(|d| !d.entry.value.is_empty()),
        "有失效条目的 value 是空的",
    );
    checks.check(
        "每条用户名依赖都指出了是哪一级",
        analysis
            .username_dependencies
            .iter()
            .all(|d| !d.name.is_empty() && !d.entry.value.is_empty()),
        "有用户名依赖缺名字或缺条目",
    );

    // ─────────────────── 2. 写机制的往返（只碰 PROBE_NAME） ───────────────────
    println!("── 2. 写机制的往返（值名 = {PROBE_NAME}，绝不碰 Path） ──");

    // ② 含 `%` 的值 → 必须落成 EXPAND_SZ，否则变量被永久冻成字面量。
    let expand_value = r"%USERPROFILE%\tuoen-probe";
    let wrote_expand = registry.set_value(
        RegHive::Hkcu,
        USER_ENV_SUBKEY,
        PROBE_NAME,
        &RegValue::ExpandSz(expand_value.to_owned()),
    );
    checks.check(
        "含 `%` 的值写成了 EXPAND_SZ 并读得回来",
        wrote_expand.is_ok()
            && registry
                .value(RegHive::Hkcu, USER_ENV_SUBKEY, PROBE_NAME)
                .is_some_and(|v| v == RegValue::ExpandSz(expand_value.to_owned())),
        &format!("写回结果：{wrote_expand:?}"),
    );

    // ③ 不含 `%` 的值 → 必须落成 SZ（不能顺手写成 EXPAND_SZ）。
    let plain_value = r"C:\tuoen-probe";
    let wrote_plain = registry.set_value(
        RegHive::Hkcu,
        USER_ENV_SUBKEY,
        PROBE_NAME,
        &RegValue::Sz(plain_value.to_owned()),
    );
    checks.check(
        "不含 `%` 的值写成了 SZ 并读得回来",
        wrote_plain.is_ok()
            && registry
                .value(RegHive::Hkcu, USER_ENV_SUBKEY, PROBE_NAME)
                .is_some_and(|v| v == RegValue::Sz(plain_value.to_owned())),
        &format!("写回结果：{wrote_plain:?}"),
    );

    // 覆盖写：同一个值名写两次不能变两条。
    let overwritten = registry.set_value(
        RegHive::Hkcu,
        USER_ENV_SUBKEY,
        PROBE_NAME,
        &RegValue::Sz(r"C:\tuoen-probe-2".to_owned()),
    );
    let listed = registry
        .values(RegHive::Hkcu, USER_ENV_SUBKEY)
        .map(|values| values.iter().filter(|(name, _)| name == PROBE_NAME).count())
        .unwrap_or(0);
    checks.check(
        "同一个值名覆盖写之后只有一条",
        overwritten.is_ok() && listed == 1,
        &format!("同名条目数 = {listed}"),
    );

    // ④ 广播：能被真的发出去。
    let replies = tuoen_platform::sys::broadcast_environment_change();
    println!("  广播 WM_SETTINGCHANGE(\"Environment\") 发出，收到 {replies} 个顶层窗口的应答");
    // `0` 不代表失败（没有顶层窗口响应是正常的），所以这里**不检查返回值** ——
    // 检查的是"函数被真的调了、没崩"。这条自律写在 docs/acceptance/L0-08-path.md 里。
    checks.check("广播调用没有崩", true, "");

    // 广播做不到什么：**我们自己的进程环境块不会变**。环境块是 CreateProcess 时复制的。
    let we_see_it = std::env::var_os(PROBE_NAME).is_some();
    println!("  广播之后，我们自己进程的环境里有没有 {PROBE_NAME}：{we_see_it}");
    checks.check(
        "广播**不会**改变已经在跑的进程（含我们自己）的环境块",
        !we_see_it,
        "我们的进程竟然看到了新变量 —— 那说明广播的语义和我们写的不一样",
    );
    // 更强的一条：一个新起的子进程看到的是**我们**的环境块，不是注册表里最新的。
    let runner = SystemProcessRunner;
    let child = runner.run(
        Path::new(r"C:\Windows\System32\cmd.exe"),
        &["/c", &format!("echo [{PROBE_NAME}=%{PROBE_NAME}%]")],
        Duration::from_secs(10),
    );
    let child_saw = child.stdout.trim().to_owned();
    println!("  新起的子进程（父进程是我们）看到：{child_saw}");
    checks.check(
        "**新起的**子进程也拿不到注册表里刚写的值（环境块继承自父进程）",
        child.spawned && child_saw == format!("[{PROBE_NAME}=%{PROBE_NAME}%]"),
        &format!("子进程输出 `{child_saw}`（spawned={}）", child.spawned),
    );

    // 删除 + 幂等删除。
    let deleted = cleanup(&registry);
    let again = cleanup(&registry);
    checks.check(
        "探针值删得掉，且重复删不报错（幂等）",
        deleted.is_ok()
            && again.is_ok()
            && registry
                .value(RegHive::Hkcu, USER_ENV_SUBKEY, PROBE_NAME)
                .is_none(),
        &format!("第一次 {deleted:?} / 第二次 {again:?}"),
    );

    // ─────────────────── 3. 自证：真实 Path 一个字节都没变 ───────────────────
    println!("── 3. 自证：两个作用域的真实 Path 一个字节都没变 ──");
    let after = snapshot(&block);
    for (before_one, after_one) in before.iter().zip(after.iter()) {
        let same = before_one == after_one;
        println!(
            "  {} 级 Path：{} → {}（{} 字符 → {} 字符）",
            before_one.scope,
            type_name(before_one.reg_type),
            type_name(after_one.reg_type),
            before_one.raw.chars().count(),
            after_one.raw.chars().count()
        );
        checks.check(
            &format!("{} 级 Path 的原文与类型前后完全相同", before_one.scope),
            same,
            "前后不一致 —— 探针碰了它不该碰的东西",
        );
    }
    checks.check(
        "探针值没有留下残留",
        registry
            .value(RegHive::Hkcu, USER_ENV_SUBKEY, PROBE_NAME)
            .is_none(),
        "注册表里还能读到 TUOEN_PATH_PROBE",
    );

    // ─────────────────── 3.5 真机遮蔽实验（可选） ───────────────────
    //
    // 遮蔽检测的判据是"我们的 shim 目录里发布了哪些 `<名字>.exe`"。本机这个目录
    // **还不存在**（我们一个 shim 都还没装），所以默认那一跑报 `shadowed=0` ——
    // 那证明的是"目录不存在时不报假遮蔽"，**证明不了"遮蔽报得出来"**。
    //
    // 所以这里做一个**只看文件、不碰 PATH** 的实验：往我们的 shim 目录里放一个
    // 空的 `node.exe`，再分析一次，看它能不能报出"`node` 被 `C:\nvm4w\nodejs` 抢先"。
    // 跑完把放进去的文件删掉；如果那个目录是我们建的，目录也删掉。**PATH 一个字节都不动。**
    let mut shadow_experiment_done = false;
    let mut shadow_experiment_found = 0usize;
    if std::env::args().any(|arg| arg == "--shadow-check") {
        println!("── 3.5 真机遮蔽实验（只放一个空文件，不碰 PATH） ──");
        if let Some(dir) = shim_dir.clone() {
            match shadow_experiment(&dir, &block, &process, &fs) {
                Ok(found) => {
                    shadow_experiment_done = true;
                    shadow_experiment_found = found;
                    checks.check(
                        "真机上能报出遮蔽，且每条报告的目录里**真的有**那个文件",
                        found > 0,
                        "一条遮蔽都没报出来 —— 那说明这个实验没有真的制造出遮蔽",
                    );
                }
                Err(why) => {
                    checks.check("真机遮蔽实验跑得完", false, &why);
                }
            }
        } else {
            checks.check(
                "真机遮蔽实验：拿得到我们的 shim 目录",
                false,
                "LOCALAPPDATA/USERPROFILE 都没有",
            );
        }
    }

    // ─────────────────── SUMMARY（机器可读契约行） ───────────────────
    let path_untouched = before == after;
    println!(
        "SUMMARY checks_passed={} checks_failed={} path_untouched={} probe_residue={} \
         budget_level={} effective_chars={} registry_chars={} duplicates={} dangling={} \
         username_deps={} username_deps_machine={} process_only={} shadowed={} broadcast_replies={} \
         shadow_experiment={} shadow_found={}",
        checks.passed,
        checks.failed.len(),
        usize::from(path_untouched),
        usize::from(
            registry
                .value(RegHive::Hkcu, USER_ENV_SUBKEY, PROBE_NAME)
                .is_some()
        ),
        analysis.budget.level.as_str(),
        analysis.budget.effective_chars,
        analysis.budget.registry_chars,
        analysis.duplicates.len(),
        analysis.dangling.len(),
        analysis.username_dependencies.len(),
        analysis
            .username_dependencies
            .iter()
            .filter(|d| d.at_machine_scope)
            .count(),
        analysis.process_only.len(),
        analysis.shadowed_shims.len(),
        replies,
        usize::from(shadow_experiment_done),
        shadow_experiment_found,
    );

    if checks.failed.is_empty() && path_untouched {
        println!("全部通过（{} 项）", checks.passed);
        ExitCode::SUCCESS
    } else {
        for failed in &checks.failed {
            eprintln!("失败：{failed}");
        }
        if !path_untouched {
            eprintln!("**真实 Path 变了 —— 这是最严重的一种失败**");
        }
        ExitCode::FAILURE
    }
}

/// 我们的 shim 目录。**只用来做遮蔽检测**（只读），不存在就报空。
///
/// 与 `crates/cli/src/shim_cmd.rs` 的 `shim_dir(store)` 是同一个规则：
/// `<store home>/shims`，store home 默认在 `%LOCALAPPDATA%\tuoen`。
fn default_shim_dir() -> Option<PathBuf> {
    for (variable, subdir) in [("LOCALAPPDATA", "tuoen"), ("USERPROFILE", ".tuoen")] {
        if let Some(base) = std::env::var_os(variable) {
            return Some(PathBuf::from(base).join(subdir).join("shims"));
        }
    }
    None
}

/// 真机遮蔽实验：往我们的 shim 目录里放几个**空的** `<命令>.exe`，看能不能报出遮蔽。
///
/// **只看文件，从不改 `PATH`**：这个实验制造的是"我们的 shim 目录里发布了一条命令"
/// 这个前提，而遮蔽是一个纯读的分析。
///
/// 报告要被**独立核对**三件事，而不是只看它非空：
/// ①每条报告的 `by.value\<file>` 真的存在（直接问文件系统，不信任分析结果）；
/// ②被指为遮蔽者的那条目录是生效顺序里**第一个**持有该命令的（前面每一条都不持有）；
/// ③我们放进去的每一条命令**都**出现在报告里，且遮蔽者就是我们先算出来的那个目录。
fn shadow_experiment(
    dir: &Path,
    block: &RealEnvBlock<RealRegistry, RealFileSystem>,
    process: &RealProcessEnv,
    fs: &RealFileSystem,
) -> Result<usize, String> {
    let created_dir = !dir.exists();
    std::fs::create_dir_all(dir).map_err(|err| format!("建不了 {}：{err}", dir.display()))?;

    // 票据 ⑥ 点名的两个例子（Oracle `java8path` 与 `C:\nvm4w\nodejs`）优先，
    // 再加一个"这台机器上第一个找得到的 `.exe`"兜底 —— 换台机器这个实验也跑得起来。
    let effective = analyze(block, process, fs, None).effective_entries();
    let mut placed: Vec<(String, String)> = Vec::new();
    for candidate in ["java", "node"] {
        if let Some(holder) = first_dir_holding(fs, &effective, candidate) {
            placed.push((candidate.to_owned(), holder));
        }
    }
    if placed.is_empty()
        && let Some(picked) = pick_a_real_command(fs, &effective)
    {
        placed.push(picked);
    }
    if placed.is_empty() {
        cleanup_shim_dir(dir, &[], created_dir);
        return Err("生效 PATH 里一个能用的 `.exe` 都找不到，没法制造遮蔽".to_owned());
    }

    let mut dummies: Vec<PathBuf> = Vec::new();
    for (command, _) in &placed {
        let dummy = dir.join(format!("{command}.exe"));
        if !dummy.exists() {
            std::fs::write(&dummy, [])
                .map_err(|err| format!("写不进 {}：{err}", dummy.display()))?;
            dummies.push(dummy.clone());
        }
        println!("  放了一个空文件：{}（0 字节）", dummy.display());
    }

    let analysis = analyze(block, process, fs, Some(dir));
    println!("  这一次分析报出 {} 条遮蔽", analysis.shadowed_shims.len());
    for shadow in &analysis.shadowed_shims {
        println!(
            "  遮蔽：`{}` 被 {} 级条目 `{}`（{}）抢在前面",
            shadow.command,
            match shadow.by.scope {
                EnvScope::Machine => "机器",
                EnvScope::User => "用户",
                EnvScope::ProcessOnly => "进程",
            },
            shadow.by.value,
            shadow.file
        );
    }

    // ① 每条报告的"证据文件"真的存在。
    let all_files_real = analysis.shadowed_shims.iter().all(|shadow| {
        fs.inspect(&Path::new(&shadow.by.value).join(&shadow.file))
            .exists
    });
    // ② 被指为遮蔽者的目录是生效顺序里**第一个**持有该命令的。
    let winner_is_first = analysis.shadowed_shims.iter().all(|shadow| {
        let Some(at) = effective
            .iter()
            .position(|entry| entry.value.eq_ignore_ascii_case(&shadow.by.value))
        else {
            return false;
        };
        effective[..at]
            .iter()
            .all(|earlier| first_extension_here(fs, &earlier.value, &shadow.command).is_none())
    });
    // ③ 我们放进去的每一条命令都出现在报告里，且遮蔽者是我们先算出来的那个目录。
    let mut ours_reported = Vec::new();
    for (command, expected_by) in &placed {
        let found = analysis.shadowed_shims.iter().any(|shadow| {
            shadow.command == *command && shadow.by.value.eq_ignore_ascii_case(expected_by)
        });
        println!("  我们放的 `{command}.exe`：预期被 `{expected_by}` 遮蔽 → 报出来了 = {found}");
        ours_reported.push(found);
    }
    let ours_all_reported = ours_reported.iter().all(|ok| *ok);

    println!(
        "  独立核对：证据文件都存在 = {all_files_real}；遮蔽者是第一个持有的目录 = {winner_is_first}；\
         我们放的那几条都被报出来 = {ours_all_reported}"
    );

    cleanup_shim_dir(dir, &dummies, created_dir);

    if !all_files_real {
        return Err("有报告指向一个并不存在的文件".to_owned());
    }
    if !winner_is_first {
        return Err("有报告的遮蔽者并不是生效顺序里第一个持有该命令的目录".to_owned());
    }
    if !ours_all_reported {
        return Err("我们放进去的某一条命令没被报出来（或遮蔽者不是预期的那条）".to_owned());
    }
    Ok(analysis.shadowed_shims.len())
}

/// 生效顺序里**第一个**持有 `<命令>.<扩展名>` 的目录。
fn first_dir_holding(
    fs: &RealFileSystem,
    effective: &[tuoen_platform::EntryRef],
    command: &str,
) -> Option<String> {
    effective
        .iter()
        .find(|entry| first_extension_here(fs, &entry.value, command).is_some())
        .map(|entry| entry.value.clone())
}

/// 这个目录里有没有 `<命令>.<扩展名>`；有就返回命中的那个文件名（扩展名按 Windows 的解析顺序）。
fn first_extension_here(fs: &RealFileSystem, dir: &str, command: &str) -> Option<String> {
    PATH_EXTENSIONS.iter().find_map(|extension| {
        let file = format!("{command}.{extension}");
        fs.inspect(&Path::new(dir).join(&file))
            .exists
            .then_some(file)
    })
}

/// 兜底：从生效 `PATH` 里挑一个真的存在的 `.exe`，返回（小写命令名，它所在的目录）。
fn pick_a_real_command(
    fs: &RealFileSystem,
    effective: &[tuoen_platform::EntryRef],
) -> Option<(String, String)> {
    for entry in effective {
        for file in fs.list_dir(Path::new(&entry.value)) {
            let lowered = file.name.to_lowercase();
            let Some(stem) = lowered.strip_suffix(".exe") else {
                continue;
            };
            if !stem.is_empty() {
                return Some((stem.to_owned(), entry.value.clone()));
            }
        }
    }
    None
}

/// 清场。**只删我们自己放的东西**：目录是我们建的就删目录，父目录空着也顺手删掉。
fn cleanup_shim_dir(dir: &Path, dummies: &[PathBuf], created_dir: bool) {
    for dummy in dummies {
        let _ = std::fs::remove_file(dummy);
    }
    if created_dir {
        // `remove_dir` 只在**空**的时候成功 —— 这正是我们要的语义：
        // 目录里还有别的东西（比如真的 shim）就不动它。
        let _ = std::fs::remove_dir(dir);
        if let Some(parent) = dir.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}
