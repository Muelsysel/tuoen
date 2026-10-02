//! 最薄的一层 Win32 封装：只把 C 形状翻译回 Rust 形状，**不含任何策略**。
//!
//! # 为什么用 `windows-sys` 而不是自己写 `extern "system"`
//!
//! 1. **GNU target 上的链接**。本机没有 MSVC（`AGENTS.md`"构建这台机器"三条实测发现），
//!    只能走 `x86_64-pc-windows-gnu`。`windows-sys` 通过 `windows-link` 生成
//!    `#[link(name = "advapi32")]` 这种声明，由 MinGW 自带的 import library 满足；
//!    手写 `extern "system"` 要自己保证 dll 名、调用约定与结构体布局全部正确，
//!    而布局抄错的后果是**静默读错字段**（不报错、拿到垃圾值）。
//! 2. 结构体布局（`WIN32_FIND_DATAW`）与常量由上游维护，不靠人抄。
//!
//! **只开真正用到的 feature**：`Win32_Foundation`（HANDLE / 错误码）、
//! `Win32_Storage_FileSystem`（`FindFirstFileW`）、`Win32_System_Registry`（注册表枚举）。
//! 三个 reparse tag 常量在 `windows-sys` 里散落在 `Win32::System::SystemServices` 与
//! `Win32::System::Ioctl` —— 为两个常量引入两个 feature 不值，改为在本文件用
//! Win32 头文件里的名字定义，并加测试钉住取值（见本文件底部的测试）。
//!
//! # 为什么用 `FindFirstFileW` 而不是 `GetFileAttributesW` + `DeviceIoControl`
//!
//! `FindFirstFileW` **一次调用同时给出属性、大小与 reparse tag**（`dwReserved0`），
//! 而"读得到 tag"正是 `alias-ghost` 判定的全部依据：本机实测
//! `…\WindowsApps\python.exe` 长度 0、tag `0x8000001b`，而 `Get-Command python` **成功**。
//! 走 `DeviceIoControl(FSCTL_GET_REPARSE_POINT)` 需要额外开 `Win32_Security`
//! （`SECURITY_ATTRIBUTES`）与 `Win32_System_IO` 两个 feature，还得自己声明
//! `REPARSE_DATA_BUFFER` 那个联合体，换来的信息是同一个 tag。
//!
//! # 只读
//!
//! 本文件里的每个函数都是**读**：`FindFirstFileW` / `RegOpenKeyExW` /
//! `RegEnumKeyExW` / `RegEnumValueW`。没有任何写注册表、写文件、改 reparse point 的调用，
//! 这是 ticket #11"只读检测"在代码层面的落点。

use std::path::Path;

/// Win32 码。**判断逻辑只看码，不看文本**（`AGENTS.md`：本机 zh-CN，错误文本是本地化的）。
pub type Win32Code = u32;

/// `ERROR_FILE_NOT_FOUND`
pub const ERROR_FILE_NOT_FOUND: Win32Code = 2;
/// `ERROR_PATH_NOT_FOUND`
pub const ERROR_PATH_NOT_FOUND: Win32Code = 3;
/// `ERROR_ACCESS_DENIED`
pub const ERROR_ACCESS_DENIED: Win32Code = 5;
/// `ERROR_NOT_SUPPORTED` —— 也用于"本平台没有这个能力"。
pub const ERROR_NOT_SUPPORTED: Win32Code = 50;
/// `ERROR_INVALID_NAME` —— `FindFirstFileW` 拒绝的模式字符。
pub const ERROR_INVALID_NAME: Win32Code = 123;
/// `ERROR_MORE_DATA`
pub const ERROR_MORE_DATA: Win32Code = 234;
/// `ERROR_NO_MORE_ITEMS`
pub const ERROR_NO_MORE_ITEMS: Win32Code = 259;

/// 一个路径的文件系统事实，来自 `FindFirstFileW`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindFacts {
    /// 目录位置位。
    pub is_dir: bool,
    /// `nFileSizeHigh << 32 | nFileSizeLow`。目录恒为 0。
    pub size: u64,
    /// 仅当 `FILE_ATTRIBUTE_REPARSE_POINT` 置位时有值，取自 `dwReserved0`。
    pub reparse_tag: Option<u32>,
}

/// 注册表根键。**只有两个**：`WOW6432Node` 不是根键，是 `HKLM\SOFTWARE` 下的一个子键
/// （见 `crate::registry::RegHive`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKey {
    /// `HKEY_CURRENT_USER`
    CurrentUser,
    /// `HKEY_LOCAL_MACHINE`
    LocalMachine,
}

/// 枚举出来的一条注册表值。字节与类型原样带出，解释交给 `crate::registry`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRegValue {
    /// 值名。默认值（"默认"）的名字是空字符串。
    pub name: String,
    /// `REG_*` 类型码。
    pub kind: u32,
    /// 原始数据字节。`REG_SZ` / `REG_EXPAND_SZ` / `REG_MULTI_SZ` 是 UTF-16LE。
    pub bytes: Vec<u8>,
}

#[cfg(windows)]
mod imp {
    use std::iter;
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::Path;
    use std::ptr;

    use windows_sys::Win32::Foundation::{
        GetLastError, ERROR_SUCCESS, WIN32_ERROR,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        INVALID_HANDLE_VALUE, WIN32_FIND_DATAW,
    };
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
        HKEY_LOCAL_MACHINE, KEY_READ,
    };

    use super::{FindFacts, RawRegValue, RootKey, Win32Code, ERROR_INVALID_NAME};

    /// UTF-16 + NUL，供 `*W` 系列调用。
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(iter::once(0)).collect()
    }

    /// `FindFirstFileW` 的路径准备。
    ///
    /// 两个必须处理的坑：
    /// - 它**把 `*` / `?` 当通配符**。检测路径里出现这两个字符不是正常情况，但真出现时
    ///   宁可报"查不到"（`ERROR_INVALID_NAME`）也不能当成模式匹配 —— 否则
    ///   `C:\*` 会被报告成"存在"。
    /// - 它**拒绝尾部分隔符**（`C:\dir\` 直接失败），而 `PATH` 上的条目真的会带尾部分隔符
    ///   （本机 48 条里就有）。所以削掉，但**保住裸盘符**：`C:\` 削成 `C:` 会变成
    ///   "C 盘当前目录"，语义变了。
    /// `pub(super)`：本文件底部的测试要直接验这段纯逻辑（它不碰文件系统）。
    pub(super) fn find_pattern(path: &Path) -> Result<Vec<u16>, Win32Code> {
        let text = path.as_os_str().to_string_lossy();
        if text.contains(['*', '?']) {
            return Err(ERROR_INVALID_NAME);
        }
        let had_separator = text.ends_with(['\\', '/']);
        let trimmed = text.trim_end_matches(['\\', '/']);
        // 裸盘符：`C:\` 削成 `C:` 会变成"C 盘当前目录"，语义变了 —— 把分隔符补回去。
        let normalized = if had_separator && trimmed.len() == 2 && trimmed.ends_with(':') {
            format!("{trimmed}\\")
        } else {
            trimmed.to_owned()
        };
        Ok(wide(&normalized))
    }

    pub fn find_first(path: &Path) -> Result<FindFacts, Win32Code> {
        let pattern = find_pattern(path)?;
        let mut data = WIN32_FIND_DATAW::default();
        // SAFETY: `pattern` 是以 NUL 结尾的 UTF-16 缓冲，`data` 是本栈上正确对齐的
        // `WIN32_FIND_DATAW`（由 windows-sys 声明布局）。`FindFirstFileW` 只写 `data`。
        let handle = unsafe { FindFirstFileW(pattern.as_ptr(), &mut data) };
        if handle == INVALID_HANDLE_VALUE {
            // SAFETY: 紧接失败调用之后取 `GetLastError`，中间没有其它可能覆盖它的调用。
            return Err(unsafe { GetLastError() });
        }
        // SAFETY: `handle` 是上面成功返回的查找句柄，只关一次。
        unsafe { FindClose(handle) };

        let attributes = data.dwFileAttributes;
        Ok(FindFacts {
            is_dir: attributes & FILE_ATTRIBUTE_DIRECTORY != 0,
            size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
            // 只有置了 reparse 位才读 `dwReserved0`：MSDN 明确说该字段在非 reparse 时无效。
            reparse_tag: if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                Some(data.dwReserved0)
            } else {
                None
            },
        })
    }

    fn root_handle(root: RootKey) -> HKEY {
        match root {
            RootKey::CurrentUser => HKEY_CURRENT_USER,
            RootKey::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }

    /// 一个 `HKEY`，`Drop` 时关闭。**没有写方法** —— 这个包装器就是为了让"只读"成为类型事实。
    struct OpenKey(HKEY);

    impl Drop for OpenKey {
        fn drop(&mut self) {
            // SAFETY: 句柄来自 `RegOpenKeyExW` 成功返回，且只在这里关闭一次。
            let _ = unsafe { RegCloseKey(self.0) };
        }
    }

    fn open(root: RootKey, subkey: &str) -> Result<OpenKey, Win32Code> {
        let subkey = wide(subkey);
        let mut hkey: HKEY = ptr::null_mut();
        // SAFETY: `subkey` 以 NUL 结尾；`&mut hkey` 是本栈上的有效出参。
        let rc: WIN32_ERROR =
            unsafe { RegOpenKeyExW(root_handle(root), subkey.as_ptr(), 0, KEY_READ, &mut hkey) };
        if rc != ERROR_SUCCESS {
            return Err(rc);
        }
        Ok(OpenKey(hkey))
    }

    pub fn reg_subkeys(root: RootKey, subkey: &str) -> Result<Vec<String>, Win32Code> {
        let key = open(root, subkey)?;
        let mut out = Vec::new();
        let mut index: u32 = 0;
        let mut name = vec![0u16; 512];
        loop {
            let mut len = u32::try_from(name.len()).unwrap_or(u32::MAX);
            // SAFETY: 句柄有效；`name` 是可写缓冲，`len` 是它的长度；类名与最后写入时间
            // 传空指针，因为 `RegEnumKeyExW` 允许这两者不关心。
            let rc: WIN32_ERROR = unsafe {
                RegEnumKeyExW(
                    key.0,
                    index,
                    name.as_mut_ptr(),
                    &mut len,
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            match rc {
                ERROR_SUCCESS => {
                    out.push(String::from_utf16_lossy(&name[..len as usize]));
                    index += 1;
                }
                super::ERROR_MORE_DATA => name.resize(name.len() * 2, 0),
                super::ERROR_NO_MORE_ITEMS => break,
                other => return Err(other),
            }
        }
        Ok(out)
    }

    pub fn reg_values(root: RootKey, subkey: &str) -> Result<Vec<RawRegValue>, Win32Code> {
        let key = open(root, subkey)?;
        let mut out = Vec::new();
        let mut index: u32 = 0;
        let mut name = vec![0u16; 512];
        let mut data = vec![0u8; 4096];
        loop {
            let mut name_len = u32::try_from(name.len()).unwrap_or(u32::MAX);
            let mut kind: u32 = 0;
            let mut data_len = u32::try_from(data.len()).unwrap_or(u32::MAX);
            // SAFETY: 同 `reg_subkeys`；`kind` / `data_len` 是有效出参，
            // `reserved` 按 MSDN 必须传空。
            let rc: WIN32_ERROR = unsafe {
                RegEnumValueW(
                    key.0,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    ptr::null(),
                    &mut kind,
                    data.as_mut_ptr(),
                    &mut data_len,
                )
            };
            match rc {
                ERROR_SUCCESS => {
                    data.truncate(data_len as usize);
                    out.push(RawRegValue {
                        name: String::from_utf16_lossy(&name[..name_len as usize]),
                        kind,
                        bytes: std::mem::take(&mut data),
                    });
                    data = vec![0u8; 4096];
                    index += 1;
                }
                // 名字或数据任一放不下都是 `ERROR_MORE_DATA`，两个都长。
                super::ERROR_MORE_DATA => {
                    name.resize(name.len() * 2, 0);
                    data.resize(data_len as usize, 0);
                }
                super::ERROR_NO_MORE_ITEMS => break,
                other => return Err(other),
            }
        }
        Ok(out)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    use super::{FindFacts, RawRegValue, RootKey, Win32Code, ERROR_NOT_SUPPORTED};

    /// 非 Windows 上**故意不提供任何实现**：`tuoen` V1 只发 Windows 二进制
    /// （`docs/DESIGN.md` 决策 5）。这里返回"不支持"而不是假装路径不存在，
    /// 因为"文件不存在"会被上层误读成幽灵条目 —— 一个假答案比一个明确的失败更危险。
    pub fn find_first(_path: &Path) -> Result<FindFacts, Win32Code> {
        Err(ERROR_NOT_SUPPORTED)
    }

    pub fn reg_subkeys(_root: RootKey, _subkey: &str) -> Result<Vec<String>, Win32Code> {
        Err(ERROR_NOT_SUPPORTED)
    }

    pub fn reg_values(_root: RootKey, _subkey: &str) -> Result<Vec<RawRegValue>, Win32Code> {
        Err(ERROR_NOT_SUPPORTED)
    }
}

pub use imp::{find_first, reg_subkeys, reg_values};

#[cfg(test)]
mod tests {
    use super::*;

    /// 这三个 tag 是本票最贵的一条知识的落点：`alias-ghost` 的判定完全依赖它们。
    /// 故意不用 `windows-sys` 的常量（那两个 feature 没开），因此**必须钉住取值**。
    #[test]
    fn reparse_tags_match_the_win32_headers() {
        assert_eq!(crate::fs_facts::IO_REPARSE_TAG_APPEXECLINK, 0x8000_001b);
        assert_eq!(crate::fs_facts::IO_REPARSE_TAG_MOUNT_POINT, 0xa000_0003);
        assert_eq!(crate::fs_facts::IO_REPARSE_TAG_SYMLINK, 0xa000_000c);
    }

    #[test]
    fn app_exec_alias_tag_is_the_documented_one() {
        // 0x8000001b 的十进制是 2147483675 —— 与 windows-sys 0.61.2 的
        // `Win32::System::SystemServices::IO_REPARSE_TAG_APPEXECLINK` 一致。
        assert_eq!(crate::fs_facts::IO_REPARSE_TAG_APPEXECLINK, 2_147_483_675);
        // 它是"微软自定义"位（0x80000000）而非"名称代理"位（0x20000000）——
        // 这正是为什么它既不是文件也不是符号链接。
        assert_eq!(crate::fs_facts::IO_REPARSE_TAG_APPEXECLINK & 0x2000_0000, 0);
        assert_ne!(
            crate::fs_facts::IO_REPARSE_TAG_APPEXECLINK,
            crate::fs_facts::IO_REPARSE_TAG_MOUNT_POINT
        );
    }

    #[cfg(windows)]
    #[test]
    fn our_error_codes_agree_with_windows_sys() {
        // 我们故意自己定义这些码（非 Windows 分支也要用），所以必须证明没抄错。
        use windows_sys::Win32::Foundation as f;
        assert_eq!(ERROR_FILE_NOT_FOUND, f::ERROR_FILE_NOT_FOUND);
        assert_eq!(ERROR_PATH_NOT_FOUND, f::ERROR_PATH_NOT_FOUND);
        assert_eq!(ERROR_ACCESS_DENIED, f::ERROR_ACCESS_DENIED);
        assert_eq!(ERROR_NOT_SUPPORTED, f::ERROR_NOT_SUPPORTED);
        assert_eq!(ERROR_INVALID_NAME, f::ERROR_INVALID_NAME);
        assert_eq!(ERROR_MORE_DATA, f::ERROR_MORE_DATA);
        assert_eq!(ERROR_NO_MORE_ITEMS, f::ERROR_NO_MORE_ITEMS);
    }

    // **注意本模块为什么不测真实注册表**：ticket #11 的硬约束是"测试不得读取真实的
    // `HKCU\Environment` / `HKLM` 注册表 / 真实 `PATH` / 用户真实安装目录"。
    // 所以真实 `Reg*` 原语**没有**自动化测试 —— 它由真机验收运行覆盖
    // （`cargo run -p tuoen-cli -- detect --json`，见 ticket 的验收一节）。
    // 这里只测**纯函数**与**测试自己造的固定装置**（临时目录），不读任何机器状态。

    /// 只测路径准备这一段纯逻辑，不碰文件系统 —— 所以它不需要任何真实路径。
    #[cfg(windows)]
    #[test]
    fn find_pattern_handles_the_path_shapes_that_appear_on_real_path_values() {
        use super::imp::find_pattern;

        // 本机 PATH 上真的存在带尾部分隔符的条目，它们必须仍能被查。
        assert_eq!(
            String::from_utf16(&find_pattern(Path::new(r"C:\Dev\base\JDK\JDK8\bin\")).unwrap())
                .unwrap(),
            r"C:\Dev\base\JDK\JDK8\bin"
        );
        // 裸盘符削成 `C:` 会变成"C 盘当前目录"，语义就变了 —— 必须保住反斜杠。
        assert_eq!(
            String::from_utf16(&find_pattern(Path::new(r"C:\")).unwrap()).unwrap(),
            r"C:\"
        );
        assert_eq!(
            String::from_utf16(&find_pattern(Path::new("C:/")).unwrap()).unwrap(),
            r"C:\"
        );
        // 通配符必须被拒绝：若放过去，`C:\*` 会被 `FindFirstFileW` 匹配成"存在"。
        assert_eq!(
            find_pattern(Path::new(r"C:\*")).unwrap_err(),
            ERROR_INVALID_NAME
        );
        assert_eq!(
            find_pattern(Path::new(r"C:\a?b")).unwrap_err(),
            ERROR_INVALID_NAME
        );
    }

    /// 真实 `FindFirstFileW`，但只查**测试自己造的临时目录**。
    #[cfg(windows)]
    #[test]
    fn find_first_reports_size_and_missing_paths_from_the_tests_own_temp_dir() {
        let dir = crate::test_support::TempDir::new("sys-find");
        let file = dir.write("real.exe", b"xx");
        let facts = find_first(&file).expect("刚写出来的文件必须查得到");
        assert!(!facts.is_dir);
        assert_eq!(facts.size, 2);
        assert_eq!(facts.reparse_tag, None, "普通文件没有 reparse tag");

        let empty = dir.write("empty.exe", b"");
        assert_eq!(find_first(&empty).expect("0 字节文件存在").size, 0);

        assert!(find_first(dir.path()).expect("目录可查").is_dir);

        let missing = dir.path().join("nope.exe");
        let code = find_first(&missing).unwrap_err();
        assert!(
            code == ERROR_FILE_NOT_FOUND || code == ERROR_PATH_NOT_FOUND,
            "期望 2 或 3，实际 {code}"
        );
    }

    /// **这一条覆盖 reparse tag 的真实路径**：junction 是本票唯一能在非提权下真造出来的
    /// reparse point（ADR-0001：未提权 + 未开 Developer Mode 时 symlink 创建失败，
    /// 而 junction 成功）。App Execution Alias 造不出来（它由系统别名库生成），
    /// 所以那一层只在 fixture 与真机验收里覆盖。
    #[cfg(windows)]
    #[test]
    fn find_first_reads_a_real_junction_tag() {
        let dir = crate::test_support::TempDir::new("sys-junction");
        let target = dir.mkdir("target");
        let link = dir.path().join("link");
        if !crate::test_support::make_junction(&link, &target) {
            // 造不出 junction 就不假装测过 —— 但这台机器上它必须能成功。
            panic!(
                "未提权创建 junction 必须成功（ADR-0001 的本机实测）：{}",
                link.display()
            );
        }
        let facts = find_first(&link).expect("junction 可查");
        assert!(facts.is_dir, "junction 的目录位必须置位");
        assert_eq!(
            facts.reparse_tag,
            Some(crate::fs_facts::IO_REPARSE_TAG_MOUNT_POINT),
            "junction 的 tag 必须是 IO_REPARSE_TAG_MOUNT_POINT(0xA0000003)"
        );
    }
}
