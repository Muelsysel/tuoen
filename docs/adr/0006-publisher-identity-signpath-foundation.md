# ADR-0006: 发布者身份锁定为 SignPath Foundation

**状态**: 已接受（2026-10-02）
**来源**: `docs/DESIGN.md` 决策 31 相关；`docs/CODE_SIGNING.md`

## 背景

Windows 上未签名的可执行文件会触发 SmartScreen 警告，且 Windows 11 的 **Smart App Control** 可能直接阻止执行。

**Azure Trusted Signing / Artifact Signing 对中国大陆维护者永久不可用**——两道国别门槛都过不去：

- 个人开发者：**仅限美国或加拿大**
- 组织级 Public Trust：美国、加拿大、EU、UK、澳大利亚、NZ、日本、韩国、新加坡、瑞士、挪威、以色列——**不含中国**

即"注册个公司"也解决不了。

**[SignPath Foundation](https://signpath.org)** 是免费的 OSS 签名项目，且**结构上免疫国别问题**：证书签发给 **SignPath Foundation 自身**，不签发给维护者。原话："No need for personal identification, we verify that the binary was built from your open source repository and vouch for that with our name."

## 决策

**接受发布者身份为 `SignPath Foundation`**（而不是项目名 `tuoen`）。

**这是一个永久决定**，因此必须现在冻结：微软明确 **"Use a consistent signing identity — changing your signing certificate affects the publisher trust signal"**，且信誉**不能跨发布者身份转移**。

## 后果

- Windows 签名对话框将显示 `SignPath Foundation`，不是 `tuoen`。**starship（Rust CLI、GitHub Releases、Windows 二进制——形态最接近的先例）接受的就是这个取舍。**
- **不能第一天就申请**：SignPath 要求项目"must already be released in the form that should be signed"，且需要一定可验证信誉，录取是自由裁量。**流程是：v0.1 未签名发布 → 积累用户 → 再申请。**
- 因此 **v0.1 必须带一段"未签名版本怎么运行"的说明**（已写在 `docs/UNSIGNED_BUILD.md`）。
- 需要仓库卫生工作：LICENSE 在根目录、公开 release 流程、Code Signing Policy 章节（已写在 `docs/CODE_SIGNING.md`）、MFA。
- **三项 SignPath 条款需要与本设计对照**：① 无 hacking tools；② **"must not modify the user's system configuration without proper warnings"**——**我们改 `PATH`，因此警告与 plan/diff 是合规要求，不只是体验**；③ 必须提供卸载。

## 被否决的方案

- **SSL.com IV（约 $129/年 + eSigner）**：能得到项目品牌的发布者名，但**每年花钱**，且其中国大陆个人资格**未能核实**。
- **Certum**：中国大陆个人可以买（排除名单只有俄罗斯、白俄罗斯、朝鲜、古巴、叙利亚），但**明确不支持 CI/CD**（"Can I use the certificate in GitHub Actions or other CI/CD pipelines? Not at the moment."）——**作为主策略是致命的**。
- **自签名证书**：微软文档明确其行为**等同无签名**——只有困惑，没有好处。
- **不签名**：不是"用户多点一次"的问题，而是**每个版本的信誉都从零开始**（"Reputation cannot transfer from previous versions unless both were signed using the same publisher identity"），且**永远无法免费摆脱**。

## 必须知道的两个纠正

1. **"SAC 会阻止未签名文件"是夸大的**。官方判定顺序是**先查云端智能服务**，**只有云端无法判断时**才检查签名。所以未签名不等于必拦。
2. **对 Smart App Control，任何链到微软 Trusted Root Program 的证书都够**——不需要 EV，不需要 Azure。微软自 2024 年 8 月起**已废除 EV 与非 EV 的区分**（"all Code Signing certificates will be treated equally"）。
   **但签名不会让首次下载的蓝色警告消失**——微软自己的表格把 OV/EV 标注为"⚠️ 警告"。签名买到的是发布者名称、SAC 通过、以及**跨版本的信誉延续**。
