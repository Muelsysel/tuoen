//! `tuoen globals list` 的**契约测试**（ticket #23）。
//!
//! # 这一票的两条硬约束怎么落在这里
//!
//! 1. **测试里绝不出现网络**：`PATH` 只有"一个临时 `bin` + `%SystemRoot%` + `System32`"，
//!    真正的 `npm` / `pip` / `node` 一个都问不到。要测"工具答了什么"，就在临时 `bin`
//!    里放一个只 `echo` 一份 canned JSON 的假 `npm.cmd` —— 重定向的是**子进程环境**，
//!    产品代码一行都不用知道自己在测试里（不是给厂二进制留开关）。
//! 2. **绝不写真实的 `%LOCALAPPDATA%\tuoen\globals`**：每一处都用 [`IsolatedHome`]，
//!    `LOCALAPPDATA` 指向临时树，于是我们自己的两个根都落在临时树里。
//!
//! # `node.exe` / `pip.exe` 为什么是系统 PE 的副本
//!
//! npm 那个根写在**版本目录名**里（`…\npm\<node -v 原样>`），所以要测"根算得出来"
//! 就必须有一个名叫 `node.exe`、**答得上一句话且退出码为 0** 的真 PE。
//! 测试绝不能依赖开发机装没装 node，所以这里用系统自带 `cmd.exe` 的副本：
//! 它被当成别的名字调用时会印一行 banner 并**退出 0**（实测）。两个工具各要一件事：
//!
//! * `node.exe` 要的是"**有**答案" → 根算得出来，版本目录名就是那行 banner 的原样；
//! * `pip.exe` 要的是"答案**不是** `(python X.Y)`" → `tool_version = unknown`、
//!   前缀不出键，而**根照旧出现**（pip 的根与版本无关）。
//!
//! 断言因此是**结构性**的（"`<base>\npm\` 后面恰好一段"），不是逐字比 banner ——
//! 逐字比就把开发机的系统语言写进契约里了（`AGENTS.md` 规矩五）。
//!
//! # `--json` 的键序
//!
//! 信封把 `data` 过了一遍 `serde_json::Value`，于是键是**字母序**的。这是全仓库
//! 所有命令的共同行为，所以这里的断言只钉**有哪些键、值是什么**，
//! 不钉顺序（顺序不属于冻结形状的一部分）。

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{IsolatedHome, TempDir, json, stderr, stdout};
use serde_json::Value;

/// 机器自己那一份的 npm 前缀（假的，但**真的在磁盘上**：`binNames` 要读它）。
const MACHINE_PACKAGES: &str = r#"{"name":"lib","dependencies":{"corepack":{"version":"0.35.0"},"pnpm":{"version":"11.21.0"}}}"#;
/// `bin` 是**字符串**的包 —— npm 的约定：命令名 = 包名（去掉 scope）。
const COREPACK_MANIFEST: &str = r#"{"name":"corepack","bin":"internal/corepack.js"}"#;
/// `bin` 是**对象**的包 —— 键就是命令名（本机 `pnpm` 实测四个）。
const PNPM_MANIFEST: &str =
    r#"{"name":"pnpm","version":"11.21.0","bin":{"pnpm":"bin/pnpm.mjs","pnx":"bin/pnpx.mjs"}}"#;

/// 一个"两个工具都在 `PATH` 上"的隔离夹具。
struct Fixture {
    home: IsolatedHome,
    /// 临时 `bin`：假 `npm.cmd` + 冒充 `node.exe` / `pip.exe` 的系统 PE 副本。
    bin: TempDir,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let home = IsolatedHome::new(label);
        let bin = TempDir::new(&format!("globals-bin-{label}"));
        let npm_prefix = home.local_app_data().join("machine-npm-prefix");

        // 机器那一份 npm 前缀里的两个包 —— 各带一份 `package.json`（binNames 的来源）。
        write_package(&npm_prefix, "pnpm", PNPM_MANIFEST);
        write_package(&npm_prefix, "corepack", COREPACK_MANIFEST);

        bin.write("npm.cmd", &npm_script(&npm_prefix));
        copy_system_pe(&bin.path().join("node.exe"));
        copy_system_pe(&bin.path().join("pip.exe"));

        Self { home, bin }
    }

    /// 我们自己的根（`<LOCALAPPDATA>\tuoen\globals`）—— 在测试里就是临时树里的那个。
    fn globals_base(&self) -> PathBuf {
        self.home.local_app_data().join("tuoen").join("globals")
    }

    fn command(&self) -> Command {
        let system_root = std::env::var_os("SystemRoot").expect("SystemRoot 必须存在");
        let mut path = OsString::from(self.bin.path().as_os_str());
        path.push(";");
        path.push(&system_root);
        path.push("\\System32");

        let mut command = self.home.command();
        command
            .env("USERPROFILE", self.home.local_app_data().join("home"))
            .env("HOME", self.home.local_app_data().join("home"))
            // **清掉**开发机可能有的 npm/pip 重定向变量：它们会把这台"假机器"的答案
            // 换成开发机自己的（而且我们的重定向会被继承进来的值悄悄盖掉）。
            .env_remove("NPM_CONFIG_PREFIX")
            .env_remove("PYTHONUSERBASE")
            .env_remove("PIP_USER")
            .env("PATH", path);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .output()
            .expect("运行 tuoen 应当成功")
    }
}

/// 往 `<prefix>\node_modules\<name>\package.json` 里写一份清单。
fn write_package(prefix: &Path, name: &str, manifest: &str) {
    let dir = prefix.join("node_modules").join(name);
    std::fs::create_dir_all(&dir).expect("建包目录");
    std::fs::write(dir.join("package.json"), manifest).expect("写 package.json");
}

/// 把系统 `cmd.exe` 复制成 `destination` —— 一个"答得上一句话、退出 0"的真 PE。
///
/// 为什么不是 `python.exe`（本机 `where python` 的第一条是 0 字节的 App Execution
/// Alias）：Alias **不是一个可执行文件**，`find_on_path` 会跳过它，而且执行它会
/// 打开应用商店。这正是本票要求"pip 必须用 `pip.exe`"的那条真机教训。
fn copy_system_pe(destination: &Path) {
    let cmd = Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot"))
        .join("System32")
        .join("cmd.exe");
    std::fs::copy(&cmd, destination).unwrap_or_else(|err| {
        panic!(
            "复制 {} → {} 失败：{err}",
            cmd.display(),
            destination.display()
        )
    });
}

/// 一个只 `echo` 的假 `npm.cmd`（照抄 `capture_contract.rs` 的手法）。
///
/// 它按**第一个参数**分岔：`config get prefix` 答前缀，`ls` 答包清单 —— 其余一律
/// 非零退出。于是"机器那一问"与"我们那一问"（带 `--prefix` 的那条）看起来是同一个
/// 答案，而那正是我们要的：**两个来源的差别只在 `source` 上**，不在工具的回答上。
fn npm_script(prefix: &Path) -> String {
    format!(
        "@echo off\r\n\
         if \"%~1\"==\"config\" (\r\n\
         \x20 echo {prefix}\r\n\
         \x20 exit /b 0\r\n\
         )\r\n\
         if \"%~1\"==\"ls\" (\r\n\
         \x20 echo {packages}\r\n\
         \x20 exit /b 0\r\n\
         )\r\n\
         echo npm.cmd: unexpected arguments 1>&2\r\n\
         exit /b 1\r\n",
        prefix = prefix.display(),
        packages = MACHINE_PACKAGES
    )
}

/// 从 `packages` 数组里挑一行（按来源 + 包名）—— 两个来源都要按来源挑。
fn package<'a>(packages: &'a [Value], source: &str, name: &str) -> &'a Value {
    packages
        .iter()
        .find(|row| row["source"] == source && row["name"] == name)
        .unwrap_or_else(|| panic!("没有 {source}/{name}：{packages:?}"))
}

/// 一个对象的键（**排序后**；信封把 `data` 过了一遍 `Value`，键本来就是字母序的）。
fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap_or_else(|| panic!("不是一个对象：{value}"))
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

/// 除 `roots[].root` 之外，载荷里**我们自己写的每一个字符串都是 ASCII**。
///
/// 根那条路径是**机器给的**（`node -v` 的原样输出 + 用户的 `LOCALAPPDATA`），
/// 它可以含中文（用户名是中文时就会）—— 所以判据只覆盖我们自己的词汇。
fn assert_our_values_are_ascii(data: &Value) {
    let mut checked = Vec::new();
    for row in data["packages"].as_array().expect("packages 是数组") {
        for key in ["tool", "name", "version", "source"] {
            checked.push(row[key].as_str().expect("字符串").to_owned());
        }
        if let Some(names) = row.get("binNames") {
            for name in names.as_array().expect("binNames 是数组") {
                checked.push(name.as_str().expect("字符串").to_owned());
            }
        }
    }
    for row in data["roots"].as_array().expect("roots 是数组") {
        for key in ["tool", "source"] {
            checked.push(row[key].as_str().expect("字符串").to_owned());
        }
    }
    for value in &checked {
        assert!(
            value.is_ascii(),
            "这个值不是 ASCII（成功载荷不许本地化）：{value}"
        );
    }
}

/// 冻结的 `--json` 形状：两个根（**空根也在**）、两个来源的包、每个包带 `binNames`。
#[test]
fn the_json_shape_is_frozen_and_an_empty_root_is_still_listed() {
    let fixture = Fixture::new("shape");
    let output = fixture.run(&["globals", "list", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    // 单行 JSON：stdout 上没有第二个写者。
    assert_eq!(stdout(&output).trim().lines().count(), 1);

    let envelope = json(&output);
    assert!(envelope.ok, "{:?}", envelope.error);
    assert_eq!(envelope.command, "globals.list");
    assert_eq!(envelope.schema_version, 2);
    let data = envelope.data.expect("成功载荷有 data");

    // ---- roots：两个根，而且**还没有**的那两个也在 ----
    let roots = data["roots"].as_array().expect("roots 是数组");
    assert_eq!(roots.len(), 2, "{roots:?}");
    assert_eq!(
        keys(&roots[0]),
        ["root", "source", "tool"],
        "形状冻结：三键"
    );
    assert_eq!(roots[0]["tool"], "npm");
    assert_eq!(roots[0]["source"], "tuoen");
    assert_eq!(roots[1]["tool"], "pip");
    assert_eq!(roots[1]["source"], "tuoen");
    assert_eq!(
        roots[1]["root"].as_str().expect("字符串"),
        fixture.globals_base().join("pip").to_string_lossy(),
        "pip 的根与版本无关（`Python312\\` 那一层是 pip 自己插的）"
    );

    // npm 的根 = `<base>\npm\<node -v 的原样输出>`：后面**恰好一段**、非空、不是 `unknown`。
    let npm_root = roots[0]["root"].as_str().expect("字符串");
    let head = format!("{}\\npm\\", fixture.globals_base().display());
    let version_dir = npm_root
        .strip_prefix(&head)
        .unwrap_or_else(|| panic!("npm 的根必须在 `{head}` 下面，实际是 `{npm_root}`"));
    assert!(!version_dir.is_empty());
    assert!(
        !version_dir.contains('\\'),
        "版本目录名是**一段**：{version_dir}"
    );
    assert_ne!(version_dir, "unknown", "答不出 `node -v` 时压根不该有根");
    // **这一遍它是空的**（我们没建那个目录）—— 但根照样在清单里。

    // ---- packages：机器那一份（两个包，各带 binNames）----
    let packages = data["packages"].as_array().expect("packages 是数组");
    assert_eq!(packages.len(), 2, "{packages:?}");
    assert!(
        packages.iter().all(|row| row["source"] == "machine"),
        "我们的根还不存在 ⇒ 一个 tuoen 包都没有：{packages:?}"
    );

    let corepack = package(packages, "machine", "corepack");
    assert_eq!(
        keys(corepack),
        ["binNames", "name", "source", "tool", "version"]
    );
    assert_eq!(corepack["version"], "0.35.0");
    assert_eq!(corepack["tool"], "npm");
    assert_eq!(
        corepack["binNames"],
        serde_json::json!(["corepack"]),
        "`bin` 是字符串 ⇒ 命令名 = 包名"
    );

    let pnpm = package(packages, "machine", "pnpm");
    assert_eq!(pnpm["version"], "11.21.0");
    assert_eq!(
        pnpm["binNames"],
        serde_json::json!(["pnpm", "pnx"]),
        "`bin` 是对象 ⇒ 键就是命令名（排序后）"
    );

    assert_our_values_are_ascii(&data);
}

/// 我们的根**存在之后**：同一个工具长出**第二行**，来源是 `tuoen`。
///
/// 两遍跑法是刻意的：第一遍我们才能从 `roots` 里学到那个根到底叫什么
/// （版本目录名是 `node -v` 的原样输出，测试不许硬编码它）。
#[test]
fn the_tuoen_source_appears_once_its_root_exists() {
    let fixture = Fixture::new("two-sources");

    let first = fixture.run(&["globals", "list", "--json"]);
    let data = json(&first).data.expect("data");
    let npm_root = PathBuf::from(data["roots"][0]["root"].as_str().expect("字符串"));
    assert!(
        !npm_root.exists(),
        "这一遍它还不该存在：{}",
        npm_root.display()
    );

    // 造出那个根，并在里面放**一个**包（另一个故意不放 package.json）。
    std::fs::create_dir_all(&npm_root).expect("建我们的 npm 根");
    write_package(
        &npm_root,
        "corepack",
        r#"{"name":"corepack","bin":{"corepack":"lib/corepack.js"}}"#,
    );

    let second = fixture.run(&["globals", "list", "--json"]);
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    let data = json(&second).data.expect("data");
    let packages = data["packages"].as_array().expect("packages");
    assert_eq!(packages.len(), 4, "两个来源各两个包：{packages:?}");

    // 同一个包名在两边各有一行 —— **那不是重复**，是两套安装。
    let machine = package(packages, "machine", "corepack");
    let tuoen = package(packages, "tuoen", "corepack");
    assert_eq!(machine["tool"], "npm");
    assert_eq!(tuoen["tool"], "npm");
    assert_eq!(machine["version"], tuoen["version"], "版本一样，来源不一样");

    // binNames：tuoen 那一行从**我们根里的**那份 `package.json` 读。
    assert_eq!(tuoen["binNames"], serde_json::json!(["corepack"]));
    assert_eq!(
        machine["binNames"],
        serde_json::json!(["corepack"]),
        "两份清单各读自己那个根里的 manifest"
    );
    // 拿不到就**整键消失**（不是 `[]`）：tuoen 那一侧我们没给 pnpm 放 manifest。
    let tuoen_pnpm = package(packages, "tuoen", "pnpm");
    assert!(
        tuoen_pnpm.get("binNames").is_none(),
        "拿不到命令名时 `binNames` 键整个不出现：{tuoen_pnpm}"
    );
    assert_eq!(keys(tuoen_pnpm), ["name", "source", "tool", "version"]);

    assert_our_values_are_ascii(&data);
}

/// 人类输出：中文、六列、两个来源、空根说出"还没有"。
#[test]
fn the_human_table_is_chinese_and_names_both_sources() {
    let fixture = Fixture::new("human");
    let output = fixture.run(&["globals", "list"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);

    assert!(
        !text.trim_start().starts_with('{'),
        "人类输出不是 JSON：{text}"
    );
    // 中文：人类输出必须是中文（`--json` 才不许本地化）。
    assert!(
        text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
        "人类输出里必须有中文：{text}"
    );
    // 根那一段：两个根都印，空的那个说"还没有"。
    assert!(text.contains("tuoen 管着的根"), "{text}");
    assert!(
        text.contains(
            &fixture
                .globals_base()
                .join("pip")
                .to_string_lossy()
                .to_string()
        ),
        "pip 的根（空的）也要印出来：{text}"
    );
    assert!(text.contains("还没有"), "空根说的是「还没有」：{text}");
    // 表头六列。
    for column in ["工具", "运行时版本", "来源", "包名", "版本", "bin 名"] {
        assert!(text.contains(column), "表头缺 `{column}`：{text}");
    }
    // 两个来源的 slug 都要出现在表里（脚本与人的表用同一套词）。
    assert!(text.contains("machine") && text.contains("tuoen"), "{text}");
    // 包名与命令名。
    assert!(text.contains("pnpm") && text.contains("pnx"), "{text}");
    assert!(text.contains("共 2 个包"), "{text}");
}

/// 工具不在 `PATH` 上：**不是错误**，但必须说出为什么它一行都没有。
#[test]
fn a_tool_that_is_not_on_path_is_named_in_the_human_output() {
    let fixture = Fixture::new("no-pip");
    std::fs::remove_file(fixture.bin.path().join("pip.exe")).expect("撤掉 pip.exe");

    let output = fixture.run(&["globals", "list", "--json"]);
    assert_eq!(output.status.code(), Some(0), "工具不在 PATH 上不是错误");
    let data = json(&output).data.expect("data");
    let roots: Vec<&str> = data["roots"]
        .as_array()
        .expect("roots")
        .iter()
        .map(|row| row["tool"].as_str().expect("字符串"))
        .collect();
    assert_eq!(roots, ["npm"], "pip 不在 ⇒ 连它的根都不产生（决策 171）");

    let human = stdout(&fixture.run(&["globals", "list"]));
    assert!(human.contains("pip"), "{human}");
    assert!(
        human.contains("不在 `PATH` 上"),
        "为什么它一行都没有必须说出来：{human}"
    );
    assert!(human.contains("决策 171"), "{human}");
}

/// 进程环境里没有 `LOCALAPPDATA`：**退出码 1 + 稳定 slug**，不是"半张表"。
#[test]
fn without_local_app_data_the_command_fails_with_the_unwired_root_slug() {
    let fixture = Fixture::new("no-localappdata");

    let mut command = fixture.command();
    command.env_remove("LOCALAPPDATA");
    let output = command
        .args(["globals", "list", "--json"])
        .output()
        .expect("运行 tuoen 应当成功");
    assert_eq!(output.status.code(), Some(1), "{}", stdout(&output));
    let envelope = json(&output);
    assert!(!envelope.ok);
    assert!(
        envelope.data.is_none(),
        "失败载荷不许带 data：{:?}",
        envelope.data
    );
    let error = envelope.error.expect("有 error");
    assert_eq!(error.code, "unwired-root");
    assert!(
        error.message.contains("LOCALAPPDATA"),
        "错误必须点名那个变量：{}",
        error.message
    );

    // 人类那一版：错误进 **stderr**（stdout 上没有第二个写者）。
    let mut command = fixture.command();
    command.env_remove("LOCALAPPDATA");
    let output = command
        .args(["globals", "list"])
        .output()
        .expect("运行 tuoen 应当成功");
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    assert!(
        stderr(&output).contains("LOCALAPPDATA"),
        "{}",
        stderr(&output)
    );
}

/// 用法：不带子命令是**错误**（这一族以后会长出会写磁盘的 `add`/`remove`），
/// 而 `--help` 里要看得见 `list`。
#[test]
fn the_family_needs_a_subcommand_and_the_help_lists_it() {
    let fixture = Fixture::new("usage");

    let bare = fixture.run(&["globals"]);
    assert_eq!(
        bare.status.code(),
        Some(2),
        "不是默认列出：{}",
        stdout(&bare)
    );
    assert!(stderr(&bare).contains("list"), "{}", stderr(&bare));

    let unknown = fixture.run(&["globals", "add", "pnpm"]);
    assert_eq!(unknown.status.code(), Some(2), "这一票没有 add");

    let help = fixture.run(&["globals", "list", "--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout(&help).contains("--json"), "{}", stdout(&help));
}

/// `capture --only globals` 的**新键**（ticket #23）：行要带来源，而且那个
/// "落在按版本隔离目录里"的计数要按来源拆开。
///
/// 这一条同时钉住 `globals.toml` 里的 `source` 字段 —— 它是决策 166 的**加法**
/// 变更（`schemaVersion` 不动，所以这里顺带断言它还是 1）。
#[test]
fn capture_reports_the_source_of_every_globals_row() {
    let fixture = Fixture::new("capture");
    let out = TempDir::new("globals-capture-out");
    let out_dir = out.path().to_string_lossy().into_owned();

    // 第一遍：我们的根还不存在 ⇒ 两行都是机器自己的（pip 也在 PATH 上，只是零个包）。
    let first = fixture.run(&["capture", "--only", "globals", "--out", &out_dir, "--json"]);
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let envelope = json(&first);
    assert_eq!(
        envelope.schema_version, 2,
        "加法变更**不**递增格式版本（2 来自决策 189 的那次删除）"
    );
    let globals = envelope.data.expect("data")["globals"].clone();
    let by_tool = globals["byTool"].as_array().expect("byTool");
    assert_eq!(
        by_tool
            .iter()
            .map(|row| (
                row["tool"].as_str().unwrap(),
                row["source"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        [("npm", "machine"), ("pip", "machine")],
        "两个工具各一行，来源都是 machine"
    );
    let npm = by_tool
        .iter()
        .find(|row| row["tool"] == "npm")
        .expect("npm 行");
    assert_eq!(npm["packages"], 2);
    assert_eq!(
        globals["insideVersionDirMachine"].as_u64(),
        Some(0),
        "机器那一份的假前缀里没有版本段（那个键必须是个数字）"
    );
    assert_eq!(globals["insideVersionDir"].as_u64(), Some(0));

    // 落盘的那一份：`source` **永远出键**。
    let toml = std::fs::read_to_string(out.path().join("globals.toml")).expect("globals.toml");
    assert!(toml.contains(r#"source = "machine""#), "{toml}");
    assert_eq!(toml.matches("[[global]]").count(), 2, "{toml}");

    // 造出我们的 npm 根 ⇒ 第二遍应出现 `source = "tuoen"` 的第二行（同一个工具的**第二行**）。
    let npm_root = PathBuf::from(
        json(&fixture.run(&["globals", "list", "--json"]))
            .data
            .expect("data")["roots"][0]["root"]
            .as_str()
            .expect("字符串"),
    );
    std::fs::create_dir_all(&npm_root).expect("建我们的 npm 根");

    let second = fixture.run(&["capture", "--only", "globals", "--out", &out_dir, "--json"]);
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    let globals = json(&second).data.expect("data")["globals"].clone();
    let by_tool = globals["byTool"].as_array().expect("byTool");
    assert_eq!(
        by_tool
            .iter()
            .map(|row| (
                row["tool"].as_str().unwrap(),
                row["source"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        [("npm", "machine"), ("npm", "tuoen"), ("pip", "machine")],
        "同一个工具两个来源是**两行**，而机器那一行在前"
    );
    // **新键的意义**：机器那一行里没有一条落在版本目录里 ⇒ 那个"会被版本管理器
    // 换掉"的警告数必须还是 0。`insideVersionDir` 是**两个来源**的总数，
    // 所以它只保证不小于 machine 那个数（这一份固定装置里我们那个根的名字
    // 来自 `node -v` 的替身，它有没有版本段不该由测试来假装知道）。
    assert_eq!(
        globals["insideVersionDirMachine"].as_u64(),
        Some(0),
        "tuoen 那一行不该把机器那一行的警告数抬起来"
    );
    assert!(
        globals["insideVersionDir"].as_u64().expect("数字")
            >= globals["insideVersionDirMachine"].as_u64().expect("数字"),
        "总数永远 ≥ machine 那一份：{globals}"
    );

    let toml = std::fs::read_to_string(out.path().join("globals.toml")).expect("globals.toml");
    assert!(toml.contains(r#"source = "machine""#), "{toml}");
    assert!(toml.contains(r#"source = "tuoen""#), "{toml}");
    assert_eq!(toml.matches("[[global]]").count(), 3, "{toml}");
    // 同一个工具两行、来源不同 —— `tool` 不再是主键（`(tool, source)` 才是）。
    assert_eq!(toml.matches(r#"tool = "npm""#).count(), 2, "{toml}");
}
