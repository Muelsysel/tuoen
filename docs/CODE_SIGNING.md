# 代码签名政策 · Code Signing Policy

本文件是 [SignPath Foundation](https://signpath.org) 开源代码签名项目的申请与合规依据，
同时向用户说明**谁有权签署发布物、以及为什么这些二进制值得信任**。

**当前状态：v0.x 发布物为未签名版本。** 运行时的 Windows 警告及处理方式见
[`UNSIGNED_BUILD.md`](UNSIGNED_BUILD.md)。

---

## 免费签名来源

> Free code signing provided by [SignPath.io](https://signpath.io), certificate by
> [SignPath Foundation](https://signpath.org).

证书签发给 **SignPath Foundation**（而非项目维护者），因此 Windows 的发布者名称显示为
`SignPath Foundation`。这是本项目的**既定且永久的发布者身份** —— 更换签名身份会作废已积累的
SmartScreen 信誉（微软明确说明信誉不能跨发布者身份转移）。

**为什么不使用 Azure Trusted Signing**：其个人开发者身份验证仅限美国与加拿大，组织级
Public Trust 支持的国家列表也不含中国，因此对中国大陆的维护者永久不可用。
详见调研记录（`research/WINDOWS_CODE_SIGNING.md`，本仓库未公开）。

## 什么会被签名

仅签名**由本仓库的公开构建流程产生**的发布物：

- `tuoen.exe`（CLI 二进制）
- 安装包 / 压缩归档（若发布）
- GUI 可执行文件（GUI 里程碑之后）

**不会签名**：任何由第三方或非公开流程构建的二进制；任何被本项目作为工具链**下载**的上游制品
（那些制品由各自的上游厂商签名，见下）。

## 签名角色

任何签名请求必须由 **Approver** 人工审核并批准。三个角色如下：

| 角色 | 成员 | 职责 |
|---|---|---|
| **Author（作者）** | Muelsyse | 编写代码、提交变更 |
| **Reviewer（审查者）** | Muelsyse | 审查变更 |
| **Approver（批准者）** | Muelsyse | 批准签名请求；对发布物内容负最终责任 |

**单人项目的如实声明**：本项目目前是单人维护。三个角色由同一人承担 —— 这意味着**不存在
独立复核**，我们如实披露而不假装有。当项目出现第二位常规贡献者时，本节将更新为真实分工，
且 **Author 与 Approver 将分离**。

**签名请求的审核标准**（无论由谁批准，均逐条检查）：

1. 构建来源是本仓库的公开 commit，且该 commit 已在 `main` 分支上
2. 发布物由 CI 从该 commit 构建，**非本地构建**
3. 变更已通过 `cargo test` 与 `cargo clippy`
4. 发布物附带 SHA256 校验和

## 多因素认证

SignPath 账号与 GitHub 账号均启用 MFA。

## 供应链

- 依赖全部来自 crates.io，版本锁定在 `Cargo.lock`
- 发布构建在 GitHub Actions 上进行，日志公开可查
- 每个发布物附带 SHA256 校验和

## 关于本项目下载的上游制品

`tuoen` 会从上游厂商下载开发工具链。**这些制品不由本项目签名**，其完整性与签名由上游负责，
且**每个制品的许可证是模型中的一等字段**：

| 上游 | 再分发 |
|---|---|
| Eclipse Temurin / CPython / Node.js | 允许（在许可条款内） |
| Oracle JDK 21+ (NFTC) | 有条件 —— 仅在不收费的前提下可再分发未修改版本 |
| Oracle JDK 8 / 11 / 17 | **不可再分发** |
| MSVC / VS Build Tools | **不可再分发**（以管理员调用微软自己的 bootstrapper） |

我们**默认只镜像元数据（URL + SHA256），不镜像二进制**；只在许可明确允许处重新托管。

## 隐私

签名服务仅接收待签名的构建产物。不向其传输用户数据、遥测或任何个人信息。

## 分发

- 唯一官方来源：本仓库的 GitHub Releases
- 通过 winget / Scoop 分发时，manifest 指向上述同一发布物

## 修改本政策

本政策的任何变更都以公开 commit 的形式记录在本仓库历史中。

---

## English (summary)

Code signing policy for `tuoen`. Free code signing provided by
[SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).
Only artifacts built from this repository's public CI are signed; every signing request is
manually approved. This is currently a single-maintainer project — the Author, Reviewer and
Approver roles are held by the same person, and that is disclosed rather than disguised.
MFA is enabled on both SignPath and GitHub. Upstream toolchain artifacts downloaded by `tuoen`
are signed by their respective vendors and are **not** signed by this project.
