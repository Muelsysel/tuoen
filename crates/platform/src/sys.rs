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
/// `ERROR_ALREADY_EXISTS` —— 也被 [`crate::junction`] 用来表示
/// "那个位置已经有东西了，而我不覆盖它"。
pub const ERROR_ALREADY_EXISTS: Win32Code = 183;
/// `ERROR_MORE_DATA`
pub const ERROR_MORE_DATA: Win32Code = 234;
/// `ERROR_NO_MORE_ITEMS`
pub const ERROR_NO_MORE_ITEMS: Win32Code = 259;
/// `ERROR_REPARSE_TAG_MISMATCH` —— 想就地替换重解析点数据，但标签对不上。
///
/// **这一条是"能不能原子翻转 junction"的判据**：同标签时
/// `FSCTL_SET_REPARSE_POINT` 是"替换数据"，不同标签才报这个错。
pub const ERROR_REPARSE_TAG_MISMATCH: Win32Code = 4390;
/// `IO_REPARSE_TAG_MOUNT_POINT` —— junction 的标签。
pub const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// 一个路径在"链接"这件事上的形状。
///
/// **把"是 junction"与"是 symlink"分开，是因为它们在 Windows 上
/// 不可互换**（`GLOSSARY.md` 专门写了一条）：symlink 需要
/// `SeCreateSymbolicLinkPrivilege`，junction 不需要；而两者的重解析
/// **标签不同**，所以"就地替换"只对同标签成立。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JunctionState {
    /// 路径不存在。
    Missing,
    /// 是一个普通文件。
    File,
    /// 是一个**真实的**目录（不是任何链接）。
    RealDirectory,
    /// 是 junction。
    Junction,
    /// 是 symlink（文件或目录）。
    Symlink,
    /// 是别的重解析点（App Execution Alias 等）。
    OtherReparse {
        /// 原始 tag。
        tag: u32,
    },
}

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
#[allow(
    unsafe_code,
    reason = "全仓库唯一允许 unsafe 的地方：Win32 FFI 的调用点。声明本身由 windows-sys 提供，\
              这里的每个 unsafe 块都带 SAFETY 注释说明指针来源与生命周期。业务逻辑（tuoen-core）\
              永远不会看到 unsafe。"
)]
mod imp {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;

    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetLastError, INVALID_HANDLE_VALUE, WIN32_ERROR,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FindClose, FindFirstFileW,
        WIN32_FIND_DATAW,
    };
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_READ, KEY_SET_VALUE,
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW,
        RegSetValueExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    use super::{ERROR_INVALID_NAME, FindFacts, RawRegValue, RootKey, Win32Code};

    /// UTF-16 + NUL，供 `*W` 系列调用。
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(iter::once(0)).collect()
    }

    /// 一个**路径**的以 NUL 结尾的 UTF-16 缓冲区。
    ///
    /// 用 `OsStrExt::encode_wide` 而不是 `to_string_lossy`：路径可能含
    /// 无法用 UTF-8 表示的字符（Windows 上少见但合法），lossy 会让它变成
    /// 一个**不同的**路径 —— 那种 bug 只在别人的机器上出现。
    pub fn path_wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(iter::once(0))
            .collect()
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
    ///
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

    // ─────────────── junction（重解析点） ───────────────

    /// `IO_REPARSE_TAG_SYMLINK`。
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    /// `FSCTL_SET_REPARSE_POINT`
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    /// `FSCTL_DELETE_REPARSE_POINT`
    const FSCTL_DELETE_REPARSE_POINT: u32 = 0x0009_00AC;
    /// `GENERIC_READ | GENERIC_WRITE` —— 设置重解析点要写权限。
    const GENERIC_READ_WRITE: u32 = 0x8000_0000 | 0x4000_0000;

    /// 一个打开的、**指向重解析点本身**（不跟随）的句柄。
    ///
    /// 三个 flag 都不能少：
    ///
    /// * `FILE_FLAG_OPEN_REPARSE_POINT`：打开链接**本身**而不是它的目标。
    ///   少了它就变成了对目标操作 —— 那会把目标改掉；
    /// * `FILE_FLAG_BACKUP_SEMANTICS`：打开**目录**句柄必需
    ///   （没有它 `CreateFileW` 对目录报 `ERROR_ACCESS_DENIED`）；
    /// * `FILE_SHARE_DELETE`：不共享删除的话，别的进程（资源管理器、
    ///   索引器）打开着它就打不开。这是"偶发失败"的常见来源。
    struct ReparseHandle(windows_sys::Win32::Foundation::HANDLE);

    impl ReparseHandle {
        fn open(path: &Path, access: u32) -> Result<Self, super::Win32Code> {
            let wide = path_wide(path);
            // SAFETY: `wide` 是一个以 NUL 结尾的 UTF-16 缓冲区，在本函数内一直存活。
            // 返回的句柄由本结构体持有，`Drop` 里关掉。
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    ptr::null_mut(),
                )
            };
            if std::ptr::eq(handle, INVALID_HANDLE_VALUE) {
                // SAFETY: 无参数，只读线程局部的 last-error。
                return Err(unsafe { GetLastError() });
            }
            Ok(Self(handle))
        }
    }

    impl Drop for ReparseHandle {
        fn drop(&mut self) {
            // SAFETY: 句柄来自 `CreateFileW`，且没有被别处关闭。
            unsafe { CloseHandle(self.0) };
        }
    }

    // ───────────────────────── 写 ─────────────────────────
    //
    // **本文件在此之前是纯只读的，这一段是唯一的例外。** 它只服务于一件事：
    // 把用户级 `PATH` 整条写回 `HKCU\Environment`。理由写在 `src/path.rs` 的模块文档里
    // （`setx` 会在 1024 处静默裁剪并永久展开 `%VAR%`）。
    //
    // 三个刻意的限制：
    // 1. **没有任何"追加"或"删除某一项"的函数** —— 只有"整条值一次写完"。
    //    逐段改会产生"`PATH` 暂时少了几个目录"的中间状态，而那个状态是活的。
    // 2. `RegCreateKeyExW` 只在键不存在时创建（`HKCU\Environment` 正常总是存在，
    //    但"不存在"不该变成一次失败的写）。
    // 3. 返回 `Win32Code`，不在这里翻译成业务错误 —— 与读函数一致。

    /// 打开（或创建）一个可写的键。
    fn open_writable(root: RootKey, subkey: &str) -> Result<OpenKey, Win32Code> {
        let subkey = wide(subkey);
        let mut hkey: HKEY = ptr::null_mut();
        let mut disposition: u32 = 0;
        // SAFETY: `subkey` 以 NUL 结尾；出参都在本栈上；安全属性与 `samDesired` 之外的
        // 参数传 0/NULL 是文档允许的（`REG_OPTION_NON_VOLATILE` = 0）。
        let rc: WIN32_ERROR = unsafe {
            RegCreateKeyExW(
                root_handle(root),
                subkey.as_ptr(),
                0,
                ptr::null_mut(),
                0,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                ptr::null(),
                &mut hkey,
                &mut disposition,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(rc);
        }
        Ok(OpenKey(hkey))
    }

    /// 写一个字符串值（`REG_SZ` 或 `REG_EXPAND_SZ`）。
    ///
    /// `kind` 只接受这两个：调用方要传的是 [`crate::RegType`] 的落点，
    /// 而多传几个类型码只会让"我们到底写了什么类型"更难回答。
    pub fn reg_set_string(
        root: RootKey,
        subkey: &str,
        name: &str,
        kind: u32,
        text: &str,
    ) -> Result<(), Win32Code> {
        let key = open_writable(root, subkey)?;
        let name = wide(name);
        // `REG_SZ` 的数据**包含结尾的 NUL**（本机读回来的字节里就有它）。
        let mut data: Vec<u16> = text.encode_utf16().collect();
        data.push(0);
        let bytes = std::mem::size_of_val(data.as_slice());
        // SAFETY: 句柄有效；`data` 是可读缓冲，`bytes` 是它的字节长度；`name` 以 NUL 结尾。
        let rc: WIN32_ERROR = unsafe {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                kind,
                data.as_ptr().cast::<u8>(),
                u32::try_from(bytes).unwrap_or(u32::MAX),
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(rc);
        }
        Ok(())
    }

    /// 删掉一个值。值不存在时也返回成功 —— 那是幂等，不是失败。
    pub fn reg_delete_value(root: RootKey, subkey: &str, name: &str) -> Result<(), Win32Code> {
        let key = open_writable(root, subkey)?;
        let name = wide(name);
        // SAFETY: 句柄有效；`name` 以 NUL 结尾。
        let rc: WIN32_ERROR = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
        if rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        Err(rc)
    }

    /// 广播 `WM_SETTINGCHANGE`，让 Explorer 重读环境。
    ///
    /// 返回值是**收到应答的顶层窗口数**。它的含义必须说准：
    ///
    /// - `0` 不代表失败。没有顶层窗口响应是正常的（本机实测有时就是 0），
    ///   而广播本身仍然发生了。
    /// - 它**不是**"所有进程都更新了"的证据。环境块是 `CreateProcess` 时复制的，
    ///   所以已经在跑的 cmd / PowerShell / IDE / 服务**永远拿不到**这次变更。
    ///   "请重启终端"是平台限制，不是我们偷懒。
    pub fn broadcast_environment_change() -> usize {
        let mut result: usize = 0;
        let param = wide("Environment");
        // SAFETY: `param` 以 NUL 结尾；`&mut result` 是有效出参；
        // `HWND_BROADCAST` 是文档规定的"发给所有顶层窗口"。
        let replied = unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                param.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            )
        };
        // 返回 0 表示"一个应答都没收到"或"超时"，两种情况 `replied` 都是 0。
        if replied == 0 { 0 } else { result }
    }

    /// 当前进程**有没有管理员权限**。
    ///
    /// # 为什么用 `TokenElevation` 而不是"用户是不是管理员"
    ///
    /// UAC 下，一个未提权的进程里管理员组仍然在令牌里，只是被标成
    /// `SE_GROUP_USE_FOR_DENY_ONLY` —— 所以"这个用户是管理员"与"这个进程现在能不能
    /// 写 `HKLM` / 建 symlink"是两个问题，而只有后者决定我们能不能做那些事。
    /// `TokenElevation` 问的正是后者。
    ///
    /// 读不到令牌时返回 `None`（**不是 `false`**）："不知道"与"没提权"是两件事，
    /// 而 `system.elevated` 会把它们印成不同的字。
    pub fn is_elevated() -> Option<bool> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Security::{
            GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        let mut token: windows_sys::Win32::Foundation::HANDLE = ptr::null_mut();
        // SAFETY: `GetCurrentProcess` 返回一个伪句柄（不需要关闭）；
        // `&mut token` 是有效出参；`TOKEN_QUERY` 是文档要求的最小权限。
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        if opened == 0 {
            return None;
        }

        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned: u32 = 0;
        // SAFETY: `token` 是刚打开的有效句柄；出参缓冲的大小按 `size_of` 给出，
        // 正是 `GetTokenInformation` 对 `TokenElevation` 的要求。
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                ptr::addr_of_mut!(elevation).cast(),
                u32::try_from(std::mem::size_of::<TOKEN_ELEVATION>()).unwrap_or(4),
                &mut returned,
            )
        };
        // SAFETY: 句柄由 `OpenProcessToken` 打开，这里关闭它一次。
        unsafe { CloseHandle(token) };

        if ok == 0 {
            return None;
        }
        Some(elevation.TokenIsElevated != 0)
    }

    /// 一个路径在"链接"这件事上的形状。
    pub fn junction_state(path: &Path) -> super::JunctionState {
        match super::find_first(path) {
            Err(_) => super::JunctionState::Missing,
            Ok(facts) => match (facts.reparse_tag, facts.is_dir) {
                (Some(super::IO_REPARSE_TAG_MOUNT_POINT), _) => super::JunctionState::Junction,
                (Some(IO_REPARSE_TAG_SYMLINK), _) => super::JunctionState::Symlink,
                (Some(tag), _) => super::JunctionState::OtherReparse { tag },
                (None, true) => super::JunctionState::RealDirectory,
                (None, false) => super::JunctionState::File,
            },
        }
    }

    /// 把一个目标路径规范化成重解析点里能用的形式。
    ///
    /// **`PathBuf` 在 Windows 上不规范化分隔符。** `Path::new(r"C:\a").join("b/c")`
    /// 得到的是 `C:\a\b/c` —— 一个混着两种分隔符的路径，而 Rust 的文件 API
    /// 全都接受它，所以**没有任何东西会告诉你它有问题**。
    ///
    /// 但重解析点里不行：把 `C:\a\b/c` 写进替代名之后，内核按字面量处理它，
    /// 而 `\??\C:\a\b/c` 会走到 `STATUS_OBJECT_NAME_INVALID` ——
    /// 用户看到的是 `ERROR_INVALID_NAME`(123)，"文件名、目录名或卷标语法不正确"。
    ///
    /// **症状特别难查**：`junction_state` 说它是 junction、`read_link` 也能
    /// 读出目标、`fsutil` 的字段全都自洽，只是**任何一次访问都失败**。
    /// 我在这上面绕了两轮，最后是靠把 `mklink /J` 造的真品与自己的产物
    /// 逐字节对比才定位到 —— 区别只在目标路径里那一个 `/`。
    fn normalize_for_reparse(target: &Path) -> String {
        target.to_string_lossy().replace('/', r"\")
    }

    /// 拼一个 junction 的重解析数据块。
    ///
    /// 布局（`REPARSE_DATA_BUFFER` + `MountPointReparseBuffer`）：
    ///
    /// ```text
    /// 偏移  大小  字段
    ///   0    4   ReparseTag           = IO_REPARSE_TAG_MOUNT_POINT
    ///   4    2   ReparseDataLength    = 8 + PathBuffer 的字节数
    ///   6    2   Reserved             = 0
    ///   8    2   SubstituteNameOffset = 0
    ///  10    2   SubstituteNameLength = 替代名的字节数 **含结尾的 NUL**
    ///  12    2   PrintNameOffset      = SubstituteNameLength + 2
    ///  14    2   PrintNameLength      = 打印名的字节数 **含结尾的 NUL**
    ///  16   ..   PathBuffer           = 替代名 NUL ・ 2 字节间隙 ・ 打印名 NUL ・ 2 字节尾部
    /// ```
    ///
    /// # 这张表是**实测**来的，不是从文档抄的
    ///
    /// 我先按"长度不含 NUL"写了一遍，结果是 `junction_state` 认为它是
    /// junction、`read_link` 也能读出目标，但**任何一次访问都报
    /// `ERROR_INVALID_NAME`(123)** —— 一个"看起来对但打不开"的链接。
    ///
    /// 拿 `fsutil reparsepoint query` 读 `mklink /J` 造出来的真品，
    /// 才看清三件文档没写清楚的事：
    ///
    /// ```text
    /// Substitue Name length: 136     ← 67 个字符 + NUL = 68 个 UTF-16 单元
    /// Print Name offset:     138     ← 136 + 2，**中间有 2 字节**
    /// Print Name Length:     128     ← 63 个字符 + NUL = 64 个 UTF-16 单元
    /// Reparse Data Length:   0x114   ← 276 = 8 + 268
    /// ```
    ///
    /// ① **两个长度都含结尾的 NUL**；② 替代名与打印名之间**有 2 字节间隙**
    /// （`PrintNameOffset` 不是 `SubstituteNameLength`）；③ `PathBuffer`
    /// 的长度比"名字 + NUL"多 2 字节。
    ///
    /// 三条都照做之后才通。**这一条值得记下来**：一个"元数据对但打不开"
    /// 的 junction 是最难查的一类 bug —— 每个单独的检查都说它是对的。
    ///
    /// # 两个名字的区别
    ///
    /// **替代名必须带 `\??\` 前缀**：它走的是进程的 DOS 设备命名空间，
    /// 少了这个前缀，内核会把目标当成一个字面量相对路径。
    /// **打印名不能带**：它是给人与资源管理器看的。
    fn mount_point_data(target: &Path) -> Vec<u8> {
        let plain = normalize_for_reparse(target);
        let substitute = format!(r"\??\{plain}");

        let substitute: Vec<u16> = substitute.encode_utf16().collect();
        let print: Vec<u16> = plain.encode_utf16().collect();

        // 长度**不含**各自结尾的 NUL —— 这是 `fsutil` 给出的真值
        // （69 个字符的路径，替代名 73 个字符 → 146 字节）。
        let substitute_bytes = substitute.len() * 2;
        let print_bytes = print.len() * 2;
        let print_offset = substitute_bytes + 2;

        let mut path_buffer: Vec<u16> = Vec::with_capacity(substitute.len() + print.len() + 2);
        path_buffer.extend_from_slice(&substitute);
        path_buffer.push(0); // 替代名的 NUL
        path_buffer.extend_from_slice(&print);
        path_buffer.push(0); // 打印名的 NUL

        let data_length = 8 + path_buffer.len() * 2;

        let mut out = Vec::with_capacity(8 + data_length);
        out.extend_from_slice(&super::IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        out.extend_from_slice(&(data_length as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        out.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
        out.extend_from_slice(&(substitute_bytes as u16).to_le_bytes());
        out.extend_from_slice(&(print_offset as u16).to_le_bytes());
        out.extend_from_slice(&(print_bytes as u16).to_le_bytes());
        for unit in path_buffer {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    }

    /// 创建一个 junction。
    ///
    /// **先建一个空目录，再把重解析点装在它上面** —— 这是
    /// `FSCTL_SET_REPARSE_POINT` 的用法要求：它作用于一个已存在的文件或目录。
    pub fn create_junction(link: &Path, target: &Path) -> Result<(), super::Win32Code> {
        std::fs::create_dir(link).map_err(|source| {
            u32::try_from(source.raw_os_error().unwrap_or(0)).unwrap_or(super::ERROR_ACCESS_DENIED)
        })?;
        match set_junction_data(link, target) {
            Ok(()) => Ok(()),
            Err(error) => {
                // **失败时把空目录收回去**，不留下一个"看起来存在但打不开"的东西。
                let _ = std::fs::remove_dir(link);
                Err(error)
            }
        }
    }

    /// 把一个 junction 的数据**就地**替换成指向 `target`。
    ///
    /// 这是原子翻转的全部秘密：同标签时 `FSCTL_SET_REPARSE_POINT` 是
    /// **替换数据**，不是"先删再建"。所以 `current` 这个路径在任何
    /// 时刻都是可解析的，只是"某一瞬间之后指向的是新版本"。
    ///
    /// 标签不同（比如试图把 symlink 就地改成 junction）会报
    /// [`super::ERROR_REPARSE_TAG_MISMATCH`] —— 那是**好事**：
    /// 它说明内核不会让我们悄悄换掉另一种重解析点。
    pub fn set_junction_data(link: &Path, target: &Path) -> Result<(), super::Win32Code> {
        let handle = ReparseHandle::open(link, GENERIC_READ_WRITE)?;
        let data = mount_point_data(target);
        let mut returned: u32 = 0;
        // SAFETY: `handle` 是刚打开的有效句柄；`data` 的布局按上面的
        // `REPARSE_DATA_BUFFER` 手工拼好，长度由 `data.len()` 给出；
        // `returned` 是一个栈上的 u32。
        let ok = unsafe {
            DeviceIoControl(
                handle.0,
                FSCTL_SET_REPARSE_POINT,
                data.as_ptr().cast(),
                data.len() as u32,
                ptr::null_mut(),
                0,
                &mut returned,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            // SAFETY: 无参数。
            return Err(unsafe { GetLastError() });
        }
        Ok(())
    }

    /// 删掉重解析点，并把那个目录本身删掉。
    ///
    /// **只删链接，永远不删目标。** 这是整个工具里最容易写错、
    /// 后果最严重的一处：junction 的目录句柄上执行递归删除会**跟着
    /// 跳进目标**（`std::fs::remove_dir_all` 在某些路径下会这样做）。
    /// 所以这里两步都是"针对链接本身"的：先摘掉重解析点
    /// （`FILE_FLAG_OPEN_REPARSE_POINT` 保证不跟过去），
    /// 再删空的目录项。
    pub fn remove_reparse_point(link: &Path) -> Result<(), super::Win32Code> {
        // **`FSCTL_DELETE_REPARSE_POINT` 不接受空输入缓冲区。**
        // 传 NULL 会得到 `ERROR_INVALID_USER_BUFFER`(1784) —— 实测。
        //
        // 文档要求的输入是一个 `REPARSE_DATA_BUFFER`，其中
        // `ReparseTag` 指明要删**哪一种**重解析点，而
        // `ReparseDataLength` 为 0 表示"只删点，不改数据"。
        // 所以最小合法输入是 8 个字节：tag(4) + 0(2) + 0(2)。
        let facts = super::find_first(link)?;
        let Some(tag) = facts.reparse_tag else {
            // `ERROR_NOT_A_REPARSE_POINT` 与 `ERROR_REPARSE_TAG_MISMATCH`
            // 在 Win32 里是**同一个值**（4390），所以用后者这个名字。
            return Err(super::ERROR_REPARSE_TAG_MISMATCH);
        };
        let mut input = Vec::with_capacity(8);
        input.extend_from_slice(&tag.to_le_bytes());
        input.extend_from_slice(&0u16.to_le_bytes()); // ReparseDataLength
        input.extend_from_slice(&0u16.to_le_bytes()); // Reserved

        {
            let handle = ReparseHandle::open(link, GENERIC_READ_WRITE)?;
            let mut returned: u32 = 0;
            // SAFETY: `handle` 是刚打开的有效句柄；`input` 是上面拼好的
            // 8 字节缓冲区，长度由 `input.len()` 给出；`returned` 在栈上。
            let ok = unsafe {
                DeviceIoControl(
                    handle.0,
                    FSCTL_DELETE_REPARSE_POINT,
                    input.as_ptr().cast(),
                    input.len() as u32,
                    ptr::null_mut(),
                    0,
                    &mut returned,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                // SAFETY: 无参数。
                return Err(unsafe { GetLastError() });
            }
        } // 句柄在这里关掉 —— **必须在删目录之前**，否则 `remove_dir` 会因
        // 为句柄还开着而报"目录不是空的"。
        std::fs::remove_dir(link).map_err(|source| {
            u32::try_from(source.raw_os_error().unwrap_or(0)).unwrap_or(super::ERROR_ACCESS_DENIED)
        })
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    use super::{ERROR_NOT_SUPPORTED, FindFacts, JunctionState, RawRegValue, RootKey, Win32Code};

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

    /// 非 Windows 上没有注册表可写 —— 返回"缺能力"，而不是假装成功。
    pub fn reg_set_string(
        _root: RootKey,
        _subkey: &str,
        _name: &str,
        _kind: u32,
        _text: &str,
    ) -> Result<(), Win32Code> {
        Err(ERROR_NOT_SUPPORTED)
    }

    /// 同上。
    pub fn reg_delete_value(_root: RootKey, _subkey: &str, _name: &str) -> Result<(), Win32Code> {
        Err(ERROR_NOT_SUPPORTED)
    }

    /// 非 Windows 上没有 `WM_SETTINGCHANGE`。返回 0 个应答 ——
    /// 这在 Windows 上也是合法结果（见 Windows 版本的文档），所以调用方无需分支。
    pub fn broadcast_environment_change() -> usize {
        0
    }

    /// 非 Windows 上不假装知道 —— 返回"问不出来"，而不是 `false`。
    ///
    /// 这一条不是形式主义：`system.elevated` 会把 `None` 印成"不知道"，
    /// 而 `Some(false)` 印成"未提权"。把前者写成后者是在编一个答案。
    pub fn is_elevated() -> Option<bool> {
        None
    }

    /// 非 Windows 上没有 junction：返回"缺能力"，而不是假装成功。
    pub fn junction_state(_path: &Path) -> JunctionState {
        super::JunctionState::Missing
    }

    /// 非 Windows 上没有 junction。
    pub fn create_junction(_link: &Path, _target: &Path) -> Result<(), Win32Code> {
        Err(ERROR_NOT_SUPPORTED)
    }

    /// 非 Windows 上没有 junction。
    pub fn set_junction_data(_link: &Path, _target: &Path) -> Result<(), Win32Code> {
        Err(ERROR_NOT_SUPPORTED)
    }

    /// 非 Windows 上没有 junction。
    pub fn remove_reparse_point(_link: &Path) -> Result<(), Win32Code> {
        Err(ERROR_NOT_SUPPORTED)
    }
}

pub use imp::{
    broadcast_environment_change, create_junction, find_first, is_elevated, junction_state,
    reg_delete_value, reg_set_string, reg_subkeys, reg_values, remove_reparse_point,
    set_junction_data,
};

#[cfg(test)]
mod tests {
    use std::path::Path;

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

        /// 拿到给 `FindFirstFileW` 的 UTF-16 缓冲的文本部分，**并断言它以 NUL 结尾**
        /// （忘了结尾 NUL 会读到栈后面的垃圾，是这类代码最经典的静默 bug）。
        fn pattern_text(path: &str) -> String {
            let mut wide = find_pattern(Path::new(path)).expect("应当接受");
            assert_eq!(wide.pop(), Some(0), "必须以 NUL 结尾：{wide:?}");
            String::from_utf16(&wide).expect("合法 UTF-16")
        }

        // 本机 PATH 上真的存在带尾部分隔符的条目，它们必须仍能被查。
        assert_eq!(
            pattern_text(r"C:\Dev\base\JDK\JDK8\bin\"),
            r"C:\Dev\base\JDK\JDK8\bin"
        );
        // 裸盘符削成 `C:` 会变成"C 盘当前目录"，语义就变了 —— 必须保住反斜杠。
        assert_eq!(pattern_text(r"C:\"), r"C:\");
        assert_eq!(pattern_text("C:/"), r"C:\");
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
