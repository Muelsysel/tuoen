//! tuoen 的 shim —— 一个**真 `.exe`** 转发器，目标路径在生成时**烘进二进制**。
//!
//! # 为什么是真 `.exe`
//!
//! ① 用户级工具在 `PATH` 上永远排在机器级条目之后（本机实测：进程 `PATH` 首条是机器条目），
//! 所以抢名字只能靠放一个真的可执行文件；
//! ② `.cmd` / `.ps1` shim 在 Node ≥18.20.2 / 20.12.2 / 21.7.3 之后**无法被 spawn**
//! （CVE-2024-27980，nodejs/node#52681）。
//!
//! # 为什么目标路径是"烘进去"的，而不是边车文件
//!
//! 边车（`.shim` 那种 JSON）意味着**每次调用都要开一个文件、读、解析**，还多出一个
//! "两个文件互相不同步"的失效模式。烘进去之后运行时**一个文件都不读**，
//! 而且和 junction 翻转天然配合：shim 指向稳定的 `<store>/<tool>/current/<cmd>.exe`，
//! 换版本只动链接，shim 一动不动（决策 11）。
//!
//! 代价是"改目标要重新生成 shim"，这正是我们要的：改了就是改了，没有中间态。
//!
//! # 槽位
//!
//! 模板二进制里有一段固定长度的数据（[`SLOT_MAGIC`] + 长度 + UTF-16 前缀 + 哨兵）：
//!
//! ```text
//! 偏移 0        16 字节魔数（SLOT_MAGIC）
//! 偏移 16       u32 LE：UTF-16 单元数（0 表示"还没烘过"）
//! 偏移 20       u32 LE：保留，必须为 0
//! 偏移 24       1024 个 u16 LE：`"<目标>" <引号化后的前缀参数>`
//! 偏移 2072     u32 LE 哨兵 0x5A5A5A5A（截断/写歪能被认出来）
//! ```
//!
//! **写入方（[`write_shim`]）与读取方（转发器二进制）共用这里的同一份定义** ——
//! 这是本 crate 最重要的一条纪律：两边各写一份布局定义，迟早会不一致，
//! 而症状是"元数据全对但程序打不开"（票据 #6 在重解析点数据块上踩过同一个坑）。
//!
//! # 前缀里为什么要带引号
//!
//! 槽位里存的是**已经引号化好的**一整段文本，运行时只做一件事：
//! `前缀 ++ 原始命令行去掉第一个 token 之后的一切`。于是转发器里**没有任何引号逻辑**，
//! 而"参数有没有被重新引号化"这个 bug 面直接被删掉了（决策 52）。
//!
//! # 为什么拒绝 `.cmd` / `.bat` / `.ps1` 目标
//!
//! 实测（`docs/acceptance/L0-07-shim.md` §2）：把 `.cmd` 当目标时 `CreateProcessW`
//! 内部会走 `%COMSPEC% /c`，而 cmd 的 `/c` 引号剥离规则会**删掉整行的第一个和最后一个
//! 引号**，于是
//!
//! ```text
//! "…\npm.cmd" config get "cache"   →   …\npm.cmd" config get "cache
//! '…npm.cmd" config get "cache' is not recognized as an internal or external command
//! ```
//!
//! **目标程序根本没跑起来。** 换成文档推荐的 `/S /C ""<目标>" <原文>"` 构造能救回引号，
//! 但 cmd 仍然会展开参数里的 `%VAR%`、并把未加引号的 `&` 当命令分隔符执行 ——
//! 也就是说，shim 变成了一条参数注入通道。所以本 crate **不转发到脚本**。
//! 需要转发到"只有 `.cmd` 启动器"的工具时，正确做法是让目录提供
//! "真 `.exe` + 前缀参数"（node 的 `npm` 就是 `node.exe` + `npm-cli.js`，
//! 实测与 `npm.cmd` 输出逐字节一致）。
//!
//! # 运行时顺序
//!
//! 1. 解槽位 → 拿不到就报"这是模板"并以 [`EXIT_LAUNCH_FAILED`] 退出
//! 2. `SetConsoleCtrlHandler(NULL, TRUE)` **忽略 Ctrl-C** —— 转发器必须先活下来
//! 3. `GetCommandLineW` → 切掉第一个 token → 拿到参数**原文**
//! 4. `CreateProcessW`（继承句柄，所以重定向与管道照常工作）
//! 5. 等它结束 → `GetExitCodeProcess` → 原样退出，**含 `0xC0000005` 这类异常码**

#![cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "本 crate 只发 Windows；非 Windows 上保留定义只为让 workspace 能 clippy"
    )
)]

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

// ─────────────────────────── 槽位定义（读写双方唯一的一份） ───────────────────────────

/// 槽位魔数。生成器靠它在模板二进制里定位槽位。
pub const SLOT_MAGIC: [u8; 16] = *b"TUOENSHIM\x00v1\x00\x00\x00\x00";

/// 模板标记：只在 shim 模板二进制里出现的字符串。
///
/// 它的作用是**拒绝"拿错文件当模板"**：只校验魔数的话，一个恰好含有这 16 字节的
/// 任意文件都会被当成模板并在中间被改写。多一个标记就让这件事从"理论上不会"
/// 变成"结构上不可能"。
pub const TEMPLATE_MARKER: &str = "tuoen-shim/template:v1";

/// 槽位能容纳的 UTF-16 单元数。
pub const SLOT_PREFIX_UNITS: usize = 1024;

/// 魔数之后的固定头部字节数（长度 + 保留字段）。
pub const SLOT_HEADER_BYTES: usize = 8;

/// 前缀数组之后哨兵的字节数。
pub const SLOT_SENTINEL_BYTES: usize = 4;

/// 哨兵取值。
pub const SLOT_SENTINEL: u32 = 0x5A5A_5A5A;

/// 槽位总字节数。
pub const SLOT_BYTES: usize =
    SLOT_MAGIC.len() + SLOT_HEADER_BYTES + SLOT_PREFIX_UNITS * 2 + SLOT_SENTINEL_BYTES;

/// 前缀数组的起始偏移。
pub const SLOT_PREFIX_OFFSET: usize = SLOT_MAGIC.len() + SLOT_HEADER_BYTES;

/// 槽位里能放的**最大字符数**（不是单元数）：代理对占两个单元，所以这只是上界。
pub const SLOT_PREFIX_MAX_UNITS: usize = SLOT_PREFIX_UNITS;

/// shim 自己启动不了目标程序时的退出码。
///
/// 取 9009 是因为 `cmd.exe` 用它表示"不是内部或外部命令" —— 用户看到的语义一致。
/// **它与目标程序自己以 9009 退出无法区分**，这是刻意的取舍：stderr 上有明确的中文
/// 说明，而"能一眼看懂"比"能靠数字区分"更重要。
pub const EXIT_LAUNCH_FAILED: i32 = 9009;

/// 一个还没烘过任何东西的槽位（模板里的样子）。
///
/// **`const fn` 而不是 `static`**：转发器二进制里写
/// `static SLOT: [u8; SLOT_BYTES] = baked_slot();`，于是"槽位长什么样"只有这一份定义，
/// 编译器还会替我们检查尺寸。
#[must_use]
pub const fn baked_slot() -> [u8; SLOT_BYTES] {
    let mut out = [0u8; SLOT_BYTES];
    let mut i = 0;
    while i < SLOT_MAGIC.len() {
        out[i] = SLOT_MAGIC[i];
        i += 1;
    }
    let sentinel = SLOT_SENTINEL.to_le_bytes();
    let base = SLOT_PREFIX_OFFSET + SLOT_PREFIX_UNITS * 2;
    let mut j = 0;
    while j < SLOT_SENTINEL_BYTES {
        out[base + j] = sentinel[j];
        j += 1;
    }
    out
}

/// 把一段前缀编码成一个完整的槽位字节块。
///
/// # Errors
///
/// 前缀超过 [`SLOT_PREFIX_UNITS`] 个 UTF-16 单元时返回 [`ShimError::PrefixTooLong`]。
pub fn encode_slot(prefix: &str) -> Result<Vec<u8>, ShimError> {
    let units: Vec<u16> = prefix.encode_utf16().collect();
    if units.len() > SLOT_PREFIX_UNITS {
        return Err(ShimError::PrefixTooLong {
            units: units.len(),
            limit: SLOT_PREFIX_UNITS,
        });
    }
    let mut out = baked_slot().to_vec();
    out[SLOT_MAGIC.len()..SLOT_MAGIC.len() + 4]
        .copy_from_slice(&(units.len() as u32).to_le_bytes());
    for (index, unit) in units.iter().enumerate() {
        let at = SLOT_PREFIX_OFFSET + index * 2;
        out[at..at + 2].copy_from_slice(&unit.to_le_bytes());
    }
    Ok(out)
}

/// 从一个槽位字节块里解出前缀。
///
/// 返回 `None` 的每一种情况都是"这不是一个烘好的 shim"：长度不够、魔数不对、
/// 哨兵不对、长度越界、单元数为 0（模板）、或者 UTF-16 有落单的代理项。
#[must_use]
pub fn decode_slot(slot: &[u8]) -> Option<String> {
    if slot.len() < SLOT_BYTES {
        return None;
    }
    if slot[..SLOT_MAGIC.len()] != SLOT_MAGIC {
        return None;
    }
    let units = u32::from_le_bytes(
        slot[SLOT_MAGIC.len()..SLOT_MAGIC.len() + 4]
            .try_into()
            .ok()?,
    );
    if units == 0 || units as usize > SLOT_PREFIX_UNITS {
        return None;
    }
    let sentinel_at = SLOT_PREFIX_OFFSET + SLOT_PREFIX_UNITS * 2;
    let sentinel = u32::from_le_bytes(slot[sentinel_at..sentinel_at + 4].try_into().ok()?);
    if sentinel != SLOT_SENTINEL {
        return None;
    }
    let mut wide = Vec::with_capacity(units as usize);
    for index in 0..units as usize {
        let at = SLOT_PREFIX_OFFSET + index * 2;
        wide.push(u16::from_le_bytes(slot[at..at + 2].try_into().ok()?));
    }
    String::from_utf16(&wide).ok()
}

// ─────────────────────────── 命令行切片 ───────────────────────────

/// 从一条**原始**命令行里切掉第一个 token（程序名），返回其后的原文。
///
/// 规则与 C 运行时解析 `argv[0]` 的规则一致：
///
/// 1. 先跳过前导空白（空格与 tab）；
/// 2. 第一个字符是 `"` 时，token 到**下一个** `"` 为止（`argv[0]` **不做反斜杠转义**）；
/// 3. 否则 token 到下一个空白为止。
///
/// 返回的切片**一个字节都没有被重新引号化** —— 这是整个 crate 的核心不变量。
/// 返回的空切片表示"没有参数"。
///
/// # 它为什么能只靠字符串扫描完成
///
/// 因为**根本起不来的情况不需要处理**：本机实测（`docs/acceptance/L0-07-shim.md` §3），
/// 调用者只有三种能在 PATH 上把我们启动起来的写法 ——
///
/// - `cmd.exe` 把**用户敲的原文**交下来（`argdump  -a "b c"`，第一个 token 是裸名字）
/// - `cmd.exe` 给出完整路径且**不加引号**（只在路径没有空格时能启动，
///   于是第一个 token 一定不含空格）
/// - PowerShell 给出**带引号的完整路径**
///
/// 反过来，"不加引号、又含空格的第一个 token"在 cmd 里连启动都会失败
/// （实测：`The system cannot find the path specified.`）。唯一能造出那种输入的方式，
/// 是调用者显式指定 `lpApplicationName` 却把命令行写成不加引号 —— 那种调用者的
/// `argv[0]` 不带 shim 也已经坏了。
#[must_use]
pub fn split_argv0_wide(command_line: &[u16]) -> &[u16] {
    let mut i = 0;
    while i < command_line.len() && is_blank(command_line[i]) {
        i += 1;
    }
    if i >= command_line.len() {
        return &[];
    }
    if command_line[i] == u16::from(b'"') {
        i += 1;
        while i < command_line.len() && command_line[i] != u16::from(b'"') {
            i += 1;
        }
        if i < command_line.len() {
            i += 1; // 吃掉闭引号
        }
    } else {
        while i < command_line.len() && !is_blank(command_line[i]) {
            i += 1;
        }
    }
    &command_line[i..]
}

fn is_blank(unit: u16) -> bool {
    unit == u16::from(b' ') || unit == u16::from(b'\t')
}

// ─────────────────────────── 引号化 ───────────────────────────

/// 按 `CommandLineToArgvW` 的规则引号化一个参数。
///
/// 规则（与 Rust 标准库、Go 标准库用的是同一套）：
/// - 不含空格、tab、引号的参数**原样输出**（省掉一对引号，也让 `--flag=value` 可读）；
/// - 否则用 `"` 包起来；参数里的 `"` 前面要加 `\`，而 `"` 前面原本的 `\` 数量要**翻倍**；
/// - 结尾连续的反斜杠要翻倍，否则它们会转义掉我们加的那个闭引号。
pub fn quote_arg(arg: &str) -> String {
    let needs_quotes = arg.is_empty()
        || arg
            .chars()
            .any(|c| c == ' ' || c == '\t' || c == '"' || c == '\n' || c == '\u{b}');
    if !needs_quotes {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                // 反斜杠翻倍，再补一个转义 `"` 自己的反斜杠。
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push('\\');
                out.push('"');
            }
            _ => {
                backslashes = 0;
                out.push(c);
            }
        }
    }
    for _ in 0..backslashes {
        out.push('\\');
    }
    out.push('"');
    out
}

/// 反解 `prefix` 的第一个 token —— 也就是写死的目标路径。
///
/// 只用在运行时的**自指保险丝**上（见转发器 `main.rs`），所以它是尽力而为的：
/// 解不出来就返回 `None`，**绝不因此拒绝启动用户的程序**。
///
/// 之所以可以这么简单，是因为前缀是**我们自己**渲染的（[`ShimSpec::render_prefix`]）：
/// 目标里不可能有引号（[`ShimSpec::validate`] 拒了），所以"以引号开头就取到下一个引号，
/// 否则取到第一个空格"是充分的。
#[must_use]
pub fn prefix_target(prefix: &str) -> Option<&str> {
    if let Some(rest) = prefix.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(&rest[..end])
    } else {
        let end = prefix.find(' ').unwrap_or(prefix.len());
        if end == 0 { None } else { Some(&prefix[..end]) }
    }
}

/// 两个路径是否指向同一个文件。
///
/// **用 `canonicalize` 而不是字符串比较**：`..`、8.3 短名、junction、大小写都会让
/// 字符串比较得出错误答案（票据 #6 的 `ensure_inside_store` 就是栽在这上面）。
/// 目标与模板都**已经存在**，所以能规范化；落盘路径可能还不存在，那就规范化它的
/// 父目录再拼上文件名。任何一步规范化失败就返回 `false` ——
/// **这道守卫绝不能变成误报的来源**。
#[must_use]
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (resolve_for_compare(a), resolve_for_compare(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn resolve_for_compare(path: &Path) -> Option<PathBuf> {
    if let Ok(real) = std::fs::canonicalize(path) {
        return Some(fold_case(real));
    }
    let parent = path.parent()?;
    let name = path.file_name()?;
    let real_parent = std::fs::canonicalize(parent).ok()?;
    Some(fold_case(real_parent.join(name)))
}

fn fold_case(path: PathBuf) -> PathBuf {
    PathBuf::from(path.to_string_lossy().to_lowercase())
}

// ─────────────────────────── 规格 ───────────────────────────

/// 一个 shim 的完整规格。**生成时定死，之后运行时不再读任何配置。**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimSpec {
    /// shim 自己的名字，**不带扩展名**（`node`、`npm`、`pip3.12`）。
    /// 落盘文件名是 `<name>.exe`。
    pub name: String,
    /// 写死的目标可执行文件**绝对路径**。
    pub target: PathBuf,
    /// 透传给目标的固定前缀参数（`npm` 就是 `node.exe` + `npm-cli.js`）。
    pub prefix_args: Vec<String>,
}

impl ShimSpec {
    /// 只校验"与文件系统无关"的部分，纯函数。
    ///
    /// # Errors
    ///
    /// 名字不安全、目标不是绝对路径、目标是脚本、前缀放不进槽位。
    pub fn validate(&self) -> Result<(), ShimError> {
        validate_name(&self.name)?;
        if !self.target.is_absolute() {
            return Err(ShimError::TargetNotAbsolute(self.target.clone()));
        }
        if let Some(extension) = self.target.extension().and_then(OsStr::to_str) {
            let lowered = extension.to_ascii_lowercase();
            if let Some(kind) = ScriptKind::from_extension(&lowered) {
                return Err(ShimError::ScriptTarget {
                    target: self.target.clone(),
                    kind,
                });
            }
        }
        let target_text = self.target.to_string_lossy();
        if target_text.contains('"') {
            return Err(ShimError::TargetContainsQuote(self.target.clone()));
        }
        if target_text.ends_with('\\') {
            // 目录不是可执行文件；而结尾反斜杠在引号里会转义掉闭引号。
            return Err(ShimError::TargetEndsWithSeparator(self.target.clone()));
        }
        let prefix = self.render_prefix();
        // **前缀里不能有 U+0000。** 烘进去的那个字符串最终是 `CreateProcessW` 的
        // 命令行，而它是 NUL 结尾的：内部出现一个 NUL，命令行就在那里被截断 ——
        // 目标会带着**比预期少**的参数启动，而且没有任何东西会报错。
        //
        // 这条守卫是这次事故的远亲：那个 409 进程的递归，根因正是"参数被拼在了
        // NUL 之后"，也就是同一个"NUL 把命令行截断"的机制。在这条路径上它不该
        // 有第二次机会。
        if let Some(index) = self.prefix_args.iter().position(|a| a.contains('\0')) {
            return Err(ShimError::PrefixContainsNul { index });
        }
        let units = prefix.encode_utf16().count();
        if units > SLOT_PREFIX_UNITS {
            return Err(ShimError::PrefixTooLong {
                units,
                limit: SLOT_PREFIX_UNITS,
            });
        }
        Ok(())
    }

    /// 烘进槽位的完整字符串：`"<目标>" <引号化后的前缀参数…>`。
    ///
    /// **这是唯一一次引号化**。运行时只做字符串拼接。
    #[must_use]
    pub fn render_prefix(&self) -> String {
        let mut out = quote_arg(&self.target.to_string_lossy());
        for arg in &self.prefix_args {
            out.push(' ');
            out.push_str(&quote_arg(arg));
        }
        out
    }

    /// 落盘文件名。
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}.exe", self.name)
    }
}

/// 名字校验：shim 的文件名会被放到 `PATH` 上，而且是我们替用户敲的命令。
pub fn validate_name(name: &str) -> Result<(), ShimError> {
    if name.is_empty() {
        return Err(ShimError::BadName {
            name: name.to_owned(),
            why: "名字是空的",
        });
    }
    if name.contains(['\\', '/', ':']) {
        return Err(ShimError::BadName {
            name: name.to_owned(),
            why: "名字里有路径分隔符或冒号 —— shim 是 PATH 上的一个文件名，不是路径",
        });
    }
    if name.chars().any(char::is_control) {
        return Err(ShimError::BadName {
            name: name.to_owned(),
            why: "名字里有控制字符（会破坏列表与日志的行结构）",
        });
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(ShimError::BadName {
            name: name.to_owned(),
            why: "名字以点或空格结尾 —— Win32 会吃掉结尾的点与空格，\
                  于是这个文件在多数工具里打不开也删不掉",
        });
    }
    if is_reserved_device_name(name) {
        return Err(ShimError::BadName {
            name: name.to_owned(),
            why: "名字是 Windows 保留设备名（CON/PRN/AUX/NUL/COM1-9/LPT1-9，\
                  基名比较、大小写不敏感，`CON.txt` 也算）",
        });
    }
    Ok(())
}

/// 保留设备名：看**基名**（第一个点之前），大小写不敏感，含上标 ¹²³。
fn is_reserved_device_name(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name);
    let upper = base.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    for (prefix, digits) in [("COM", "123456789¹²³"), ("LPT", "123456789¹²³")] {
        if let Some(rest) = upper.strip_prefix(prefix)
            && rest.chars().count() == 1
            && digits.contains(rest)
        {
            return true;
        }
    }
    false
}

/// 需要 `cmd.exe` / PowerShell / WSH 才能跑起来的脚本类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    /// `.cmd`
    Cmd,
    /// `.bat`
    Bat,
    /// `.ps1` / `.psm1`
    PowerShell,
    /// `.vbs` / `.vbe` / `.js` / `.jse` / `.wsf` / `.wsh`
    WindowsScriptHost,
    /// `.sh`
    Shell,
}

impl ScriptKind {
    fn from_extension(extension: &str) -> Option<ScriptKind> {
        match extension {
            "cmd" => Some(ScriptKind::Cmd),
            "bat" => Some(ScriptKind::Bat),
            "ps1" | "psm1" => Some(ScriptKind::PowerShell),
            "vbs" | "vbe" | "js" | "jse" | "wsf" | "wsh" => Some(ScriptKind::WindowsScriptHost),
            "sh" => Some(ScriptKind::Shell),
            _ => None,
        }
    }

    /// 稳定英文标识（进 `--json`）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ScriptKind::Cmd => "cmd",
            ScriptKind::Bat => "bat",
            ScriptKind::PowerShell => "powershell",
            ScriptKind::WindowsScriptHost => "windows-script-host",
            ScriptKind::Shell => "shell",
        }
    }

    /// 中文解释，进错误消息。
    #[must_use]
    pub fn why(self) -> &'static str {
        match self {
            ScriptKind::Cmd | ScriptKind::Bat => {
                "它是 `.cmd` / `.bat`。把脚本当目标时 `CreateProcessW` 内部会走 \
                 `%COMSPEC% /c`，而 cmd 的 `/c` 会把整行的第一个和最后一个引号删掉 —— \
                 实测结果是目标脚本**根本没跑起来**。用文档推荐的 `/S /C \"\"<目标>\" <原文>\"` \
                 构造能救回引号，但 cmd 仍会展开参数里的 `%VAR%`、并把不加引号的 `&` \
                 当命令分隔符执行，于是 shim 变成一条参数注入通道"
            }
            ScriptKind::PowerShell => {
                "它是 `.ps1`。`PowerShell` 脚本不是可执行映像，必须经过 `powershell.exe` \
                 才能运行，而那条路径上的参数要经过 PowerShell 自己的解析器"
            }
            ScriptKind::WindowsScriptHost => {
                "它是 Windows Script Host 脚本（`.vbs` / `.js` / `.wsf` 等）。\
                 它靠 shell 关联启动，`CreateProcessW` 不会为你做这件事"
            }
            ScriptKind::Shell => "它是 `.sh`，需要 POSIX shell",
        }
    }
}

// ─────────────────────────── 找模板 ───────────────────────────

/// 模板从哪里找到的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSource {
    /// `TUOEN_SHIM_TEMPLATE` 环境变量。
    EnvVar,
    /// 和当前可执行文件同目录的 `tuoen-shim.exe`。
    BesideExe,
}

/// 模板文件的固定名字。
pub const TEMPLATE_FILE_NAME: &str = "tuoen-shim.exe";

/// `TUOEN_SHIM_TEMPLATE` 环境变量名。
pub const TEMPLATE_ENV_VAR: &str = "TUOEN_SHIM_TEMPLATE";

/// 找模板。
///
/// 顺序：`TUOEN_SHIM_TEMPLATE` → 与 `exe_dir` 同目录的 `tuoen-shim.exe`。
/// 找不到时把**找过的每一个位置**都列出来 —— 一个只说"找不到"的错误，
/// 会让人去猜它到底看了哪里。
///
/// # Errors
///
/// 两处都没有模板，或环境变量指的文件不存在。
pub fn find_template(exe_dir: &Path) -> Result<(PathBuf, TemplateSource), ShimError> {
    find_template_with(
        exe_dir,
        std::env::var_os(TEMPLATE_ENV_VAR).map(PathBuf::from),
    )
}

/// 同上，但环境变量的值由调用者给。
///
/// **这样拆是为了可测**：读写进程级环境变量在并行测试里是有竞争的
/// （Rust 2024 起 `std::env::set_var` 干脆变成了 `unsafe`）。
/// 把"读环境"挤到最外面那一行，里面这个就是纯函数。
///
/// # Errors
///
/// 见 [`find_template`]。
pub fn find_template_with(
    exe_dir: &Path,
    from_env: Option<PathBuf>,
) -> Result<(PathBuf, TemplateSource), ShimError> {
    let beside = exe_dir.join(TEMPLATE_FILE_NAME);
    if let Some(from_env) = from_env {
        if from_env.is_file() {
            return Ok((from_env, TemplateSource::EnvVar));
        }
        return Err(ShimError::TemplateMissing {
            tried: vec![beside],
            from_env: Some(from_env),
        });
    }
    if beside.is_file() {
        return Ok((beside, TemplateSource::BesideExe));
    }
    Err(ShimError::TemplateMissing {
        tried: vec![beside],
        from_env: None,
    })
}

// ─────────────────────────── 生成 ───────────────────────────

/// 生成结果的记录。**这不是运行时读的东西**，只是给 CLI 报告用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimOutcome {
    /// 落盘路径。
    pub path: PathBuf,
    /// 文件字节数。
    pub bytes: usize,
    /// 烘进去的前缀（`"<目标>" …`）。
    pub prefix: String,
    /// 是否覆盖了一个已存在的 shim。
    pub replaced: bool,
    /// 改写了模板里几处槽位。
    ///
    /// **正常情况下是 1，但可能是 2 以上** —— 同一个 `const` 数组会被编译器复制到
    /// 多处，每一份都要改写（见 [`pristine_slot_offsets`]）。把它报告出来，
    /// 是为了让"这个数字从来不等于 1"这件事有一天能被看见。
    pub slot_count: usize,
}

/// 复制模板、把 `spec` 烘进槽位、原子落到 `dest`。
///
/// 每一步失败都给出**具体的哪一步**：模板不认、目标不存在、前缀太长、写不进去。
/// 写完之后会**重新读回来解一遍**，解不出预期内容就报 [`ShimError::SelfCheckFailed`] ——
/// "生成了一个打不开的 shim"必须在生成时就被抓住，而不是等用户敲命令的时候。
///
/// # Errors
///
/// 见 [`ShimError`]。
pub fn write_shim(template: &Path, dest: &Path, spec: &ShimSpec) -> Result<ShimOutcome, ShimError> {
    spec.validate()?;
    let meta = std::fs::metadata(&spec.target).map_err(|source| ShimError::TargetUnreadable {
        target: spec.target.clone(),
        source,
    })?;
    if !meta.is_file() {
        return Err(ShimError::TargetNotAFile(spec.target.clone()));
    }

    // ── 三道守卫，全部由一次真实事故换来的 ────────────────────────────
    //
    // 事故：测试里让 shim 的落盘路径**和它的目标路径是同一个文件**。生成出来的
    // 那个 `.exe` 于是"目标是它自己"，运行一次就无限自我启动 ——
    // 本机实测堆到 **20580 个进程**，内存被吃干。
    //
    // 教训不是"测试写错了"，而是**这个失效模式的代价不对称**：写错一个名字，
    // 换来的是整台机器失去响应。所以生成时三道守卫，运行时还有第四道保险丝。
    if same_file(dest, &spec.target) {
        return Err(ShimError::DestinationIsTarget(dest.to_owned()));
    }
    if same_file(dest, template) {
        return Err(ShimError::DestinationIsTemplate(dest.to_owned()));
    }
    if dest.exists() {
        let existing = std::fs::read(dest).map_err(|source| ShimError::WriteFailed {
            path: dest.to_owned(),
            source,
        })?;
        // 判据是"有没有魔数"，**不是**"有没有原始槽位"：上一次生成的 shim
        // 槽位是烘过的，它仍然是我们的东西，必须允许覆盖。
        if !(contains(&existing, TEMPLATE_MARKER.as_bytes()) && find_magic(&existing).is_some()) {
            return Err(ShimError::DestinationNotAShim(dest.to_owned()));
        }
    }

    let template_bytes =
        std::fs::read(template).map_err(|source| ShimError::TemplateUnreadable {
            template: template.to_owned(),
            source,
        })?;
    if !contains(&template_bytes, TEMPLATE_MARKER.as_bytes()) {
        return Err(ShimError::TemplateNotRecognized {
            template: template.to_owned(),
            why: "里面没有模板标记字符串",
        });
    }
    let slots = pristine_slot_offsets(&template_bytes);
    if slots.is_empty() {
        return Err(ShimError::TemplateNotRecognized {
            template: template.to_owned(),
            why: "里面没有一段「未烘过」的槽位",
        });
    }

    let prefix = spec.render_prefix();
    let slot = encode_slot(&prefix)?;
    let mut patched = template_bytes;
    // **改写全部候选，不是第一个。** 见 `pristine_slot_offsets` 的文档：
    // 同一个 const 会被编译器复制到多处，只改第一处是"碰巧能跑"。
    let patched_count = slots.len();
    for at in &slots {
        patched[*at..*at + SLOT_BYTES].copy_from_slice(&slot);
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ShimError::WriteFailed {
            path: dest.to_owned(),
            source,
        })?;
    }
    let replaced = dest.exists();
    write_atomically(dest, &patched)?;

    // 自检：把落盘的东西读回来解一遍，并且**确认没有漏网的原始槽位**。
    let written = std::fs::read(dest).map_err(|source| ShimError::WriteFailed {
        path: dest.to_owned(),
        source,
    })?;
    let left = pristine_slot_offsets(&written);
    if !left.is_empty() {
        return Err(ShimError::SelfCheckFailed {
            path: dest.to_owned(),
        });
    }
    let decoded = slots
        .iter()
        .filter_map(|at| decode_slot(&written[*at..]))
        .collect::<Vec<_>>();
    if decoded.len() != patched_count || decoded.iter().any(|found| found != &prefix) {
        return Err(ShimError::SelfCheckFailed {
            path: dest.to_owned(),
        });
    }

    Ok(ShimOutcome {
        path: dest.to_owned(),
        bytes: written.len(),
        prefix,
        replaced,
        slot_count: patched_count,
    })
}

fn write_atomically(dest: &Path, bytes: &[u8]) -> Result<(), ShimError> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let stem = dest
        .file_name()
        .map_or_else(|| "shim".to_owned(), |n| n.to_string_lossy().into_owned());
    let temp = parent.join(format!(".{stem}.tmp-{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        std::fs::write(&temp, bytes)?;
        // `std::fs::rename` 在 Windows 上走 `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`，
        // 所以覆盖是原子的：要么看到旧的，要么看到新的，不会看到半个。
        std::fs::rename(&temp, dest)
    };
    write().map_err(|source| {
        let _ = std::fs::remove_file(&temp);
        ShimError::WriteFailed {
            path: dest.to_owned(),
            source,
        }
    })
}

fn find_magic(haystack: &[u8]) -> Option<usize> {
    haystack
        .windows(SLOT_MAGIC.len())
        .position(|window| window == SLOT_MAGIC)
}

/// 找出模板里**所有**"还没烘过"的槽位偏移。
///
/// # 为什么不是"找到第一处魔数就下手"——这是实测换来的
///
/// 第一版就是这么写的，而它**碰巧能跑**：本机 debug 模板里魔数出现了 **4 次**，
/// 其中只有 1 处是真的槽位（偏移 710964），另外 3 处是编译产物里别的东西
/// （调试信息里的常量值等），它们后面第 16 个字节分别是 `0x54` / `0x5C` / `0xE9`
/// —— 也就是说第一处**恰好**是真槽位纯属运气。
///
/// 一个把目标路径写进错误位置的生成器，症状是"生成的 shim 说自己是模板"
/// 或者更糟"它启动了别的东西"。所以：
///
/// - 判据不是"有魔数"，而是**整段 2076 字节逐字节等于 [`baked_slot()`]**。
///   一个 2076 字节的常量数组只有它自己能被完全匹配上，别的数据进来的概率是零。
/// - **所有**匹配上的位置都会被改写（同一个 const 的每一份拷贝都要一致），
///   而不是只改第一处。
#[must_use]
pub fn pristine_slot_offsets(bytes: &[u8]) -> Vec<usize> {
    let pristine = baked_slot();
    let mut offsets = Vec::new();
    if bytes.len() < SLOT_BYTES {
        return offsets;
    }
    for at in 0..=bytes.len() - SLOT_BYTES {
        if bytes[at..at + SLOT_MAGIC.len()] != SLOT_MAGIC {
            continue;
        }
        if bytes[at..at + SLOT_BYTES] == pristine {
            offsets.push(at);
        }
    }
    offsets
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

// ─────────────────────────── 错误 ───────────────────────────

/// shim 生成与运行时的错误。
#[derive(Debug)]
pub enum ShimError {
    /// 名字不安全。
    BadName {
        /// 被拒的名字。
        name: String,
        /// 为什么。
        why: &'static str,
    },
    /// 目标不是绝对路径。
    TargetNotAbsolute(PathBuf),
    /// 目标是脚本，不能直接转发。
    ScriptTarget {
        /// 目标。
        target: PathBuf,
        /// 脚本类型。
        kind: ScriptKind,
    },
    /// 目标路径里有引号。
    TargetContainsQuote(PathBuf),
    /// 目标路径以分隔符结尾。
    TargetEndsWithSeparator(PathBuf),
    /// 前缀放不进槽位。
    PrefixTooLong {
        /// 实际单元数。
        units: usize,
        /// 上限。
        limit: usize,
    },
    /// 某个前缀参数里含 U+0000。
    ///
    /// NUL 会把 `CreateProcessW` 的命令行**在那里截断**，目标于是带着比预期少的
    /// 参数启动，且不报任何错 —— 所以它在写进槽位之前就必须被拒。
    PrefixContainsNul {
        /// 第几个前缀参数（从 0 数）。
        index: usize,
    },
    /// 目标读不到（多半是不存在）。
    TargetUnreadable {
        /// 目标。
        target: PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },
    /// 目标存在但不是普通文件。
    TargetNotAFile(PathBuf),
    /// 模板读不到。
    TemplateUnreadable {
        /// 模板。
        template: PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },
    /// 模板不认（缺标记串或魔数）。
    TemplateNotRecognized {
        /// 模板。
        template: PathBuf,
        /// 为什么。
        why: &'static str,
    },
    /// 找不到模板。
    TemplateMissing {
        /// 找过的位置。
        tried: Vec<PathBuf>,
        /// 环境变量指的位置（如果设了但不存在）。
        from_env: Option<PathBuf>,
    },
    /// 写不进去。
    WriteFailed {
        /// 目标路径。
        path: PathBuf,
        /// 底层错误。
        source: std::io::Error,
    },
    /// 写完自检没过。
    SelfCheckFailed {
        /// 目标路径。
        path: PathBuf,
    },
    /// 落盘路径**就是目标本身** —— 会造出一个"目标是它自己"的转发器。
    DestinationIsTarget(PathBuf),
    /// 落盘路径**就是模板本身** —— 会把共享的模板改掉。
    DestinationIsTemplate(PathBuf),
    /// 落盘路径上已经有一个不是 tuoen shim 的文件。
    DestinationNotAShim(PathBuf),
}

impl ShimError {
    /// 稳定英文标识，进 `--json`。
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            ShimError::BadName { .. } => "bad-name",
            ShimError::TargetNotAbsolute(_) => "target-not-absolute",
            ShimError::ScriptTarget { .. } => "script-target",
            ShimError::TargetContainsQuote(_) => "target-contains-quote",
            ShimError::TargetEndsWithSeparator(_) => "target-ends-with-separator",
            ShimError::PrefixTooLong { .. } => "prefix-too-long",
            ShimError::PrefixContainsNul { .. } => "prefix-nul",
            ShimError::TargetUnreadable { .. } => "target-unreadable",
            ShimError::TargetNotAFile(_) => "target-not-a-file",
            ShimError::TemplateUnreadable { .. } => "template-unreadable",
            ShimError::TemplateNotRecognized { .. } => "template-not-recognized",
            ShimError::TemplateMissing { .. } => "template-missing",
            ShimError::WriteFailed { .. } => "write-failed",
            ShimError::SelfCheckFailed { .. } => "self-check-failed",
            ShimError::DestinationIsTarget(_) => "dest-is-target",
            ShimError::DestinationIsTemplate(_) => "dest-is-template",
            ShimError::DestinationNotAShim(_) => "dest-not-a-shim",
        }
    }

    /// 是不是"用户能自己改对"的错误（用来决定 CLI 的退出码与措辞）。
    #[must_use]
    pub fn is_user_error(&self) -> bool {
        match self {
            ShimError::BadName { .. }
            | ShimError::TargetNotAbsolute(_)
            | ShimError::TargetContainsQuote(_)
            | ShimError::TargetEndsWithSeparator(_)
            | ShimError::PrefixTooLong { .. }
            | ShimError::PrefixContainsNul { .. }
            | ShimError::TargetNotAFile(_) => true,
            ShimError::ScriptTarget { .. } => true,
            ShimError::TargetUnreadable { .. } => true,
            ShimError::TemplateMissing { .. } => true,
            ShimError::DestinationIsTarget(_)
            | ShimError::DestinationIsTemplate(_)
            | ShimError::DestinationNotAShim(_) => true,
            ShimError::TemplateUnreadable { .. }
            | ShimError::TemplateNotRecognized { .. }
            | ShimError::WriteFailed { .. }
            | ShimError::SelfCheckFailed { .. } => false,
        }
    }
}

impl fmt::Display for ShimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShimError::BadName { name, why } => {
                write!(f, "shim 名字 `{name}` 不能用：{why}")
            }
            ShimError::TargetNotAbsolute(target) => write!(
                f,
                "shim 目标必须是绝对路径，给的是 `{}`。相对路径会在**调用者**的当前目录下解析 —— \
                 同一个 shim 在不同目录里跑不同的程序",
                target.display()
            ),
            ShimError::ScriptTarget { target, kind } => write!(
                f,
                "shim 目标 `{}` 不能直接用：{}。\n\
                 目录应当为它提供「真 `.exe` + 前缀参数」的等价写法",
                target.display(),
                kind.why()
            ),
            ShimError::TargetContainsQuote(target) => write!(
                f,
                "shim 目标 `{}` 里有引号 —— Windows 路径里不该出现引号，无法安全地写进命令行",
                target.display()
            ),
            ShimError::TargetEndsWithSeparator(target) => write!(
                f,
                "shim 目标 `{}` 以反斜杠结尾 —— 那是目录，不是可执行文件",
                target.display()
            ),
            ShimError::PrefixContainsNul { index } => write!(
                f,
                "拒绝生成：第 {} 个前缀参数里含 U+0000。
\
                 那个字符会把命令行**在那里截断** —— 目标会带着比预期少的参数启动，
\
                 而且不会有任何东西报错。
                 请把它去掉：参数是文本，文本里不该有 NUL。",
                index + 1
            ),
            ShimError::PrefixTooLong { units, limit } => write!(
                f,
                "shim 前缀太长：{units} 个 UTF-16 单元，槽位只能放 {limit} 个。\
                 目标路径加上前缀参数一共不能超过这个数"
            ),
            ShimError::TargetUnreadable { target, source } => write!(
                f,
                "读不到 shim 目标 `{}`：{source}。\
                 目标不存在通常意味着那个版本已经被卸载，而 `current` 联接还指着它",
                target.display()
            ),
            ShimError::TargetNotAFile(target) => {
                write!(f, "shim 目标 `{}` 不是一个文件", target.display())
            }
            ShimError::TemplateUnreadable { template, source } => {
                write!(f, "读不到 shim 模板 `{}`：{source}", template.display())
            }
            ShimError::TemplateNotRecognized { template, why } => write!(
                f,
                "`{}` 不是一个 tuoen shim 模板（{why}）。\
                 它应该是与 `tuoen.exe` 同目录的 `{TEMPLATE_FILE_NAME}`",
                template.display()
            ),
            ShimError::TemplateMissing { tried, from_env } => {
                write!(f, "找不到 shim 模板。找过的位置：")?;
                for path in tried {
                    write!(f, "\n  - {}", path.display())?;
                }
                if let Some(from_env) = from_env {
                    write!(
                        f,
                        "\n`{TEMPLATE_ENV_VAR}` 设成了 `{}`，但那个文件不存在",
                        from_env.display()
                    )?;
                }
                write!(
                    f,
                    "\n模板是发行包里与 `tuoen.exe` 并列的 `{TEMPLATE_FILE_NAME}`；\
                     从源码跑测试时用 `{TEMPLATE_ENV_VAR}` 指到 `target/<profile>/tuoen-shim.exe`"
                )
            }
            ShimError::WriteFailed { path, source } => {
                write!(f, "写 shim `{}` 失败：{source}", path.display())
            }
            ShimError::SelfCheckFailed { path } => write!(
                f,
                "shim `{}` 写完之后读回来解不出刚烘进去的内容 —— 这个文件不可用，已放弃",
                path.display()
            ),
            ShimError::DestinationIsTarget(path) => write!(
                f,
                "拒绝生成：落盘路径 `{}` **就是 shim 的目标本身**。\n\
                 那样生成的转发器目标是它自己，运行一次就会无限自我启动 —— \
                 本机实测过一次，堆到 20580 个进程把内存吃干。\n\
                 请把 shim 放到一个和目标不同的目录里。",
                path.display()
            ),
            ShimError::DestinationIsTemplate(path) => write!(
                f,
                "拒绝生成：落盘路径 `{}` **就是 shim 模板本身**。\n\
                 模板是所有 shim 共用的那一份，就地改写它会让以后生成的每一个 shim \
                 都建立在一个被改过的模板上。",
                path.display()
            ),
            ShimError::DestinationNotAShim(path) => write!(
                f,
                "拒绝覆盖 `{}`：它不是 tuoen 生成的 shim。\n\
                 这个命令只会覆盖自己生成的东西 —— 一个不属于我们的文件出现在 shim 目录里，\
                 多半意味着别的东西也在这条 `PATH` 上放文件，那件事值得先看一眼。",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ShimError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ShimError::TargetUnreadable { source, .. }
            | ShimError::TemplateUnreadable { source, .. }
            | ShimError::WriteFailed { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn handled(rest: &[u16]) -> String {
        String::from_utf16_lossy(rest)
    }

    // ── 引号化 ──────────────────────────────────────────────────────

    #[test]
    fn plain_arguments_are_not_quoted() {
        assert_eq!(quote_arg("-a"), "-a");
        assert_eq!(quote_arg("--flag=value"), "--flag=value");
        assert_eq!(quote_arg(r"C:\x\node.exe"), r"C:\x\node.exe");
    }

    #[test]
    fn arguments_that_need_quotes_get_them() {
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a\tb"), "\"a\tb\"");
    }

    #[test]
    fn embedded_quotes_and_trailing_backslashes_follow_the_crt_rules() {
        // 这几个是本票最容易写错的边界：写错了的后果是"参数偶尔多一个反斜杠"。
        assert_eq!(quote_arg(r#"he said "hi""#), r#""he said \"hi\"""#);
        assert_eq!(
            quote_arg(r"C:\path with space\"),
            r#""C:\path with space\\""#
        );
        assert_eq!(quote_arg(r"a b\\"), r#""a b\\\\""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        // 反斜杠本身**不是**需要引号化的字符 —— CRT 只把空格与 tab 当分隔符。
        // （我第一版把这条写成 `"\\"`，是**期望值写错了**，不是代码错了。）
        assert_eq!(quote_arg(r"\"), r"\");
        assert_eq!(quote_arg(r"\\"), r"\\");
        // 只有"带空格的参数结尾处的反斜杠"才需要翻倍，上面第二条覆盖了它。
        assert_eq!(quote_arg("a\\b"), r"a\b");
    }

    // ── 命令行切片 ──────────────────────────────────────────────────

    #[test]
    fn split_handles_a_quoted_program_name() {
        assert_eq!(
            handled(split_argv0_wide(&wide(r#""C:\a b\node.exe" --version"#))),
            " --version"
        );
        assert_eq!(handled(split_argv0_wide(&wide(r#""C:\a b\node.exe""#))), "");
    }

    #[test]
    fn split_handles_a_bare_program_name() {
        // `cmd.exe` 交下来的就是这个形状：用户敲的原文。
        assert_eq!(
            handled(split_argv0_wide(&wide(r#"argdump  -a "b c""#))),
            r#"  -a "b c""#
        );
        assert_eq!(handled(split_argv0_wide(&wide(r#"argdump"#))), "");
    }

    #[test]
    fn split_handles_an_unquoted_full_path_without_spaces() {
        assert_eq!(
            handled(split_argv0_wide(&wide(r"C:\x\argdump.exe  -a"))),
            "  -a"
        );
    }

    #[test]
    fn split_skips_leading_whitespace() {
        assert_eq!(
            handled(split_argv0_wide(&wide(r#"  "C:\x\a.exe" -a"#))),
            " -a"
        );
        assert_eq!(handled(split_argv0_wide(&wide("   "))), "");
        assert_eq!(handled(split_argv0_wide(&[])), "");
    }

    #[test]
    fn split_preserves_the_rest_byte_for_byte() {
        // **本 crate 的核心不变量**：返回的切片里一个字符都没被动过。
        let tricky = r#" "a b" "" x --flag=v "trail\\" "\"q\"""#;
        let line = format!("\"C:\\x\\a.exe\"{tricky}");
        assert_eq!(handled(split_argv0_wide(&wide(&line))), tricky);
    }

    // ── 槽位 ────────────────────────────────────────────────────────

    #[test]
    fn the_template_slot_is_not_decodable() {
        // 模板**必须**解不出来，否则一个裸模板会被当成可用 shim 跑起来。
        assert_eq!(decode_slot(&baked_slot()), None);
    }

    #[test]
    fn a_baked_slot_round_trips() {
        for prefix in [
            r#""C:\x\node.exe""#,
            r#""C:\a b\node.exe" "C:\a b\npm-cli.js""#,
            r#""C:\中文 目录\node.exe""#,
        ] {
            let slot = encode_slot(prefix).expect("encode");
            assert_eq!(decode_slot(&slot).as_deref(), Some(prefix));
        }
    }

    #[test]
    fn a_corrupted_slot_is_rejected_rather_than_guessed() {
        let good = encode_slot(r#""C:\x\node.exe""#).expect("encode");

        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert_eq!(decode_slot(&bad_magic), None, "魔数错了必须拒");

        let mut bad_sentinel = good.clone();
        let at = SLOT_PREFIX_OFFSET + SLOT_PREFIX_UNITS * 2;
        bad_sentinel[at] ^= 0xFF;
        assert_eq!(decode_slot(&bad_sentinel), None, "哨兵错了必须拒（截断）");

        let mut too_long = good.clone();
        too_long[SLOT_MAGIC.len()..SLOT_MAGIC.len() + 4]
            .copy_from_slice(&(SLOT_PREFIX_UNITS as u32 + 1).to_le_bytes());
        assert_eq!(decode_slot(&too_long), None, "长度越界必须拒");

        let mut zero = good.clone();
        zero[SLOT_MAGIC.len()..SLOT_MAGIC.len() + 4].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(decode_slot(&zero), None, "长度为 0 = 模板，必须拒");

        assert_eq!(decode_slot(&good[..SLOT_BYTES - 1]), None, "短了必须拒");
        assert_eq!(decode_slot(&[]), None);
    }

    #[test]
    fn the_slot_size_is_what_the_documented_layout_says() {
        // 布局是**写进文档的契约**，所以把它钉住：改了这里就得改文档。
        assert_eq!(SLOT_BYTES, 16 + 8 + 1024 * 2 + 4);
        assert_eq!(SLOT_PREFIX_OFFSET, 24);
        assert_eq!(baked_slot().len(), SLOT_BYTES);
    }

    // ── 规格校验 ────────────────────────────────────────────────────

    fn spec(name: &str, target: &str) -> ShimSpec {
        ShimSpec {
            name: name.to_owned(),
            target: PathBuf::from(target),
            prefix_args: Vec::new(),
        }
    }

    #[test]
    fn a_script_target_is_refused_with_the_measured_reason() {
        for (target, kind) in [
            (r"C:\x\npm.cmd", "cmd"),
            (r"C:\x\gradlew.bat", "bat"),
            (r"C:\x\thing.ps1", "powershell"),
            (r"C:\x\thing.vbs", "windows-script-host"),
        ] {
            let error = spec("x", target).validate().expect_err("必须拒");
            match error {
                ShimError::ScriptTarget { kind: got, .. } => assert_eq!(got.as_str(), kind),
                other => panic!("应当是 script-target，实际：{other}"),
            }
        }
    }

    #[test]
    fn the_refusal_message_names_the_mechanism_not_just_the_rule() {
        // 只说"不支持 .cmd"会让人以为是我们偷懒；必须说清是 cmd 的引号剥离。
        let why = ScriptKind::Cmd.why();
        assert!(why.contains("COMSPEC"), "要指出走的是 cmd：{why}");
        assert!(why.contains("引号"), "要指出引号被剥离：{why}");
    }

    #[test]
    fn relative_and_odd_targets_are_refused() {
        assert!(matches!(
            spec("x", r"node.exe").validate(),
            Err(ShimError::TargetNotAbsolute(_))
        ));
        assert!(matches!(
            spec("x", r"C:\x\node.exe\").validate(),
            Err(ShimError::TargetEndsWithSeparator(_))
        ));
        assert!(matches!(
            spec("x", "C:\\x\\no\"de.exe").validate(),
            Err(ShimError::TargetContainsQuote(_))
        ));
    }

    #[test]
    fn unsafe_shim_names_are_refused() {
        for name in [
            "", "a/b", "a\\b", "a:b", "con", "CON.txt", "com1", "aux", "name.", "name ",
        ] {
            assert!(
                matches!(validate_name(name), Err(ShimError::BadName { .. })),
                "`{name}` 应当被拒"
            );
        }
        for name in ["node", "npm", "pip3.12", "python3", "consul", "console"] {
            assert!(validate_name(name).is_ok(), "`{name}` 应当通过");
        }
    }

    #[test]
    fn a_prefix_that_does_not_fit_is_refused_before_anything_is_written() {
        let mut long = spec("x", r"C:\x\node.exe");
        long.prefix_args = vec!["a".repeat(SLOT_PREFIX_UNITS)];
        assert!(matches!(
            long.validate(),
            Err(ShimError::PrefixTooLong { .. })
        ));
    }

    #[test]
    fn render_prefix_quotes_the_target_and_every_prefix_argument() {
        let full = ShimSpec {
            name: "npm".to_owned(),
            target: PathBuf::from(r"C:\a b\node.exe"),
            prefix_args: vec![r"C:\a b\npm-cli.js".to_owned(), "-x".to_owned()],
        };
        assert_eq!(
            full.render_prefix(),
            r#""C:\a b\node.exe" "C:\a b\npm-cli.js" -x"#
        );
        assert_eq!(full.file_name(), "npm.exe");
    }

    // ── 找模板 ──────────────────────────────────────────────────────

    #[test]
    fn a_template_beside_the_exe_is_found() {
        let dir = tuoen_platform::test_support::TempDir::new("shim-template");
        let exe_dir = dir.mkdir("app");
        std::fs::write(exe_dir.join(TEMPLATE_FILE_NAME), b"pretend").expect("写");
        let (found, source) = find_template_with(&exe_dir, None).expect("找到");
        assert_eq!(source, TemplateSource::BesideExe);
        assert_eq!(found, exe_dir.join(TEMPLATE_FILE_NAME));
    }

    #[test]
    fn a_missing_template_lists_every_place_it_looked() {
        let dir = tuoen_platform::test_support::TempDir::new("shim-template-missing");
        let exe_dir = dir.mkdir("app");
        let error = find_template_with(&exe_dir, None).expect_err("找不到");
        assert_eq!(error.kind(), "template-missing");
        assert!(error.is_user_error());
        let text = error.to_string();
        assert!(
            text.contains(&exe_dir.join(TEMPLATE_FILE_NAME).display().to_string()),
            "要把找过的位置列出来：{text}"
        );
        assert!(text.contains(TEMPLATE_ENV_VAR), "要提到逃生口：{text}");
    }

    #[test]
    fn the_env_var_wins_and_a_broken_one_says_so() {
        let dir = tuoen_platform::test_support::TempDir::new("shim-template-env");
        let exe_dir = dir.mkdir("app");
        let beside = exe_dir.join(TEMPLATE_FILE_NAME);
        std::fs::write(&beside, b"pretend").expect("写");
        let elsewhere = dir.join("custom.exe");
        std::fs::write(&elsewhere, b"pretend").expect("写");

        let (found, source) = find_template_with(&exe_dir, Some(elsewhere.clone())).expect("找到");
        assert_eq!(source, TemplateSource::EnvVar);
        assert_eq!(found, elsewhere);

        let broken = dir.join("nope.exe");
        let error = find_template_with(&exe_dir, Some(broken.clone())).expect_err("应当报错");
        let text = error.to_string();
        assert!(text.contains(&broken.display().to_string()), "{text}");
        assert!(
            text.contains(&beside.display().to_string()),
            "也要报出旁边那个：{text}"
        );
    }

    // ── 自指守卫 ────────────────────────────────────────────────────

    #[test]
    fn prefix_target_unquotes_the_path_we_wrote() {
        assert_eq!(
            prefix_target(r#""C:\a b\node.exe" "C:\a b\npm-cli.js" -x"#),
            Some(r"C:\a b\node.exe")
        );
        assert_eq!(prefix_target(r"C:\x\node.exe -y"), Some(r"C:\x\node.exe"));
        assert_eq!(prefix_target(r"C:\x\node.exe"), Some(r"C:\x\node.exe"));
        assert_eq!(prefix_target(""), None);
        assert_eq!(prefix_target(r#""unterminated"#), None);
    }

    #[test]
    fn same_file_sees_through_dots_case_and_the_file_name_itself() {
        let dir = tuoen_platform::test_support::TempDir::new("shim-same-file");
        let real = dir.join("thing.exe");
        std::fs::write(&real, b"MZ").expect("写");

        assert!(same_file(&real, &real));
        assert!(same_file(&real, &dir.join("THING.EXE")), "大小写不敏感");
        assert!(
            same_file(&dir.join("sub").join("..").join("thing.exe"), &real),
            "`..` 要折叠"
        );
        assert!(!same_file(&real, &dir.join("other.exe")));
        // 都不存在时不能误报。
        assert!(!same_file(&dir.join("a.exe"), &dir.join("b.exe")));
    }

    // ── 错误 ────────────────────────────────────────────────────────

    #[test]
    fn error_kinds_are_stable_lowercase_slugs_and_unique() {
        let samples = [
            ShimError::BadName {
                name: "x".into(),
                why: "y",
            },
            ShimError::TargetNotAbsolute(PathBuf::from("x")),
            ShimError::ScriptTarget {
                target: PathBuf::from("x"),
                kind: ScriptKind::Cmd,
            },
            ShimError::TargetContainsQuote(PathBuf::from("x")),
            ShimError::TargetEndsWithSeparator(PathBuf::from("x")),
            ShimError::PrefixTooLong { units: 1, limit: 2 },
            ShimError::PrefixContainsNul { index: 0 },
            ShimError::TargetUnreadable {
                target: PathBuf::from("x"),
                source: std::io::Error::other("boom"),
            },
            ShimError::TargetNotAFile(PathBuf::from("x")),
            ShimError::TemplateUnreadable {
                template: PathBuf::from("x"),
                source: std::io::Error::other("boom"),
            },
            ShimError::TemplateNotRecognized {
                template: PathBuf::from("x"),
                why: "y",
            },
            ShimError::TemplateMissing {
                tried: vec![],
                from_env: None,
            },
            ShimError::WriteFailed {
                path: PathBuf::from("x"),
                source: std::io::Error::other("boom"),
            },
            ShimError::SelfCheckFailed {
                path: PathBuf::from("x"),
            },
            ShimError::DestinationIsTarget(PathBuf::from("x")),
            ShimError::DestinationIsTemplate(PathBuf::from("x")),
            ShimError::DestinationNotAShim(PathBuf::from("x")),
        ];
        let mut kinds = Vec::new();
        for error in &samples {
            let kind = error.kind();
            assert!(
                kind.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "`{kind}` 不是小写 ASCII slug"
            );
            // 每条都必须能给出一句中文（Display 不能是空的、不能是 Debug 的转储）。
            let text = error.to_string();
            assert!(text.chars().count() > 6, "`{kind}` 的说明太短：{text}");
            kinds.push(kind);
        }
        let mut sorted = kinds.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), kinds.len(), "kind 有重复：{kinds:?}");
    }
}
