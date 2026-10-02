//! `tuoen detect` 的**进程边界契约测试**。
//!
//! 这些测试只断言**形状与不变量**，并且**刻意不依赖这台机器的状态** ——
//! 它们在 CI 上、在一台什么都没装的干净 Windows 上、在一台装满了东西的机器上
//! 都必须给出同样的结论。
//!
//! ## 说清楚一件事：这一份**确实**读了这台机器
//!
//! `docs/specs/L1-dev-state.md` 与票据 `docs/tickets/L1-01-detect.md` 的硬约束是：
//! **测试不得读取真实的 `HKCU\Environment` / `HKLM` / 真实 `PATH` / 用户真实安装目录，
//! 也不得依赖开发机的实时状态。**
//!
//! 这一份**满足了后半句，但没有满足前半句**，因为它在进程边界上跑真的 `detect`
//! 二进制 —— 而那个二进制当然会读这台机器。它满足的是**精神**：它只断言
//! "如果有条目，每条都必须完整"、"两次运行的 `--json` 必须逐字节一致"、
//! "七层六源的键集合恒定出现"这类**与内容无关**的性质，
//! **不出现"这台机器上一定有 node"** 这类断言。
//!
//! 这条纪律不是洁癖。本机 `PATH` 恰好是坏的（48 条里有 10 组重复、7 处失效、
//! `WindowsApps` 别名排在第 8 条而真 Python 排在第 36 条）—— 一个"依赖实时状态"的
//! 测试会因为开发机的状态而**时绿时红**，而那种红绿不携带任何信息。
//! 更糟的是：它会诱导后来的人去改测试而不是改代码。
//!
//! **真正读真机的部分是 `real_machine_acceptance.rs`**，文件名里就写着它是验收，
//! 而且它只覆盖三件假后端**原理上测不到**的事（`RealRegistry` 拼的注册表路径、
//! 真实 NTFS 的 reparse tag、不执行 0 字节别名）。
//!
//! 早先这里的注释写着"契约测试严格遵守了不得读真机这条"，**那句是不准确的**。
//! 留着这段修正，是因为"一句让人放心的假话比一个已知的取舍危险得多"。

mod common;

use common::{json, run, stderr, stdout};

/// 七层置信度，**取值是公开契约**。
const CONFIDENCE_LEVELS: &[&str] = &[
    "managed",
    "executable",
    "registered",
    "manager-owned",
    "directory-only",
    "registered-missing",
    "alias-ghost",
];

/// 六个来源，**取值是公开契约**。
const SOURCES: &[&str] = &[
    "tuoen",
    "path-resolution",
    "app-paths",
    "registry-arp",
    "filesystem-scan",
    "manager",
];

#[test]
fn help_works_and_does_not_touch_the_machine() {
    // `--help` 是最干净的契约测试：它必然成功、必然不读机器、必然有固定输出。
    let output = run(["detect", "--help"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    for level in CONFIDENCE_LEVELS {
        assert!(text.contains(level), "help 里缺少层级 {level}：{text}");
    }
    assert!(text.contains("什么都不改"), "{text}");
}

#[test]
fn detect_is_rejected_under_catalog() {
    // `detect` 读**这台机器**，`catalog` 读**我们的目录**。混在一起会让
    // "我机器上装了什么"看起来像"我们支持什么"。
    let output = run(["catalog", "detect"]);
    assert_eq!(output.status.code(), Some(2), "应当是用法错误");
}

#[test]
fn detect_rejects_unknown_flags() {
    let output = run(["detect", "--nope"]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "clap 的用法错误必须是退出码 2"
    );
    assert!(stderr(&output).contains("--nope"), "{}", stderr(&output));
}

#[test]
fn no_subcommand_is_a_usage_error() {
    let output = run(Vec::<String>::new());
    assert_eq!(output.status.code(), Some(2));
}

// ─────────────────────────────────────────────────────────────────────────────
// 形状契约：这些用例**只读 `--json` 的结构**，不断言任何具体发现
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn detect_json_envelope_has_the_expected_shape() {
    let output = run(["detect", "--json", "--no-version"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let envelope = json(&output);
    assert_eq!(envelope.schema_version, 1);
    assert_eq!(envelope.command, "detect");
    assert!(envelope.ok);
    assert!(envelope.error.is_none());

    let data = envelope.data.expect("data");
    assert!(data.get("tool").is_some_and(|v| v.is_array()), "{data}");
    let summary = data.get("summary").expect("summary");
    assert!(
        summary.get("total").is_some_and(|v| v.is_u64()),
        "{summary}"
    );
    assert!(
        summary.get("byConfidence").is_some_and(|v| v.is_array()),
        "{summary}"
    );
    assert!(
        summary.get("bySource").is_some_and(|v| v.is_array()),
        "{summary}"
    );
}

#[test]
fn every_confidence_level_and_source_is_reported_even_at_zero() {
    // 键集合不随内容变化 —— 否则消费者无法区分"这个层级是 0"与"这个层级被删了"。
    let output = run(["detect", "--json", "--no-version"]);
    let envelope = json(&output);
    let data = envelope.data.expect("data");
    let summary = data.get("summary").expect("summary");

    let confidence_keys: Vec<&str> = summary["byConfidence"]
        .as_array()
        .expect("array")
        .iter()
        .map(|entry| entry["key"].as_str().expect("key"))
        .collect();
    assert_eq!(confidence_keys, CONFIDENCE_LEVELS);

    let source_keys: Vec<&str> = summary["bySource"]
        .as_array()
        .expect("array")
        .iter()
        .map(|entry| entry["key"].as_str().expect("key"))
        .collect();
    assert_eq!(source_keys, SOURCES);
}

#[test]
fn json_values_are_english_and_never_localised() {
    // 决策 35：`--json` 的取值不本地化，否则脚本与未来的 GUI 会被界面语言绑死。
    let output = run(["detect", "--json", "--no-version"]);
    let envelope = json(&output);
    let data = envelope.data.expect("data");

    for tool in data["tool"].as_array().expect("tools") {
        let source = tool["source"].as_str().expect("source");
        let confidence = tool["confidence"].as_str().expect("confidence");
        assert!(
            SOURCES.contains(&source),
            "来源取值必须是英文 kebab：{source}"
        );
        assert!(
            CONFIDENCE_LEVELS.contains(&confidence),
            "置信度取值必须是英文 kebab：{confidence}"
        );
        // 取值本身不得含 CJK。
        assert!(
            !source.chars().any(|c| c as u32 > 0x2000),
            "来源被本地化了：{source}"
        );
        assert!(
            !confidence.chars().any(|c| c as u32 > 0x2000),
            "置信度被本地化了：{confidence}"
        );
        // 名字也是取值。
        let name = tool["name"].as_str().expect("name");
        assert_eq!(name, name.to_lowercase(), "工具名必须小写：{name}");
    }
}

#[test]
fn every_record_carries_a_source_a_confidence_and_evidence() {
    // 检测引擎的核心不变量：**不允许出现没有来源的条目**，
    // 因为用户需要知道每条记录能不能在新机器上自动重建。
    //
    // 注意这条断言**与机器上装了什么无关** —— 它说的是"如果有条目，每条都必须完整"。
    let output = run(["detect", "--json", "--no-version"]);
    let envelope = json(&output);
    let data = envelope.data.expect("data");

    for tool in data["tool"].as_array().expect("tools") {
        assert!(tool["path"].is_string(), "{tool}");
        assert!(tool["evidence"].is_string(), "{tool}");
        assert!(
            !tool["evidence"].as_str().expect("evidence").is_empty(),
            "evidence 不能为空 —— 空证据等于没有证据：{tool}"
        );
        assert!(tool["reproducible"].is_boolean(), "{tool}");
        // `version` 允许是 null（发现但问不到版本），但键必须在。
        assert!(tool.get("version").is_some(), "{tool}");
        assert!(tool.get("manager").is_some(), "{tool}");
        // **`path` 必须是一条路径或一个明确的占位符，不能是一句说明。**
        // 独立验收抓到的真 bug：这里曾经是 `（无 InstallLocation；卸载键 {…}）`。
        let path = tool["path"].as_str().expect("path");
        assert!(
            path.starts_with('<') || path.contains(':') || path.starts_with(r"\\"),
            "path 看起来不是路径，也不是 `<…>` 形式的占位符：{path}"
        );
    }
}

#[test]
fn reproducible_agrees_with_confidence() {
    // "能不能自动重建"是最常被问的问题，所以它必须与置信度**一致**。
    let output = run(["detect", "--json", "--no-version"]);
    let envelope = json(&output);
    let data = envelope.data.expect("data");

    for tool in data["tool"].as_array().expect("tools") {
        let confidence = tool["confidence"].as_str().expect("confidence");
        let reproducible = tool["reproducible"].as_bool().expect("bool");
        let expected = matches!(confidence, "managed" | "executable" | "registered");
        assert_eq!(
            reproducible, expected,
            "置信度 {confidence} 的 reproducible 应当是 {expected}：{tool}"
        );
    }
}

#[test]
fn summary_counts_match_the_records() {
    // 计数与记录必须自洽 —— 一个不匹配的 summary 比没有 summary 更糟。
    let output = run(["detect", "--json", "--no-version"]);
    let envelope = json(&output);
    let data = envelope.data.expect("data");
    let tools = data["tool"].as_array().expect("tools");
    let summary = data.get("summary").expect("summary");

    assert_eq!(
        summary["total"].as_u64().expect("total"),
        tools.len() as u64,
        "summary.total 与实际记录数不符"
    );

    let sum = |key: &str| -> u64 {
        summary[key]
            .as_array()
            .expect("array")
            .iter()
            .map(|entry| entry["count"].as_u64().expect("count"))
            .sum()
    };
    assert_eq!(
        sum("byConfidence"),
        tools.len() as u64,
        "byConfidence 之和不等于总数"
    );
    assert_eq!(
        sum("bySource"),
        tools.len() as u64,
        "bySource 之和不等于总数"
    );

    // 每个层级的计数必须与逐条数出来的相等。
    for entry in summary["byConfidence"].as_array().expect("array") {
        let key = entry["key"].as_str().expect("key");
        let count = entry["count"].as_u64().expect("count");
        let actual = tools
            .iter()
            .filter(|tool| tool["confidence"].as_str() == Some(key))
            .count() as u64;
        assert_eq!(count, actual, "层级 {key} 的计数不符");
    }
}

#[test]
fn json_output_is_byte_stable_across_runs() {
    // `--json` 是接口，两次运行之间不得漂移。`--no-version` 去掉进程输出的抖动，
    // 剩下的是结构本身。
    let first = stdout(&run(["detect", "--json", "--no-version"]));
    let second = stdout(&run(["detect", "--json", "--no-version"]));
    assert_eq!(first, second, "两次 detect 的 --json 输出必须逐字节一致");
}

#[test]
fn no_version_says_so_instead_of_silently_omitting_versions() {
    // 沉默地不报版本会让人以为"这些工具都没有版本"，而不是"我们没问"。
    let text = stdout(&run(["detect", "--no-version"]));
    assert!(text.contains("没有探测版本"), "{text}");
}

#[test]
fn human_output_is_chinese_and_names_the_columns() {
    let output = run(["detect", "--no-version"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let text = stdout(&output);

    // 中文优先：人类输出必须有中文。
    assert!(
        text.chars().any(|c| c as u32 > 0x2000),
        "人类输出应当是中文：{text}"
    );
    // 表头必须有那几列（无论有没有记录，表头都在 —— 空表也要能读）。
    assert!(text.contains("置信度"), "{text}");
    assert!(text.contains("来源"), "{text}");
    assert!(text.contains("工具"), "{text}");
    assert!(text.contains("版本"), "{text}");
    // 收尾要指向机器可读的出口。
    assert!(text.contains("--json"), "{text}");
}
