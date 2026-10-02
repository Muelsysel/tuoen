//! 捕获的测试支撑：把 [`tuoen_platform::fixture`] 的假机器接成"能跑 `capture`"的形状。
//!
//! **为什么不另造一套假后端**：`tuoen_platform::fixture` 已经是权威的那一套
//! （`MachineFixture` 可 `serde` 反序列化，`fixtures/` 下就是它），而
//! [`crate::detect::test_support::DetectFixture`] 已经把它接成了检测上下文。
//! 这里只做两件额外的事：
//!
//! 1. 把 [`CaptureOptions`] 也装配好（时间戳是**参数**，不是 `now()` ——
//!    幂等性因此可以被要求成"逐字节相同"，而不是"忽略某个字段之后相同"）；
//! 2. 给一个 [`CaptureFixture::files`]，直接把 bundle 渲染成 `文件名 → 文本`，
//!    于是"跑两次一样不一样"变成一次 `BTreeMap` 比较。
//!
//! **这个文件里的每一个入口都只读假机器**：真实注册表、真实 `PATH`、真实安装目录
//! 在这里根本够不着（`DetectContext` 里全是注入的 trait 对象）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use tuoen_platform::fixture::MachineFixture;

use crate::detect::test_support::DetectFixture;

use super::{CaptureBundle, CaptureError, CaptureOptions, Section, capture, render};

/// 一台假机器 + 我们自己的根。
#[derive(Debug, Clone)]
pub struct CaptureFixture {
    /// 检测上下文（假文件系统 / 假注册表 / 假环境块 / 假进程运行器）。
    pub detect: DetectFixture,
    /// 我们自己的根（存储根、shim 目录），只用来判 `owner = "tuoen"`。
    pub tuoen_roots: Vec<PathBuf>,
}

impl CaptureFixture {
    /// 按一份机器描述造固定装置。
    #[must_use]
    pub fn build(description: &MachineFixture) -> Self {
        Self {
            detect: DetectFixture::build(description),
            tuoen_roots: Vec::new(),
        }
    }

    /// 声明一个"这是我们自己的目录"的根。
    #[must_use]
    pub fn with_tuoen_root(mut self, root: &str) -> Self {
        self.tuoen_roots.push(PathBuf::from(root));
        self
    }

    /// 加一个扫描根（影响 `tools.toml` 里的 `directory-only` 条目）。
    #[must_use]
    pub fn with_scan_root(mut self, path: &str, why: &'static str) -> Self {
        self.detect = self.detect.clone().with_scan_root(path, why);
        self
    }

    /// 关掉版本探测（`tools.toml` 里的版本会全是 `None`）。
    #[must_use]
    pub fn without_probing(mut self) -> Self {
        self.detect = self.detect.clone().without_probing();
        self
    }

    /// 装配一份捕获选项。
    #[must_use]
    pub fn options(&self, sections: &[Section], captured_at: &str) -> CaptureOptions {
        CaptureOptions {
            out_dir: PathBuf::from("tuoen.d"),
            sections: sections.to_vec(),
            captured_at: captured_at.to_owned(),
            tuoen_roots: self.tuoen_roots.clone(),
        }
    }

    /// 跑一次捕获。**不落盘** —— 落盘是 `write_bundle` 的事，而绝大多数用例
    /// 关心的是内容而不是"文件真的在那儿"（那一条由 CLI 进程边界测）。
    ///
    /// # Panics
    ///
    /// 捕获本身只读，不返回错误；真的返回了就说明有 bug，直接 panic 比吞掉好。
    #[must_use]
    pub fn capture(&self, sections: &[Section], captured_at: &str) -> CaptureBundle {
        capture(&self.detect.context(), &self.options(sections, captured_at))
            .expect("捕获是只读的，不该失败")
    }

    /// 全部 section 的捕获。
    #[must_use]
    pub fn capture_all(&self, captured_at: &str) -> CaptureBundle {
        self.capture(&[], captured_at)
    }

    /// 渲染成 `文件名 → 文本`。
    ///
    /// # Errors
    ///
    /// 序列化失败（意味着某个字段的形状 TOML 表达不了）。
    pub fn try_files(
        &self,
        sections: &[Section],
        captured_at: &str,
    ) -> Result<BTreeMap<&'static str, String>, CaptureError> {
        let bundle = self.capture(sections, captured_at);
        Ok(render(&bundle)?.into_iter().collect())
    }

    /// 渲染成 `文件名 → 文本`，失败就 panic（用例里用它比较省事）。
    ///
    /// # Panics
    ///
    /// 序列化失败。
    #[must_use]
    pub fn files(&self, sections: &[Section], captured_at: &str) -> BTreeMap<&'static str, String> {
        self.try_files(sections, captured_at).expect("渲染不该失败")
    }

    /// 全部 section 的渲染结果。
    #[must_use]
    pub fn all_files(&self, captured_at: &str) -> BTreeMap<&'static str, String> {
        self.files(&[], captured_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capturing_twice_with_a_pinned_timestamp_is_byte_identical() {
        // 幂等性是这一票承诺的东西，而它最容易悄悄坏掉 —— 所以先在这里钉一次。
        let fixture = CaptureFixture::build(&MachineFixture::default());
        let first = fixture.all_files("2026-10-02T12:00:00Z");
        let second = fixture.all_files("2026-10-02T12:00:00Z");
        assert_eq!(first, second);
        assert!(first.contains_key("schema.toml"));
        assert!(
            first.contains_key("skipped.toml"),
            "扫过 env 就该有跳过清单"
        );
    }

    #[test]
    fn only_path_produces_exactly_two_files() {
        let fixture = CaptureFixture::build(&MachineFixture::default());
        let files = fixture.files(&[Section::Path], "2026-10-02T12:00:00Z");
        assert_eq!(
            files.keys().copied().collect::<Vec<_>>(),
            vec!["path.toml", "schema.toml"],
            "`--only path` 只产出这两个：跳过清单是「扫过环境变量」的产物"
        );
    }

    #[test]
    fn the_timestamp_really_is_the_only_difference_between_two_runs() {
        use crate::capture::without_timestamp;
        let fixture = CaptureFixture::build(&MachineFixture::default());
        let a = fixture.all_files("2026-10-02T12:00:00Z");
        let b = fixture.all_files("2026-10-02T13:30:00Z");
        assert_ne!(a, b, "时间戳不同，文件当然不同");
        for (name, text_a) in &a {
            let text_b = &b[name];
            assert_eq!(
                without_timestamp(text_a),
                without_timestamp(text_b),
                "`{name}` 除了时间戳之外不该有任何差别"
            );
        }
    }
}
