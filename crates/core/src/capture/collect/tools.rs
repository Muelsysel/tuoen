//! `tools.toml` —— 工具与版本。
//!
//! 这个采集器**自己不发现任何东西**：发现是上一票的检测引擎（`crate::detect`）干的活，
//! 六层置信度、六个来源、别名幽灵与幽灵条目的判据都在那里，且有各自的用例。
//! 这里的唯一职责是**把结果落成文件**，并且不丢掉那三样让记录可被相信的东西：
//! 来源、置信度、以及"这条能不能在新机器上重建"。
//!
//! # 为什么存字符串而不是枚举
//!
//! `source` / `confidence` 存的是 `detect.rs` 里那两个枚举的稳定 slug。
//! 存枚举会让未来新增一层置信度变成**一次文件格式变更**，而存 slug 只是多一个取值 ——
//! 读快照的一方本来就必须容忍不认识的取值（它读的是别人的机器）。

use crate::detect::{DetectContext, detect_all};

use super::super::files::{ToolRow, ToolsFile};

/// 采集工具与版本。
pub(crate) fn collect_tools(ctx: &DetectContext<'_>, captured_at: &str) -> ToolsFile {
    let summary = detect_all(ctx);
    let mut file = ToolsFile::new(captured_at);

    for tool in summary.tools {
        file.tool.push(ToolRow {
            name: tool.name,
            version: tool.version,
            path: tool.path,
            source: tool.source.as_str().to_owned(),
            confidence: tool.confidence.as_str().to_owned(),
            manager: tool.manager,
            evidence: tool.evidence,
            // `reproducible` 是 `confidence` 的投影，**而不是重新实现的判据** ——
            // 那个判断只有一处定义（`Confidence::is_reproducible`），
            // 散成两份必然会漂移。
            reproducible: tool.confidence.is_reproducible(),
        });
    }

    // 顺序稳定才能比逐字节。检测引擎自己的顺序是"来源顺序 + 发现顺序"，
    // 那个顺序对报告有用，对一个要被人 diff 的文件则是噪声。
    sort(&mut file.tool);
    file
}

/// 按（名称，路径，来源，置信度，版本）排序。
///
/// 版本用 `Option` 的自然序（`None` 在前）—— 这里不需要"哪个版本更新"的语义，
/// 只需要一个**确定的**顺序。真正的版本比较在 `tuoen_store::compare_versions`。
fn sort(rows: &mut [ToolRow]) {
    rows.sort_by(|a, b| {
        (
            &a.name,
            &a.path,
            &a.source,
            &a.confidence,
            a.version.as_deref(),
        )
            .cmp(&(
                &b.name,
                &b.path,
                &b.source,
                &b.confidence,
                b.version.as_deref(),
            ))
    });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tuoen_platform::RegHive;
    use tuoen_platform::fixture::{
        FixtureDir, FixtureKey, FixturePath, FixtureProcess, MachineFixture,
    };
    use tuoen_platform::{RegValue, fixture::FakeMachine};

    use super::super::super::files::ToolsFile;
    use super::super::super::test_support::CaptureFixture;

    /// 本机取证的一个缩影：nvm4w 管着 node、真 Python 在、一个目录谁都没注册。
    fn machine() -> MachineFixture {
        MachineFixture {
            env: BTreeMap::from([(
                "Path".to_owned(),
                r"C:\nvm4w\nodejs;C:\Python312;C:\Dev\Tool\apache-maven-3.9.5\bin".to_owned(),
            )]),
            paths: vec![FixturePath::symlink_dir(
                r"C:\nvm4w\nodejs",
                r"C:\Users\x\AppData\Local\nvm\v24.19.0",
            )],
            dirs: vec![
                FixtureDir::new(
                    r"C:\Users\x\AppData\Local\nvm\v24.19.0",
                    vec![FixturePath::file("node.exe", 80_000)],
                ),
                FixtureDir::new(
                    r"C:\Python312",
                    vec![FixturePath::file("python.exe", 102_400)],
                ),
                FixtureDir::new(
                    r"C:\Dev\Tool\apache-maven-3.9.5\bin",
                    vec![FixturePath::file("mvn.cmd", 2_048)],
                ),
            ],
            registry: vec![
                FixtureKey::new(
                    RegHive::Hkcu,
                    "Environment",
                    BTreeMap::from([(
                        "NVM_HOME".to_owned(),
                        RegValue::Sz(r"C:\Users\x\AppData\Local\nvm".to_owned()),
                    )]),
                ),
                FixtureKey::new(
                    RegHive::Hklm,
                    r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
                    BTreeMap::from([(
                        "NVM_SYMLINK".to_owned(),
                        RegValue::Sz(r"C:\nvm4w\nodejs".to_owned()),
                    )]),
                ),
            ],
            processes: vec![
                FixtureProcess {
                    program: r"C:\nvm4w\nodejs\node.exe".to_owned(),
                    stdout: "v24.19.0\n".to_owned(),
                    stderr: String::new(),
                    exit_code: Some(0),
                    timed_out: false,
                },
                FixtureProcess {
                    program: r"C:\Python312\python.exe".to_owned(),
                    stdout: "Python 3.12.10\n".to_owned(),
                    stderr: String::new(),
                    exit_code: Some(0),
                    timed_out: false,
                },
            ],
            managed: Vec::new(),
        }
    }

    /// 取这次捕获的 `tools.toml`。全部 section 的捕获必然包含它，所以这里直接断言。
    fn tools_of(fixture: &CaptureFixture) -> ToolsFile {
        fixture
            .capture_all("2026-10-02T12:00:00Z")
            .tools
            .expect("全部 section 的捕获必然包含 tools")
    }

    fn machine_handle(fixture: &CaptureFixture) -> &FakeMachine {
        &fixture.detect.machine
    }

    #[test]
    fn every_row_carries_a_source_a_confidence_and_an_evidence() {
        let fixture = CaptureFixture::build(&machine());
        let file = tools_of(&fixture);
        assert!(!file.tool.is_empty(), "这台假机器上必须有工具");
        for row in &file.tool {
            assert!(!row.source.is_empty(), "没有来源的条目：{row:?}");
            assert!(!row.confidence.is_empty(), "没有置信度的条目：{row:?}");
            assert!(!row.evidence.is_empty(), "没有证据的条目：{row:?}");
            assert!(!row.name.is_empty());
            assert!(
                row.path.starts_with(r"C:\") || row.path.starts_with('<'),
                "path 必须像路径或占位符：{row:?}"
            );
        }
    }

    #[test]
    fn the_manager_owned_node_says_it_cannot_be_rebuilt_by_us() {
        let fixture = CaptureFixture::build(&machine());
        let file = tools_of(&fixture);
        let node = file
            .tool
            .iter()
            .find(|row| row.name == "node" && row.manager.as_deref() == Some("nvm4w"))
            .expect("nvm4w 管着的 node");
        assert_eq!(node.confidence, "manager-owned");
        assert_eq!(node.version.as_deref(), Some("24.19.0"));
        assert!(
            !node.reproducible,
            "第三方管理器管的工具不能由我们重建 —— 这是还原时最重要的一条信息"
        );
    }

    #[test]
    fn the_rows_are_sorted_so_two_runs_are_byte_identical() {
        let fixture = CaptureFixture::build(&machine());
        let file = tools_of(&fixture);
        let mut sorted = file.tool.clone();
        sorted.sort_by(|a, b| {
            (
                &a.name,
                &a.path,
                &a.source,
                &a.confidence,
                a.version.as_deref(),
            )
                .cmp(&(
                    &b.name,
                    &b.path,
                    &b.source,
                    &b.confidence,
                    b.version.as_deref(),
                ))
        });
        assert_eq!(file.tool, sorted, "采集器自己必须排好序");
    }

    #[test]
    fn detecting_nothing_is_not_an_error_it_is_an_empty_file() {
        let fixture = CaptureFixture::build(&MachineFixture::default());
        let file = tools_of(&fixture);
        assert!(file.tool.is_empty());
        // 空文件照样要能被写出去（TOML 里连 `[[tool]]` 都不会出现）。
        let text = toml::to_string_pretty(&file).expect("空文件也必须序列化得出来");
        assert!(text.contains("schema_version = 1"), "{text}");
    }

    /// 采集器**不启动任何进程之外的东西**：它读的是注入的假机器。
    ///
    /// 这条用例是"测试绝不读真机"的一个可执行版本：假机器的进程运行器只会返回
    /// 固定装置里声明的那些结果，所以只要这条通过，就说明没有真实探测发生。
    #[test]
    fn probing_goes_through_the_injected_runner_only() {
        let fixture = CaptureFixture::build(&machine());
        let _ = fixture.capture_all("2026-10-02T12:00:00Z");
        assert_eq!(machine_handle(&fixture).runner.calls().len(), 2);
    }
}
