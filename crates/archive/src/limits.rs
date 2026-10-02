//! 解压的上限。
//!
//! ## 为什么要有上限，以及为什么默认值是这些数
//!
//! 一个归档可以声明"我解压后有 4 PB"，然后在解压时把磁盘写满 ——
//! 这是压缩炸弹，不需要任何漏洞就能生效。上限不是防攻击的**充分**条件，
//! 但它是必要条件，而且**代价为零**。
//!
//! 默认值的依据是本机真实制品的量级（不是拍脑袋）：
//!
//! | 制品 | 归档大小 | 解压后 | 条目数 |
//! |---|---|---|---|
//! | `node-v24.19.0-win-x64.zip` | 35.6 MB | ~110 MB | ~2450（`.7z` 实测 2454） |
//! | Temurin JDK 21 | ~190 MB | ~330 MB | ~20000 |
//!
//! 所以默认值取"比最大真实制品再宽一个量级"，既能挡住炸弹，
//! 又不会在正经制品上误报。

/// 解压的上限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractLimits {
    /// 最多多少个条目。
    ///
    /// 默认 100,000：Temurin JDK 约 2 万个文件，宽五倍。
    pub max_entries: usize,
    /// 解压后的总体积上限（字节）。
    ///
    /// 默认 8 GiB：最大的真实制品（JDK）约 330 MB，宽 25 倍。
    /// 一个 190 MB 的归档解压出 8 GB 已经是 42 倍膨胀，那不正常。
    pub max_total_bytes: u64,
    /// 单个条目声明的大小上限（字节）。
    ///
    /// 默认 2 GiB。**这一条与 `max_total_bytes` 不是重复的**：
    /// 一个条目声明 4 PB 时，我们希望在**读它之前**就拒掉，
    /// 而不是解压到一半才发现总量超了 —— 那时磁盘已经写进去一半了。
    pub max_entry_bytes: u64,
    /// 目录层数上限。
    ///
    /// 默认 64。真实制品最深的是 JDK 的
    /// `jdk-21/legal/java.desktop/…`（约 6 层）。64 层足够，
    /// 而它挡的是"用极深路径把 Win32 的 260 限制撑爆"这一类。
    pub max_depth: usize,
    /// 单个相对路径的字符数上限。
    ///
    /// 默认 1024。**不能用 260**：本机 `LongPathsEnabled = 1`，
    /// 而且我们自己会给路径加 `\\?\` 前缀（加了就不受 260 限制）。
    /// 但也不能无限 —— 32767 是 NT 的硬上限，而 1024 已经远超任何真实制品。
    pub max_relative_path: usize,
}

impl Default for ExtractLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_total_bytes: 8 * 1024 * 1024 * 1024,
            max_entry_bytes: 2 * 1024 * 1024 * 1024,
            max_depth: 64,
            max_relative_path: 1024,
        }
    }
}

impl ExtractLimits {
    /// 给测试用的**极小**上限 —— 让"超限"这件事不需要造一个大归档就能测。
    #[must_use]
    pub fn tiny() -> Self {
        Self {
            max_entries: 3,
            max_total_bytes: 1024,
            max_entry_bytes: 256,
            max_depth: 2,
            max_relative_path: 32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_generous_enough_for_real_artifacts() {
        let limits = ExtractLimits::default();
        // Temurin JDK 实测约 2 万个条目、约 330 MB。默认值必须容得下它，
        // 否则我们会把**正经制品**判成炸弹。
        assert!(limits.max_entries >= 20_000);
        assert!(limits.max_total_bytes >= 330 * 1024 * 1024);
        // node 的 `.7z` 实测 2454 个条目、解压后约 110 MB。
        assert!(limits.max_entries >= 2_454);
        assert!(limits.max_total_bytes >= 110 * 1024 * 1024);
    }

    #[test]
    fn the_path_limit_is_not_260() {
        // 260 是**相对路径**的限制，而我们给路径加了 `\\?\` 前缀就不受它约束。
        // 把上限写成 260 会让一个完全正常的深层制品解不开。
        assert!(
            ExtractLimits::default().max_relative_path > 260,
            "上限不能是 260 —— 那是没加 \\\\?\\ 时的限制"
        );
        // 但也不能超过 NT 的 32767 硬上限。
        assert!(ExtractLimits::default().max_relative_path < 32_767);
    }

    #[test]
    fn tiny_limits_are_actually_tiny() {
        let limits = ExtractLimits::tiny();
        assert!(limits.max_entries < ExtractLimits::default().max_entries);
        assert!(limits.max_total_bytes < ExtractLimits::default().max_total_bytes);
        assert!(limits.max_depth < ExtractLimits::default().max_depth);
    }
}
