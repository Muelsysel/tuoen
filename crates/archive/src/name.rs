//! 条目名校验：**解压前的第一层防线**。
//!
//! ## 为什么我们自己也要校验（bsdtar 不是已经挡了吗）
//!
//! 实测（`research/BSDTAR_SAFETY_MEASURED.md`）：bsdtar 3.8.8 对**路径逃逸**
//! 是可靠的（`..` 拒绝、symlink 拒绝、硬链接拒绝、fifo/设备拒绝），
//! 但它对**名字的危险形态**是不可靠的 —— 它要么静默改名，要么创建出
//! 普通工具碰不了的文件：
//!
//! | 形态 | bsdtar 的行为 |
//! |---|---|
//! | `CON` / `sub/NUL.txt` | **真的创建**（用 `\\?\` 绕过 Win32），而创建出来的文件普通路径**看不见** |
//! | `trailing.` / `trailing ` | **真的创建**，而 Win32 会吃掉结尾的点与空格 → 打不开、删不掉 |
//! | `stream.txt:evil` | **静默改名**成 `stream.txt_evil`，无提示 |
//! | `Readme.txt` + `README.TXT` | **静默覆盖**（NTFS 大小写不敏感），只剩一个 |
//!
//! 所以这一层不是"重复劳动"，它挡的是 bsdtar **明确不挡**的那一类。
//! 而且它能在**动手之前**给出具体原因，而不是解压完发现目录里有残骸。
//!
//! ## 校验是"拒绝"，不是"净化"
//!
//! 净化（把 `CON` 改成 `_CON`、把 `:` 换成 `_`）看着更友好，实际更危险：
//! 它让归档可以**声明一个名字、落盘成另一个**，而调用方与用户都无法知道
//! 发生了什么。bsdtar 就是这么做的，而实测证明那会带来"名字变了没提示"。
//!
//! 这里的判据很简单：**归档声明什么，就必须落成什么；做不到就整个拒绝。**

use crate::error::NameViolation;
use crate::limits::ExtractLimits;

/// 一个通过校验的条目名。
///
/// **只能由 [`validate_entry_name`] 造出来** —— 这是类型层面的保证：
/// 任何拿到 `SafeName` 的代码都不需要再检查一遍。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SafeName {
    /// 规范化后的相对路径，用 `/` 分隔（归档里的原生分隔符）。
    normalized: String,
    /// 这是一个**目录条目**（归档里的名字以 `/` 结尾）。
    ///
    /// 为什么要记住这件事：`strip_components` 会让"只有一段的顶层目录标记"
    /// 整个消失，而那是**完全正常**的（每个 tar/zip 都有 `topdir/` 这一条）。
    /// 同样消失的如果是一个**文件**，那就是"内容被静默丢掉"。两者必须区分。
    is_dir_entry: bool,
}

impl SafeName {
    /// 规范化后的相对路径（`/` 分隔）。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.normalized
    }

    /// 这是不是一个目录条目（归档里以 `/` 结尾）。
    #[must_use]
    pub const fn is_dir_entry(&self) -> bool {
        self.is_dir_entry
    }

    /// 路径有几段。
    #[must_use]
    pub fn depth(&self) -> usize {
        self.normalized.split('/').count()
    }

    /// 去掉最前面 `n` 段之后的名字。`n` 大于等于段数时返回 `None`
    /// （这个条目在 `strip_components` 之后就不存在了）。
    #[must_use]
    pub fn strip_components(&self, n: usize) -> Option<String> {
        if n == 0 {
            return Some(self.normalized.clone());
        }
        let mut parts = self.normalized.splitn(n + 1, '/');
        for _ in 0..n {
            parts.next()?;
        }
        parts.next().map(str::to_owned)
    }
}

/// 保留设备名。**大小写不敏感**（Windows 上 `con` 与 `CON` 一样）。
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 上标数字形式的保留名（`COM¹` 也是设备）。
///
/// 这不是过度设计：Windows 的文档明确把 `COM¹` / `LPT¹` 列进保留名，
/// 而它们**不在** ASCII 范围内，所以一个只查 `COM1` 的实现会漏掉。
const RESERVED_SUPERSCRIPTS: &[char] = &['\u{00b9}', '\u{00b2}', '\u{00b3}'];

/// 校验一个条目名。
///
/// `name` 是归档里**原样**的名字（`/` 或 `\` 分隔都可能）。
///
/// 名字以 `/` 结尾表示**目录条目**（每个 tar/zip 都有 `topdir/` 这一条），
/// 这完全合法，结尾的斜杠会被去掉。
///
/// # Errors
///
/// 见 [`NameViolation`] —— 每一条规则一个变体，因为"解压失败"不足以
/// 判断是上游问题还是攻击。
pub fn validate_entry_name(name: &str, limits: &ExtractLimits) -> Result<SafeName, NameViolation> {
    match validate_treating_backslash_as_separator(name, limits) {
        Ok(safe) => Ok(safe),
        Err(violation) => {
            // **报哪一种违规，要看哪一种解释更可能是真的。**
            //
            // `-tf` 的输出是**有损的**：实测 bsdtar 把名字里的控制字符
            // 转义成 `\n` / `\r` / `\t`，所以列表里一个 `\n` 可能是
            // ① 真的换行被转义，也可能是 ② Windows 风格的分隔符。
            // **两种解释在列表里长得一模一样**（实测：
            // `bin\node.exe`（真目录 + 文件）与"名字含换行的 `bin<LF>ode.exe`"）。
            //
            // 规则（三种情况都验过）：
            //
            // * 分隔符解释**通过** → 接受它。真的是控制字符的话，
            //   磁盘审计会看到真名并拒绝（那时才报 ControlCharacter）。
            //   这一条不能反着做：7z 的条目名**就是用反斜杠**的
            //   （实测 npmmirror 那份 node 7z 列出来是
            //   `node-v24.19.0-win-x64\node.exe`），把 `\n` 一律当控制字符
            //   会**拒绝掉全部 7z 制品**。
            // * 分隔符解释**不通过**，且名字里有转义序列 → 报
            //   `ControlCharacter`。真名里确实有控制字符，
            //   而那比"`n..` 这个段以点结尾"准确得多。
            // * 分隔符解释**不通过**，名字里没有转义序列 → 报分隔符
            //   解释给出的那条（`..\escaped.txt` 报 ParentTraversal，
            //   因为 `\e` 不是转义序列）。
            if looks_like_bsdtar_escape(name) {
                Err(NameViolation::ControlCharacter)
            } else {
                Err(violation)
            }
        }
    }
}

/// 名字是不是以 UNC 前缀开头。
///
/// **不是"以两个分隔符开头"那么简单**：UNC 需要 `//` 后面紧跟**非分隔符**
/// （服务器名）。所以 `//server/share/x` 是 UNC，而 `///x` 与 `///` 只是
/// "以分隔符开头的普通路径" —— 报 `AbsolutePath` 更准确。
fn starts_with_unc_prefix(name: &str) -> bool {
    let stripped = name.strip_prefix("//").or_else(|| name.strip_prefix(r"\\"));
    match stripped {
        Some(rest) => !rest.starts_with(['/', '\\']),
        None => false,
    }
}

/// 名字里有没有 bsdtar 在 `-tf` 里用的**转义序列**。
///
/// 实测 bsdtar 的转义形式（`research/BSDTAR_SAFETY_MEASURED.md` §4）：
/// `\n`（换行）、`\r`（回车）、`\t`、以及别的不可打印字符用三位八进制
/// `\ooo`。
///
/// **这里刻意不查两种东西**：
///
/// * **`\\`（转义的反斜杠）**：它**不是控制字符**，而真名里含反斜杠时
///   "反斜杠是分隔符"本来就是对的解释。更要紧的是，把 `\\` 当转义会让
///   `\\server\share\x.txt` 报成 `ControlCharacter` 而不是 `UncPath`
///   —— 后者准确得多，而 UNC 正是要防的东西。
/// * **`\` 后面跟单个数字**：三位八进制 `\ooo` 才是转义，而 `\7zip\x`
///   是一个合法的 Windows 风格分隔符路径。
///
/// 少认一种转义最多让诊断理由差一点（仍然会拒绝，因为路径本身违规），
/// 多认一种会让正经归档被误拒 —— 后者贵得多。
fn looks_like_bsdtar_escape(name: &str) -> bool {
    let bytes = name.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            index += 1;
            continue;
        }
        let Some(&next) = bytes.get(index + 1) else {
            return false; // 结尾的孤立反斜杠不是转义
        };
        if matches!(next, b'a' | b'b' | b'f' | b'n' | b'r' | b't' | b'v') {
            return true;
        }
        // 三位八进制。
        if next.is_ascii_digit() && next < b'8' {
            let octal = bytes.get(index + 1..index + 4);
            if let Some([a, b, c]) = octal
                && (b'0'..=b'7').contains(a)
                && (b'0'..=b'7').contains(b)
                && (b'0'..=b'7').contains(c)
            {
                return true;
            }
        }
        index += 1;
    }
    false
}

/// 真正的校验逻辑，**假定反斜杠是分隔符**。
fn validate_treating_backslash_as_separator(
    name: &str,
    limits: &ExtractLimits,
) -> Result<SafeName, NameViolation> {
    if name.is_empty() {
        return Err(NameViolation::Empty);
    }

    // **先把反斜杠统一成斜杠**：实测 `..\escaped-backslash.txt` 是真实存在的
    // 攻击形态（bsdtar 拒了它），而只查 `/` 的实现会漏掉。
    let unified = name.replace('\\', "/");

    // 控制字符要在切分之前查 —— 否则一个含换行的名字会伪装成两个条目。
    if unified.chars().any(|c| c.is_control()) {
        return Err(NameViolation::ControlCharacter);
    }

    // **结尾的斜杠 = 目录条目**，合法。去掉它再校验剩下的部分。
    // （不去掉的话 `topdir/` 会切出一个空段，被误判成 `EmptyOrDotSegment`
    // —— 那会让**每一个真实归档**都被拒绝。）
    let is_dir_entry = unified.ends_with('/');
    let unified = unified.trim_end_matches('/');
    if unified.is_empty() {
        // 全是分隔符（`/`、`//`、`///`）。**不报 `Empty`**：
        // 它确实是空的，但它更是"以分隔符开头"，而那条规则才解释了
        // 为什么它危险。报 `Empty` 会让用户以为归档里有个空名字。
        return Err(if starts_with_unc_prefix(name) {
            NameViolation::UncPath
        } else {
            NameViolation::AbsolutePath
        });
    }

    // UNC 必须先于"绝对路径"查：`//server/share` 也是"以 / 开头"。
    if starts_with_unc_prefix(unified) {
        return Err(NameViolation::UncPath);
    }

    // 盘符：`C:/x` 或 `C:x`（后者是**相对当前目录的盘符路径**，一样危险）。
    let bytes = unified.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(NameViolation::DriveLetter);
    }

    if unified.starts_with('/') {
        return Err(NameViolation::AbsolutePath);
    }

    // `:` 剩下唯一的可能就是在名字中间 —— ADS 写原语。
    // 放在盘符之后查，这样 `C:/x` 报的是更准确的 DriveLetter。
    if unified.contains(':') {
        return Err(NameViolation::AlternateDataStream);
    }

    if unified.len() > limits.max_relative_path {
        return Err(NameViolation::TooLong);
    }

    let mut segments: Vec<&str> = Vec::new();
    for segment in unified.split('/') {
        if segment.is_empty() || segment == "." {
            return Err(NameViolation::EmptyOrDotSegment);
        }
        if segment == ".." {
            return Err(NameViolation::ParentTraversal);
        }
        if segment.ends_with('.') || segment.ends_with(' ') {
            return Err(NameViolation::TrailingDotOrSpace);
        }
        if is_reserved_device_name(segment) {
            return Err(NameViolation::ReservedDeviceName);
        }
        segments.push(segment);
    }

    if segments.len() > limits.max_depth {
        return Err(NameViolation::TooDeep);
    }

    Ok(SafeName {
        normalized: segments.join("/"),
        is_dir_entry,
    })
}

/// 这个名字（不含路径，只是最后一段）是不是保留设备名。
///
/// Windows 的规则是**看基名**：`CON.txt` 与 `CON` 一样是设备，
/// 而 `CONsole.txt` 不是。所以先砍掉第一个 `.` 之后的部分。
#[must_use]
pub fn is_reserved_device_name(segment: &str) -> bool {
    // `CON.txt` → `CON`；`NUL` → `NUL`；`a.b.c` → `a`
    let base = segment.split('.').next().unwrap_or(segment);
    let upper = base.to_ascii_uppercase();

    if RESERVED_NAMES.contains(&upper.as_str()) {
        return true;
    }

    // `COM¹` / `LPT¹` 形式：ASCII 前缀 + 上标数字。
    for prefix in ["COM", "LPT"] {
        if let Some(rest) = upper.strip_prefix(prefix) {
            let mut chars = rest.chars();
            if let Some(first) = chars.next()
                && RESERVED_SUPERSCRIPTS.contains(&first)
                && chars.next().is_none()
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ExtractLimits {
        ExtractLimits::default()
    }

    fn ok(name: &str) -> String {
        validate_entry_name(name, &limits())
            .unwrap_or_else(|violation| panic!("`{name}` 应当通过，却被判成 {violation}"))
            .as_str()
            .to_owned()
    }

    fn bad(name: &str) -> NameViolation {
        validate_entry_name(name, &limits())
            .err()
            .unwrap_or_else(|| panic!("`{name}` 应当被拒，却通过了"))
    }

    #[test]
    fn ordinary_names_pass_and_get_normalized() {
        assert_eq!(ok("node.exe"), "node.exe");
        assert_eq!(
            ok("node-v24.19.0-win-x64/node.exe"),
            "node-v24.19.0-win-x64/node.exe"
        );
        // 反斜杠统一成斜杠 —— 归档里两种都有。
        assert_eq!(ok(r"bin\node.exe"), "bin/node.exe");
        // **含空格与版本号**是最糟的真实路径（本机 PATH 上就有
        // `C:\Dev\IDE\IDEA26\IntelliJ IDEA 2026.1.1\bin`），必须通过。
        assert_eq!(
            ok("IntelliJ IDEA 2026.1.1/bin/idea64.exe"),
            "IntelliJ IDEA 2026.1.1/bin/idea64.exe"
        );
        // 非 ASCII 也合法（安装显示名满是 CJK）。
        assert_eq!(ok("文档/说明.txt"), "文档/说明.txt");
        // `CONsole.txt` 不是设备名。
        assert_eq!(ok("CONsole.txt"), "CONsole.txt");
        assert_eq!(ok("COM10.txt"), "COM10.txt");
    }

    #[test]
    fn directory_entries_ending_in_a_slash_are_accepted() {
        // **这一条挡的是一个会让整个功能不可用的 bug。**
        // 实测 `tar.exe -tf` 把目录条目列成 `node-v24.19.0-win-x64/`
        // （带结尾斜杠），而每个真实 tar/zip 都有这一条。
        // 不处理结尾斜杠的话它会切出一个空段 → `EmptyOrDotSegment`
        // → **每一个真实归档都被拒绝**。
        let name = validate_entry_name("node-v24.19.0-win-x64/", &limits()).expect("目录条目合法");
        assert_eq!(name.as_str(), "node-v24.19.0-win-x64");
        assert!(name.is_dir_entry());

        // 嵌套的目录条目也一样。
        let nested = validate_entry_name("a/b/c/", &limits()).expect("嵌套目录条目合法");
        assert_eq!(nested.as_str(), "a/b/c");
        assert!(nested.is_dir_entry());
        assert_eq!(nested.depth(), 3);

        // 普通文件条目**不是**目录条目 —— 这个区分是有用的（见 `extract`）。
        assert!(
            !validate_entry_name("node.exe", &limits())
                .unwrap()
                .is_dir_entry()
        );

        // 但结尾斜杠**不能**变成绕过校验的后门：
        assert_eq!(bad("../"), NameViolation::ParentTraversal);
        assert_eq!(bad("/abs/"), NameViolation::AbsolutePath);
        assert_eq!(bad("CON/"), NameViolation::ReservedDeviceName);
        assert_eq!(bad("trailing./"), NameViolation::TrailingDotOrSpace);
    }

    #[test]
    fn the_escape_ambiguity_is_resolved_the_way_the_evidence_says() {
        // 实测：bsdtar 在 `-tf` 里把控制字符转义成 `\n` / `\r` / `\t`，
        // 所以列表里一个 `\n` 有**两种**解释，而两种解释长得一样。
        // 下面四条把三种情况钉住。

        // ① 分隔符解释通过 → 接受它。
        //    **这条不能反着做**：7z 的条目名就是用反斜杠的
        //    （实测 npmmirror 那份 node 7z 列出来是
        //    `node-v24.19.0-win-x64\node.exe`）。把 `\n` 一律当控制字符
        //    会拒绝掉**全部 7z 制品**。
        assert_eq!(
            ok(r"node-v24.19.0-win-x64\node.exe"),
            "node-v24.19.0-win-x64/node.exe"
        );

        // ② 分隔符解释不通过 + 名字里有转义序列 → 报 ControlCharacter。
        //    真名是 `evil<LF>../escaped.txt`，比"`n..` 这个段以点结尾"准确得多。
        assert_eq!(
            bad(r"evil\n../escaped.txt"),
            NameViolation::ControlCharacter
        );

        // ③ 分隔符解释不通过 + 没有转义序列 → 报分隔符解释给出的那条。
        //    `\e` 不是转义序列，所以 `..\escaped.txt` 就是纯粹的路径穿越。
        assert_eq!(bad(r"..\escaped.txt"), NameViolation::ParentTraversal);

        // ④ `\\` **不算转义**：它会让 `\\server\share\x` 报成
        //    ControlCharacter，而它其实是 UNC —— 后者准确得多。
        assert_eq!(bad(r"\\server\share\x.txt"), NameViolation::UncPath);

        // ⑤ `\7zip` 不算转义（三位八进制才是），所以它走分隔符解释。
        assert_eq!(ok(r"dir\7zip\x.txt"), "dir/7zip/x.txt");
        //    而真的三位八进制转义会被认出来。
        assert_eq!(bad(r"evil\012../x"), NameViolation::ControlCharacter);
    }

    // ---- Zip Slip 的每一种变体各一条 ----

    #[test]
    fn parent_traversal_is_rejected_in_every_spelling() {
        assert_eq!(bad("../escaped.txt"), NameViolation::ParentTraversal);
        assert_eq!(
            bad("a/b/../../../escaped.txt"),
            NameViolation::ParentTraversal
        );
        // **反斜杠形态**：实测这是真实存在的攻击（bsdtar 拒了它）。
        assert_eq!(bad(r"..\escaped.txt"), NameViolation::ParentTraversal);
        assert_eq!(bad("a/.."), NameViolation::ParentTraversal);
        assert_eq!(bad(".."), NameViolation::ParentTraversal);
    }

    #[test]
    fn absolute_paths_are_rejected_in_every_spelling() {
        assert_eq!(bad("/escaped-abs.txt"), NameViolation::AbsolutePath);
        assert_eq!(bad(r"\escaped-abs.txt"), NameViolation::AbsolutePath);
        // UNC 单独一类，因为它是"绝对路径"里最危险的一种。
        assert_eq!(bad("//server/share/x.txt"), NameViolation::UncPath);
        assert_eq!(bad(r"\\server\share\x.txt"), NameViolation::UncPath);
    }

    #[test]
    fn drive_letters_are_rejected_including_drive_relative() {
        assert_eq!(bad("C:/escaped.txt"), NameViolation::DriveLetter);
        assert_eq!(bad(r"C:\escaped.txt"), NameViolation::DriveLetter);
        // **`C:x`（无分隔符）是"当前目录在该盘上"的相对路径** —— 它看起来
        // 人畜无害，实际会写到进程的当前目录，而那是攻击者挑的。
        assert_eq!(bad("C:escaped.txt"), NameViolation::DriveLetter);
        assert_eq!(bad("c:/x"), NameViolation::DriveLetter);
    }

    #[test]
    fn reserved_device_names_are_rejected_including_with_extensions_and_subpaths() {
        for name in ["CON", "con", "NUL", "aux", "PRN", "COM1", "LPT9"] {
            assert_eq!(bad(name), NameViolation::ReservedDeviceName, "{name}");
        }
        // 带扩展名一样是设备。
        assert_eq!(bad("NUL.txt"), NameViolation::ReservedDeviceName);
        assert_eq!(bad("con.exe"), NameViolation::ReservedDeviceName);
        // **在子目录里也一样** —— 实测 `sub/NUL.txt` 真的被 bsdtar 创建了。
        assert_eq!(bad("sub/NUL.txt"), NameViolation::ReservedDeviceName);
        // 上标形式。
        assert_eq!(bad("COM\u{00b9}"), NameViolation::ReservedDeviceName);
        assert_eq!(bad("LPT\u{00b3}.txt"), NameViolation::ReservedDeviceName);
    }

    #[test]
    fn trailing_dots_and_spaces_are_rejected() {
        assert_eq!(bad("trailing."), NameViolation::TrailingDotOrSpace);
        assert_eq!(bad("trailing "), NameViolation::TrailingDotOrSpace);
        assert_eq!(bad("dir./file.txt"), NameViolation::TrailingDotOrSpace);
        assert_eq!(bad("dir /file.txt"), NameViolation::TrailingDotOrSpace);
    }

    #[test]
    fn alternate_data_stream_writes_are_rejected() {
        assert_eq!(bad("stream.txt:evil"), NameViolation::AlternateDataStream);
        assert_eq!(bad("a/b:c"), NameViolation::AlternateDataStream);
    }

    #[test]
    fn control_characters_are_rejected() {
        assert_eq!(bad("evil\n../escaped.txt"), NameViolation::ControlCharacter);
        assert_eq!(bad("has\rcarriage.txt"), NameViolation::ControlCharacter);
        assert_eq!(bad("bin\u{0}ode.exe"), NameViolation::ControlCharacter);
        assert_eq!(bad("del\u{7f}.txt"), NameViolation::ControlCharacter);
    }

    #[test]
    fn empty_and_dot_segments_are_rejected() {
        assert_eq!(bad("a//b"), NameViolation::EmptyOrDotSegment);
        assert_eq!(bad("a/./b"), NameViolation::EmptyOrDotSegment);
        assert_eq!(bad(""), NameViolation::Empty);
        // **退化名字报"更准确"的那一条**：`/` 与 `///` 确实是空的，
        // 但它们更是"以分隔符开头" —— 而后者解释了为什么危险。
        assert_eq!(bad("/"), NameViolation::AbsolutePath);
        assert_eq!(bad("///"), NameViolation::AbsolutePath);
        assert_eq!(bad(r"\\"), NameViolation::UncPath);
        assert_eq!(bad("./x"), NameViolation::EmptyOrDotSegment);
    }

    #[test]
    fn depth_and_length_limits_are_enforced() {
        // 默认上限是 64 层 —— 所以这里要**超过**它，不能用 40。
        let deep = (0..70)
            .map(|i| format!("d{i}"))
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(
            validate_entry_name(&deep, &limits()).unwrap_err(),
            NameViolation::TooDeep
        );

        let long = format!("{}.txt", "a".repeat(5000));
        assert_eq!(
            validate_entry_name(&long, &limits()).unwrap_err(),
            NameViolation::TooLong
        );
    }

    #[test]
    fn the_order_of_checks_is_deliberate() {
        // `C:/x` 同时是"带盘符"与"含 `:`"。报哪一个？
        // **报更准确的那个** —— DriveLetter 解释了为什么，而
        // AlternateDataStream 会让人以为攻击者在写 ADS。
        assert_eq!(bad("C:/x"), NameViolation::DriveLetter);
        // `//server/share` 同时是"以 / 开头"与 UNC。UNC 更准确。
        assert_eq!(bad("//server/share/x"), NameViolation::UncPath);
        // `a/b/../c.` 同时有 `..` 与结尾的点。**报 `..`** —— 因为段是
        // 按顺序检查的，而 `..` 是那个真正会写到目录外面的东西；
        // 结尾的点只是"删不掉"。先报更严重的那个。
        assert_eq!(bad("a/b/../c."), NameViolation::ParentTraversal);
    }

    #[test]
    fn strip_components_counts_segments() {
        let name = validate_entry_name("node-v24.19.0-win-x64/bin/node.exe", &limits()).unwrap();
        assert_eq!(name.depth(), 3);
        assert_eq!(
            name.strip_components(0).as_deref(),
            Some("node-v24.19.0-win-x64/bin/node.exe")
        );
        assert_eq!(name.strip_components(1).as_deref(), Some("bin/node.exe"));
        assert_eq!(name.strip_components(2).as_deref(), Some("node.exe"));
        // 剥掉全部段之后这个条目就不存在了 —— 返回 `None` 而不是空串，
        // 因为空串是一个**合法但错误**的路径。
        assert_eq!(name.strip_components(3), None);
        assert_eq!(name.strip_components(99), None);
    }
}
