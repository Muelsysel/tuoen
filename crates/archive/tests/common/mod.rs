//! 测试用的归档构造器。
//!
//! ## 为什么自己写 ZIP / TAR 的字节，而不用一个现成的库
//!
//! 票据 #5 要测的是"**恶意**归档"。用现成的库去造恶意归档有一个根本问题：
//! 那些库**自己就会拦掉**一部分恶意名字（或者静默净化它们），于是测试
//! 测的是"库的净化行为"，而不是"我们的校验"。
//!
//! 手写字节还有第二个好处：**能造出库造不出来的东西** ——
//! 名字里含换行、结尾带点、`CON`、只差大小写的两个条目。
//!
//! 这里只实现 **stored（不压缩）** 与 **ustar** 两种最简形态：
//! 我们要测的是**名字与条目类型**，不是压缩算法（压缩由 libarchive 负责，
//! 而它认不认这些格式由真机验收覆盖）。

use std::path::{Path, PathBuf};

/// 一个 ZIP 条目。
pub struct ZipEntry {
    /// 归档里的名字（**原样**，不做任何净化）。
    pub name: String,
    /// 内容。
    pub data: Vec<u8>,
    /// Unix 模式的高位（`0o120000` = symlink，`0o100644` = 普通文件）。
    pub unix_mode: u32,
}

impl ZipEntry {
    /// 一个普通文件条目。
    pub fn file(name: &str, data: &[u8]) -> Self {
        Self {
            name: name.to_owned(),
            data: data.to_vec(),
            unix_mode: 0o100_644,
        }
    }

    /// 一个目录条目。
    pub fn dir(name: &str) -> Self {
        Self {
            name: format!("{}/", name.trim_end_matches('/')),
            data: Vec::new(),
            unix_mode: 0o040_755,
        }
    }

    /// 一个 **symlink 条目**：内容是指向目标的文本，模式位标成 `S_IFLNK`。
    ///
    /// 实测：bsdtar 在 Windows 上会把它落成**普通文件**（内容是那串目标路径）
    /// 而不是链接 —— 安全，但结果与归档声明不符且**无提示**。
    /// 所以我们选择**整个拒绝**。
    pub fn symlink(name: &str, target: &str) -> Self {
        Self {
            name: name.to_owned(),
            data: target.as_bytes().to_vec(),
            unix_mode: 0o120_777,
        }
    }
}

/// 把一批条目写成一个 ZIP 文件（stored，不压缩）。
pub fn write_zip(path: &Path, entries: &[ZipEntry]) {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();

    for entry in entries {
        let offset = out.len() as u32;
        let crc = crc32(&entry.data);
        let name = entry.name.as_bytes();
        let size = entry.data.len() as u32;

        // ---- local file header ----
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // signature
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method = stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x21u16.to_le_bytes()); // mod date (1980-01-01)
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // compressed
        out.extend_from_slice(&size.to_le_bytes()); // uncompressed
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name);
        out.extend_from_slice(&entry.data);

        // ---- central directory header ----
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&0x031eu16.to_le_bytes()); // made by Unix, 3.0
        central.extend_from_slice(&20u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // method
        central.extend_from_slice(&0u16.to_le_bytes()); // mod time
        central.extend_from_slice(&0x21u16.to_le_bytes()); // mod date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk start
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        // 外部属性：高 16 位是 Unix 模式（symlink 就靠这里）。
        central.extend_from_slice(&(entry.unix_mode << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }

    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);

    // ---- end of central directory ----
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // disk with CD
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment len

    std::fs::write(path, out).expect("写 zip");
}

/// 一个 TAR 条目。
pub struct TarEntry {
    /// 归档里的名字（**原样**）。
    pub name: String,
    /// 内容（symlink / hardlink 时忽略）。
    pub data: Vec<u8>,
    /// ustar 的 typeflag：`0` 文件、`5` 目录、`2` symlink、`1` 硬链接、`3` 字符设备、`4` 块设备、`6` fifo。
    pub typeflag: u8,
    /// 链接目标（symlink / hardlink）。
    pub linkname: String,
}

impl TarEntry {
    /// 普通文件。
    pub fn file(name: &str, data: &[u8]) -> Self {
        Self {
            name: name.to_owned(),
            data: data.to_vec(),
            typeflag: b'0',
            linkname: String::new(),
        }
    }

    /// 目录。
    pub fn dir(name: &str) -> Self {
        Self {
            name: format!("{}/", name.trim_end_matches('/')),
            data: Vec::new(),
            typeflag: b'5',
            linkname: String::new(),
        }
    }

    /// symlink（`typeflag = '2'`）。
    pub fn symlink(name: &str, target: &str) -> Self {
        Self {
            name: name.to_owned(),
            data: Vec::new(),
            typeflag: b'2',
            linkname: target.to_owned(),
        }
    }

    /// 硬链接（`typeflag = '1'`）。
    pub fn hard_link(name: &str, target: &str) -> Self {
        Self {
            name: name.to_owned(),
            data: Vec::new(),
            typeflag: b'1',
            linkname: target.to_owned(),
        }
    }

    /// fifo（`typeflag = '6'`）。
    pub fn fifo(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            data: Vec::new(),
            typeflag: b'6',
            linkname: String::new(),
        }
    }
}

/// 把一批条目写成一个 ustar 归档。
pub fn write_tar(path: &Path, entries: &[TarEntry]) {
    let mut out: Vec<u8> = Vec::new();
    for entry in entries {
        let mut header = [0u8; 512];

        // name[100]
        write_field(&mut header[0..100], entry.name.as_bytes());
        // mode[8]
        write_octal(&mut header[100..108], 0o644);
        // uid[8] / gid[8]
        write_octal(&mut header[108..116], 0);
        write_octal(&mut header[116..124], 0);
        // size[12] —— 目录与链接的 size 是 0。
        let size = if entry.typeflag == b'0' {
            entry.data.len() as u64
        } else {
            0
        };
        write_octal(&mut header[124..136], size);
        // mtime[12]
        write_octal(&mut header[136..148], 0);
        // chksum[8] 先填空格，最后再算
        header[148..156].fill(b' ');
        // typeflag[1]
        header[156] = entry.typeflag;
        // linkname[100]
        write_field(&mut header[157..257], entry.linkname.as_bytes());
        // magic[6] + version[2]
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        // uname[32] / gname[32]
        write_field(&mut header[265..297], b"tuoen");
        write_field(&mut header[297..329], b"tuoen");
        // devmajor[8] / devminor[8]
        write_octal(&mut header[329..337], 0);
        write_octal(&mut header[337..345], 0);

        // 校验和 = 所有字节之和（chksum 字段当成 8 个空格）。
        let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
        // 格式：6 位八进制 + NUL + 空格。
        let text = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(text.as_bytes());

        out.extend_from_slice(&header);
        if entry.typeflag == b'0' {
            out.extend_from_slice(&entry.data);
            // 补齐到 512 的整数倍
            let pad = (512 - (entry.data.len() % 512)) % 512;
            out.extend(std::iter::repeat_n(0u8, pad));
        }
    }
    // 两个全零块收尾
    out.extend(std::iter::repeat_n(0u8, 1024));
    std::fs::write(path, out).expect("写 tar");
}

fn write_field(target: &mut [u8], value: &[u8]) {
    let n = value.len().min(target.len());
    target[..n].copy_from_slice(&value[..n]);
}

fn write_octal(target: &mut [u8], value: u64) {
    // 格式：宽度-1 位八进制 + NUL
    let text = format!("{:0width$o}", value, width = target.len() - 1);
    let n = text.len().min(target.len() - 1);
    target[..n].copy_from_slice(&text.as_bytes()[..n]);
    target[target.len() - 1] = 0;
}

/// 标准 CRC-32（IEEE 802.3，ZIP 用的那个）。
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// 造一个测试专用的临时工作区：一个归档 + 一个版本目录。
pub struct Workspace {
    dir: tuoen_platform::test_support::TempDir,
}

impl Workspace {
    /// 新建。
    pub fn new(label: &str) -> Self {
        Self {
            dir: tuoen_platform::test_support::TempDir::new(&format!("archive-{label}")),
        }
    }

    /// 根路径。
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// 版本目录的父目录（`<root>/versions`）。
    pub fn versions(&self) -> PathBuf {
        self.root().join("versions")
    }

    /// 一个归档文件的路径（**不创建它**）。
    pub fn archive_path(&self, name: &str) -> PathBuf {
        self.root().join(name)
    }

    /// 写一个 ZIP，返回它的路径。
    pub fn zip(&self, name: &str, entries: &[ZipEntry]) -> PathBuf {
        let path = self.archive_path(name);
        write_zip(&path, entries);
        path
    }

    /// 写一个 TAR，返回它的路径。
    pub fn tar(&self, name: &str, entries: &[TarEntry]) -> PathBuf {
        let path = self.archive_path(name);
        write_tar(&path, entries);
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_vector() {
        // ZIP 用的 CRC-32（IEEE）对 "123456789" 的标准值是 0xCBF43926。
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn the_writers_produce_files_that_our_own_listing_can_read() {
        // **这条测试测的是测试工具本身**：如果归档构造器写出来的字节
        // 不是合法 ZIP/TAR，那么所有依赖它的测试都会以"解压失败"告终，
        // 而那是**假失败** —— 我们会以为是校验逻辑错了。
        use tuoen_archive::TarCli;
        use tuoen_platform::SystemProcessRunner;

        let workspace = Workspace::new("selfcheck");
        let zip = workspace.zip("ok.zip", &[ZipEntry::file("a.txt", b"hello")]);
        let tar = workspace.tar("ok.tar", &[TarEntry::file("a.txt", b"hello")]);

        let cli = TarCli::probe().expect("tar.exe");
        let runner = SystemProcessRunner;
        for archive in [zip, tar] {
            let listed = cli
                .list_names(&runner, &archive)
                .unwrap_or_else(|e| panic!("{:?} 应当是个合法归档：{e}", archive));
            assert_eq!(listed.len(), 1, "{archive:?}");
            assert_eq!(listed[0].raw_name, "a.txt", "{archive:?}");
        }
    }
}
