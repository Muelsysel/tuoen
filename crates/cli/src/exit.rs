//! 退出码。
//!
//! 约定与 Unix 惯例一致，且**在测试里钉死** —— 脚本会依赖它们。
//!
//! - `0` 成功
//! - `1` 运行期错误（IO、校验失败、环境不符）
//! - `2` 用法错误（由 clap 直接退出，本模块不产生）

/// 成功。
pub const SUCCESS: i32 = 0;

/// 运行期错误。
#[allow(
    dead_code,
    reason = "公开契约的一部分：ticket #2 只有 list，后续子命令会用到"
)]
pub const RUNTIME_ERROR: i32 = 1;

/// 用法错误。**由 clap 产生**，列在这里是为了让约定完整可读。
#[allow(dead_code, reason = "公开契约的一部分：由 clap 直接退出，本模块不产生")]
pub const USAGE_ERROR: i32 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_the_documented_values() {
        assert_eq!(SUCCESS, 0);
        assert_eq!(RUNTIME_ERROR, 1);
        assert_eq!(USAGE_ERROR, 2);
    }
}
