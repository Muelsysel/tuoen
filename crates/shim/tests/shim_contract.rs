//! shim 的进程边界契约测试。
//!
//! **这一票的全部风险都在"参数有没有原样到达"这一件事上**，而这件事只能观察：
//! 每个用例都真的生成一个 shim、真的把它当程序启动、再从目标的 `argv` 里读回真相。
//! 没有一个用例是在测"我自己以为的引号规则"。
//!
//! 目标一律是 `examples/argdump.rs`（把 `argv` 打成一行 JSON 的夹具），
//! 模板一律是刚构建出来的 `tuoen-shim.exe`（`CARGO_BIN_EXE_tuoen-shim`）。

#![cfg(windows)]

use std::ffi::OsStr;
use std::io::Write as _;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use tuoen_platform::test_support::TempDir;
use tuoen_shim::{
    EXIT_LAUNCH_FAILED, SLOT_MAGIC, ShimError, ShimOutcome, ShimSpec, decode_slot, write_shim,
};

// ─────────────────────────── 定位夹具与模板 ───────────────────────────

/// `target/<profile>`：从 `target/<profile>/deps/<crate>-<hash>.exe` 往上两级。
fn profile_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    exe.parent()
        .and_then(Path::parent)
        .expect("测试二进制应当在 target/<profile>/deps/ 下")
        .to_path_buf()
}

/// 一个 example 夹具。`cargo test` 会把 examples 一起构建出来。
fn fixture(name: &str) -> PathBuf {
    let path = profile_dir().join("examples").join(format!("{name}.exe"));
    assert!(
        path.is_file(),
        "找不到测试夹具 `{}`。`cargo test` 会构建 examples；\
         如果你只跑了 `cargo test --lib` 或 `--test <某个>`，它可能还没被构建。",
        path.display()
    );
    path
}

/// 模板 = 刚构建出来的 `tuoen-shim.exe`（**未烘过**的那一份）。
fn template() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tuoen-shim"))
}

fn spec(name: &str, target: &Path) -> ShimSpec {
    ShimSpec {
        name: name.to_owned(),
        target: target.to_path_buf(),
        prefix_args: Vec::new(),
    }
}

/// 生成一个 shim，返回 (路径, 生成结果)。
fn generate(dir: &TempDir, spec: &ShimSpec) -> (PathBuf, ShimOutcome) {
    let dest = dir.join(&spec.file_name());
    let outcome = write_shim(&template(), &dest, spec).expect("生成 shim");
    (dest, outcome)
}

// ─────────────────────────── 观察夹具 ───────────────────────────

/// 跑一个 shim，把 argdump 打出来的 `argv` 解析回来。
fn argv_of(program: &Path, args: &[&str]) -> Vec<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("启动 {} 失败：{e}", program.display()));
    parse_argv(&output.stdout, &output.stderr)
}

/// 直接把**原始命令行文本**贴在程序名后面（绕过 std 的引号化）。
fn argv_of_raw(program: &Path, raw: &str) -> Vec<String> {
    let mut command = Command::new(program);
    command.raw_arg(raw);
    let output = command.output().expect("启动");
    parse_argv(&output.stdout, &output.stderr)
}

fn parse_argv(stdout: &[u8], stderr: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(stdout);
    let line = text
        .lines()
        .find(|line| line.starts_with("ARGV "))
        .unwrap_or_else(|| {
            panic!(
                "目标没有打出 ARGV 行。stdout={text:?} stderr={:?}",
                String::from_utf8_lossy(stderr)
            )
        });
    serde_json::from_str(&line["ARGV ".len()..]).expect("argv JSON")
}

fn run(program: &Path, args: &[&str]) -> ExitStatus {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("启动")
}

/// 退出码取原始 DWORD。
///
/// Rust 的 `ExitStatus::code()` 在 Windows 上把 DWORD 当 `i32` 报出来，
/// 所以 `0xFFFFFFFF` 是 `Some(-1)`、`0xC0000005` 是 `Some(-1073741819)`。
fn raw_exit_code(status: ExitStatus) -> u32 {
    status.code().map_or_else(
        || panic!("ExitStatus::code() 给了 None —— 这是 std 的行为，不是 shim 的"),
        |code| code as u32,
    )
}

// ─────────────────────────── 模板本身 ───────────────────────────

#[test]
fn the_raw_template_refuses_to_run_and_says_what_it_is() {
    // 一个"看起来能跑但不知道目标"的二进制是危险的：它必须明确拒绝。
    let output = Command::new(template())
        .arg("--version")
        .output()
        .expect("启动");
    assert_eq!(raw_exit_code(output.status), EXIT_LAUNCH_FAILED as u32);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("模板"), "要自称模板：{stderr}");
    assert!(
        stderr.contains(tuoen_shim::TEMPLATE_MARKER),
        "要带上模板标记，便于对版本：{stderr}"
    );
    assert!(output.stdout.is_empty(), "模板不该往 stdout 写东西");
}

#[test]
fn every_slot_in_the_template_gets_baked() {
    // **本票最重要的一条回归测试。**
    //
    // 第一版的生成器是"找到第一处魔数就下手"，而 debug 模板里魔数出现了 4 次、
    // 只有 1 处是真的槽位 —— 也就是说它碰巧能跑。这条测试把那件事钉住：
    // 生成之后的文件里**一处原始槽位都不能剩**，而且原来每一处都要能解出新前缀。
    let bytes = std::fs::read(template()).expect("读模板");
    let before = tuoen_shim::pristine_slot_offsets(&bytes);
    assert!(
        !before.is_empty(),
        "模板里应当至少有一处未烘过的槽位（找过的字节数：{}）",
        bytes.len()
    );

    let dir = TempDir::new("shim-bake-all");
    let (shim, outcome) = generate(&dir, &spec("argdump", &fixture("argdump")));
    assert_eq!(
        outcome.slot_count,
        before.len(),
        "报告改写的处数必须等于模板里的处数"
    );

    let after = std::fs::read(&shim).expect("读 shim");
    assert!(
        tuoen_shim::pristine_slot_offsets(&after).is_empty(),
        "生成之后不该剩下任何未烘过的槽位 —— 剩下的那一处才是运行时真正读的"
    );
    for at in before {
        assert_eq!(
            decode_slot(&after[at..]).as_deref(),
            Some(outcome.prefix.as_str()),
            "偏移 {at} 的槽位没被改写"
        );
    }
}

#[test]
fn the_template_contains_the_slot_magic() {
    let bytes = std::fs::read(template()).expect("读模板");
    let hits = bytes
        .windows(SLOT_MAGIC.len())
        .filter(|window| *window == SLOT_MAGIC)
        .count();
    assert!(hits >= 1, "模板里应当有槽位魔数");
    // 注意：**不能**断言 `hits == 1`。本机 debug 模板实测是 4 —— 其余几处是
    // 编译产物里的常量，它们后面第 16 个字节不是 0，所以不是槽位。
    // 判据是"整段等于 baked_slot()"，不是"魔数只出现一次"。
    assert_eq!(decode_slot(&bytes[..]), None, "整体不该被当成一个槽位");
}

// ─────────────────────────── 参数转发 ───────────────────────────

#[test]
fn arguments_reach_the_target_verbatim() {
    // 这一张表就是本票的核心断言。每一条都是"引号/反斜杠/空白"的一个边界。
    let table: &[&[&str]] = &[
        &[],
        &["-a"],
        &["-a", "-b"],
        &["a b"],
        &[""],
        &["he said \"hi\""],
        &["--flag=value"],
        &[r"C:\path with space\"],
        &["a b\\"],
        &["\\"],
        &["-Dfoo=bar baz"],
        &["中文 参数", "假"],
        &["a\tb"],
        &["\""],
        &["x\"y"],
        &["trailing ", " leading"],
        &["a", "a b", "", "he said \"hi\"", r"c:\d e\", "\\"],
    ];
    let dir = TempDir::new("shim-argv");
    let (shim, _) = generate(&dir, &spec("argdump", &fixture("argdump")));
    for args in table {
        let seen = argv_of(&shim, args);
        assert_eq!(
            &seen[1..],
            *args,
            "参数没原样到达。命令行里的样子是 {:?}",
            args
        );
    }
}

#[test]
fn raw_command_lines_are_sliced_like_the_crt_would() {
    // `raw_arg` 把文本**原样**贴上去，模拟"调用者自己拼命令行"（cmd.exe、构建脚本、
    // 编辑器都是这么干的）。下面的期望值来自**实测**（`docs/acceptance/L0-07-shim.md` §3）。
    let dir = TempDir::new("shim-raw");
    let (shim, _) = generate(&dir, &spec("argdump", &fixture("argdump")));

    let cases: &[(&str, &[&str])] = &[
        (" -a -b", &["-a", "-b"]),
        (r#" "" x"#, &["", "x"]),
        (r#" "a b" c"#, &["a b", "c"]),
        (r#" "he said \"hi\""#, &[r#"he said "hi""#]),
        (" --flag=value", &["--flag=value"]),
        (r#" "C:\path with space\\""#, &[r"C:\path with space\"]),
        (r#" "a b\\" c"#, &[r"a b\", "c"]),
        (r#" \\"#, &["\\\\"]),
        ("\t-a", &["-a"]),
        (r#" -Dfoo="bar baz""#, &["-Dfoo=bar baz"]),
        (r#" "中文 参数" 假"#, &["中文 参数", "假"]),
    ];
    for (raw, expected) in cases {
        let seen = argv_of_raw(&shim, raw);
        assert_eq!(&seen[1..], *expected, "原文 `{raw}` 切出来不对");
    }
}

#[test]
fn baked_prefix_arguments_come_first_and_exactly() {
    let dir = TempDir::new("shim-prefix");
    let full = ShimSpec {
        name: "npm".to_owned(),
        target: fixture("argdump"),
        prefix_args: vec!["--prefix one".to_owned(), "--two".to_owned(), String::new()],
    };
    let (shim, outcome) = generate(&dir, &full);
    assert_eq!(shim.file_name().unwrap(), OsStr::new("npm.exe"));
    assert!(
        outcome.prefix.contains("--prefix one"),
        "前缀必须先引号化再烘进去：{}",
        outcome.prefix
    );
    let seen = argv_of(&shim, &["--caller", "a b"]);
    assert_eq!(
        &seen[1..],
        &["--prefix one", "--two", "", "--caller", "a b"],
        "前缀参数必须原样排在调用者参数之前"
    );
}

#[test]
fn the_empty_argument_case_survives_because_we_never_requote() {
    // 空参数是"重新引号化 argv"最典型的牺牲品：它会在某一层被悄悄吃掉。
    let dir = TempDir::new("shim-empty");
    let (shim, _) = generate(&dir, &spec("argdump", &fixture("argdump")));
    let seen = argv_of(&shim, &["", "x", ""]);
    assert_eq!(&seen[1..], &["", "x", ""]);
}

// ─────────────────────────── 退出码 ───────────────────────────

#[test]
fn exit_codes_are_forwarded_including_the_all_ones_code() {
    let dir = TempDir::new("shim-exit");
    let (shim, _) = generate(&dir, &spec("argdump", &fixture("argdump")));
    for code in [0u32, 1, 3, 42, 255, 0xFFFF_FFFF] {
        let spec_arg = format!("{code:#010X}");
        let status = run(&shim, &["--exit", &spec_arg]);
        assert_eq!(raw_exit_code(status), code, "退出码 {code:#010X} 没透传");
    }
}

#[test]
fn exception_codes_are_forwarded_bit_for_bit() {
    // `0xC0000005` 不是"返回了一个负数"，它是内核写进进程对象的异常码。
    // 转发器如果只转发 `main` 的返回值，这里就会变成一个别的数字。
    let dir = TempDir::new("shim-exc");
    let (shim, _) = generate(&dir, &spec("argdump", &fixture("argdump")));
    let status = run(&shim, &["--crash"]);
    assert_eq!(raw_exit_code(status), 0xC000_0005);
}

#[test]
fn a_shim_for_a_vanished_target_fails_loudly_instead_of_pretending() {
    // **注意这里两个名字必须不同。** 第一版我把它们写成了同一个文件，
    // 于是生成出来的 `gone.exe` 目标是它自己 —— 跑一次就无限自我启动，
    // 本机堆到 20580 个进程把内存吃干。这不是"测试写错了"那么简单：
    // 它证明了生成器缺一道守卫，所以 `write_shim` 现在有 `DestinationIsTarget`，
    // 转发器运行时还有一道自指保险丝。
    let dir = TempDir::new("shim-vanish");
    let target = dir.join("payload.exe");
    std::fs::copy(fixture("argdump"), &target).expect("复制一份当目标");
    let (shim, _) = generate(&dir, &spec("tool", &target));
    assert_eq!(argv_of(&shim, &["-a"]).len(), 2, "先确认它能跑");

    std::fs::remove_file(&target).expect("删掉目标");
    let output = Command::new(&shim).arg("-a").output().expect("启动");
    assert_eq!(raw_exit_code(output.status), EXIT_LAUNCH_FAILED as u32);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("无法启动目标程序"), "{stderr}");
    assert!(stderr.contains("payload.exe"), "要报出目标路径：{stderr}");
    assert!(stderr.contains("tuoen list"), "要给出下一步：{stderr}");
}

#[test]
fn a_self_referencing_shim_cannot_be_generated() {
    // 事故的回归测试。这里的落盘路径与目标路径是**同一个文件**。
    let dir = TempDir::new("shim-self");
    let same = dir.join("self.exe");
    std::fs::copy(fixture("argdump"), &same).expect("复制");
    let error = write_shim(&template(), &same, &spec("self", &same)).unwrap_err();
    assert_eq!(error.kind(), "dest-is-target", "{error}");
    let text = error.to_string();
    assert!(
        text.contains("无限自我启动"),
        "要说出后果，而不只是规则：{text}"
    );
    // 目标要原封不动 —— 被拒之后那个文件的字节一个都不能变。
    assert_eq!(
        std::fs::read(&same).expect("读"),
        std::fs::read(fixture("argdump")).expect("读夹具")
    );
}

#[test]
fn a_self_referencing_shim_is_also_stopped_at_runtime() {
    // 生成时那道守卫防的是"我们生成出来的"；这一道防的是"字节被改过"或
    // "旧版本生成的"。做法：手工把一个烘好的 shim 的槽位改成指向它自己。
    let dir = TempDir::new("shim-self-runtime");
    let presentation = dir.join("presentation.exe");
    let (shim, _) = generate(&dir, &spec("tool", &fixture("argdump")));
    std::fs::copy(&shim, &presentation).expect("复制一份出来单独改");

    // 直接把槽位的前缀改成它自己的路径 —— 绕过所有生成时守卫。
    let mut bytes = std::fs::read(&presentation).expect("读");
    let at = bytes
        .windows(SLOT_MAGIC.len())
        .position(|w| w == SLOT_MAGIC)
        .expect("槽位");
    let baked = tuoen_shim::encode_slot(&presentation.to_string_lossy()).expect("编码");
    bytes[at..at + baked.len()].copy_from_slice(&baked);
    std::fs::write(&presentation, &bytes).expect("写回");

    let output = Command::new(&presentation)
        .arg("-a")
        .output()
        .expect("启动");
    assert_eq!(raw_exit_code(output.status), EXIT_LAUNCH_FAILED as u32);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("目标是它自己"), "{stderr}");
    assert!(
        stderr.contains("tuoen shim add"),
        "要给出修复办法：{stderr}"
    );
}

#[test]
fn a_foreign_file_in_the_shim_directory_is_not_overwritten() {
    let dir = TempDir::new("shim-foreign-dest");
    let target = dir.join("real.exe");
    std::fs::copy(fixture("argdump"), &target).expect("复制");
    let dest = dir.join("node.exe");
    std::fs::write(&dest, b"MZ not ours").expect("写一个别人的文件");

    let error = write_shim(&template(), &dest, &spec("node", &target)).unwrap_err();
    assert_eq!(error.kind(), "dest-not-a-shim", "{error}");
    assert_eq!(
        std::fs::read(&dest).expect("读"),
        b"MZ not ours",
        "不能动它"
    );
    assert!(error.is_user_error());
}

// ─────────────────────────── 句柄继承 ───────────────────────────

#[test]
fn stdio_pipes_work_through_the_shim() {
    // 转发器最容易漏的一条：`bInheritHandles` 不开、或者设了 `STARTF_USESTDHANDLES`
    // 却没把句柄传下去，于是"管道里什么都没有"。
    let dir = TempDir::new("shim-stdio");
    let (shim, _) = generate(&dir, &spec("argdump", &fixture("argdump")));
    let mut child = Command::new(&shim)
        .arg("--mirror-stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"hello through two levels")
        .expect("写 stdin");
    let output = child.wait_with_output().expect("等它结束");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "STDOUT:hello through two levels"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("STDERR:reached"));
    assert_eq!(raw_exit_code(output.status), 0);
}

// ─────────────────────────── 生成器 ───────────────────────────

#[test]
fn the_generated_shim_carries_its_target_in_its_own_bytes() {
    // "烘进去"的意思就是：这个 shim 的字节里有目标，而且**没有边车文件**。
    let dir = TempDir::new("shim-baked");
    let target = fixture("argdump");
    let (shim, outcome) = generate(&dir, &spec("argdump", &target));

    let bytes = std::fs::read(&shim).expect("读 shim");
    let at = bytes
        .windows(SLOT_MAGIC.len())
        .position(|window| window == SLOT_MAGIC)
        .expect("shim 里应当有槽位");
    assert_eq!(
        decode_slot(&bytes[at..]).as_deref(),
        Some(outcome.prefix.as_str())
    );
    assert!(outcome.prefix.contains("argdump.exe"));
    assert!(!outcome.replaced, "第一次生成不是覆盖");

    let leftovers: Vec<String> = std::fs::read_dir(dir.path())
        .expect("读目录")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "argdump.exe")
        .collect();
    assert!(leftovers.is_empty(), "不该留下临时文件：{leftovers:?}");
}

#[test]
fn regenerating_replaces_the_shim_and_the_new_one_runs() {
    let dir = TempDir::new("shim-regen");
    let first = dir.join("first.exe");
    let second = dir.join("second.exe");
    std::fs::copy(fixture("argdump"), &first).expect("复制");
    std::fs::copy(fixture("argdump"), &second).expect("复制");

    let (shim, outcome) = generate(&dir, &spec("tool", &first));
    assert!(!outcome.replaced);
    assert!(argv_of(&shim, &["-a"])[0].ends_with("first.exe"));

    let outcome = write_shim(&template(), &shim, &spec("tool", &second)).expect("重新生成");
    assert!(outcome.replaced, "第二次生成应当是覆盖");
    let seen = argv_of(&shim, &["-a"]);
    assert!(
        seen[0].ends_with("second.exe"),
        "覆盖之后必须指向新目标，实际：{}",
        seen[0]
    );
}

#[test]
fn refusals_are_specific_and_happen_before_anything_is_written() {
    let dir = TempDir::new("shim-refuse");
    let exists = dir.join("real.exe");
    std::fs::copy(fixture("argdump"), &exists).expect("复制");

    let cases: Vec<(&str, ShimSpec, &str)> = vec![
        ("脚本目标", spec("x", &dir.join("npm.cmd")), "script-target"),
        (
            "相对路径目标",
            spec("x", Path::new(r"node.exe")),
            "target-not-absolute",
        ),
        (
            "不存在的目标",
            spec("x", &dir.join("nope.exe")),
            "target-unreadable",
        ),
        ("目标是目录", spec("x", dir.path()), "target-not-a-file"),
        ("名字里有分隔符", spec("a/b", &exists), "bad-name"),
        ("名字是保留设备名", spec("con", &exists), "bad-name"),
        (
            "前缀放不下",
            ShimSpec {
                name: "x".to_owned(),
                target: exists.clone(),
                prefix_args: vec!["a".repeat(4096)],
            },
            "prefix-too-long",
        ),
    ];
    for (label, bad, expected_kind) in cases {
        let dest = dir.join("should-not-exist.exe");
        let error = write_shim(&template(), &dest, &bad).unwrap_err();
        assert_eq!(error.kind(), expected_kind, "用例：{label} → {error}");
        assert!(!dest.exists(), "被拒之后不该留下文件（{label}）");
    }
}

#[test]
fn a_foreign_file_is_not_accepted_as_a_template() {
    let dir = TempDir::new("shim-foreign");
    let target = dir.join("real.exe");
    std::fs::copy(fixture("argdump"), &target).expect("复制");
    // 拿夹具自己（一个真 `.exe`，但不是我们的模板）当模板。
    let error = write_shim(&target, &dir.join("x.exe"), &spec("x", &target)).unwrap_err();
    assert_eq!(error.kind(), "template-not-recognized", "{error}");
    let text = error.to_string();
    assert!(
        text.contains("tuoen-shim.exe"),
        "要说出正确的模板叫什么：{text}"
    );
}

#[test]
fn the_script_refusal_explains_the_mechanism() {
    // 只说"不支持 .cmd"会让人以为是我们偷懒。文档里那条实测结论必须出现在错误里。
    let dir = TempDir::new("shim-script");
    let error = write_shim(
        &template(),
        &dir.join("npm.exe"),
        &spec("npm", &dir.join("npm.cmd")),
    )
    .unwrap_err();
    assert!(matches!(error, ShimError::ScriptTarget { .. }));
    assert!(error.is_user_error());
    let text = error.to_string();
    assert!(text.contains("COMSPEC"), "{text}");
    assert!(text.contains("引号"), "{text}");
    assert!(text.contains("前缀参数"), "要给出可行的替代方案：{text}");
}
