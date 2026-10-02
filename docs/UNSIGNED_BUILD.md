# 未签名版本：运行前请先读这个

**当前发布物（v0.x）没有代码签名。** 这是有意为之，也是开源项目的常态，但它意味着
Windows 会对你发出警告。本文件说明你会看到什么、怎么安全地绕过、以及为什么可以信任这个二进制。

---

## 为什么没有签名

签名需要一个代码签名证书，而获取证书的路径对中国大陆的个人开发者存在实际障碍：

- **Azure Trusted Signing / Artifact Signing 对中国大陆维护者永久不可用** —— 其个人开发者身份验证
  仅限**美国与加拿大**；即使注册公司，组织级 Public Trust 支持的国家列表中**也没有中国**。
- 本项目计划申请 **[SignPath Foundation](https://signpath.org)** 的**免费**开源签名 —— 它的证书签发给
  SignPath Foundation 自身，因此没有国别门槛。但该项目要求**项目已经以要被签名的形态发布过**，
  且需要一定的可验证信誉，**所以必须先发布未签名版本、积累用户，之后才能申请**。

**因此：未签名是这条路径上的第一步，不是终点。** 相关承诺记录在
[`CODE_SIGNING.md`](CODE_SIGNING.md)。

**为什么不用自签名证书**：微软文档明确说明，自签名证书的行为**等同于无签名** —— 它不会带来任何
好处，只会增加困惑。

---

## 第一步：先校验哈希（比什么都重要）

**从 GitHub Releases 下载后，先核对 SHA256。** 每个发布物都附带校验和：

```powershell
Get-FileHash .\tuoen.exe -Algorithm SHA256
```

把结果与 Release 页面上的 `SHA256SUMS` 对比。**不一致就不要运行。**
这一步比签名更重要 —— 签名证明"是谁构建的"，哈希证明"你拿到的就是那个构建"。

---

## 第二步：你会看到什么

从浏览器下载的 `.exe` 带有 **Mark of the Web**（Windows 给下载文件打的标记），
所以首次运行会看到蓝色的 **"Windows 已保护你的电脑"** 对话框：

```
Windows 已保护你的电脑
Microsoft Defender SmartScreen 阻止了无法识别的应用启动。
运行此应用可能会导致你的电脑存在风险。

发布者：未知发布者

[ 不运行 ]   [ 更多信息 ]
```

**点"更多信息"后会出现"仍要运行"按钮**，点它即可。

---

## 三个处理方式

### 方式一：解除文件锁定（推荐）

最干净的做法是**去掉文件的 Mark of the Web**，之后运行就不会再弹窗：

```powershell
Unblock-File .\tuoen.exe
```

图形界面等效操作：右键文件 → 属性 → 常规 → 勾选底部的**"解除锁定"** → 确定。

> 这一步**只影响你手动解除锁定的那个文件**，不改变任何系统安全设置。

### 方式二：点击通过警告

按上面的对话框：**更多信息 → 仍要运行**。

### 方式三：通过包管理器安装（无警告）

通过 winget 或 Scoop 安装时不会经过浏览器的下载标记，因此**通常不会触发这个弹窗**。

> ⚠️ 但这**不是** SmartScreen 的绕过手段 —— 有实测案例显示 winget 安装的二进制同样可能被拦。
> 它只是省掉了浏览器下载这一环。

---

## 如果你看到的是"Smart App Control 已阻止此应用"

这是**另一个更严格的机制**，与上面的蓝色弹窗不是一回事。区别很重要：

- **SmartScreen**（蓝色弹窗）：可以点"仍要运行"绕过
- **Smart App Control（SAC）**：**没有按应用放行的选项** —— 被拦就是被拦

**它是怎么决定的**（微软官方 FAQ 的判定顺序）：
先查云端智能服务能否对该应用的安全性做出有信心的判断；**只有当云端无法判断时**，才转而检查签名。
所以未签名**不等于必然被拦** —— 有名气的未签名程序通常能通过。

**如何检查 SAC 是否开启**：

```powershell
Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy' -Name VerifiedAndReputablePolicyState
```

```
0 = 关闭    1 = 强制启用    2 = 评估模式
```

也可以从 **Windows 安全中心 → 应用和浏览器控制 → Smart App Control** 查看。

**如果它拦住了 `tuoen`**：只能整个关闭 SAC（Windows 安全中心里操作）。
**请注意 SAC 关闭后无法重新开启**，除非重装系统 —— 所以是否关闭由你自己权衡。
另外：微软说明 SAC **只在部分区域启用**（未公布区域列表），且开启了开发者模式的机器会自动关闭它。

---

## 不要做的事

- **不要**从非本仓库 Releases 的来源下载 `tuoen.exe`。唯一官方来源是本仓库的 GitHub Releases。
- **不要**因为"未签名"就去关闭 SmartScreen 整体保护 —— 用上面的方式一或方式二即可。
- **不要**使用第三方"签名修复"工具。

---

## 我们会在签名可用后做什么

一旦 SignPath 申请通过，签名版本会：

- 显示**发布者名称**（`SignPath Foundation`）而不是"未知发布者"
- 让 **Smart App Control 的判定直接通过**（任何链到微软 Trusted Root Program 的证书都足够，
  且微软自 2024 年 8 月起已废除 EV 与非 EV 的区分）
- **让信誉跨版本累积** —— 这是签名最实际的价值：未签名时，**每一个新版本的信誉都从零开始**
  （微软原话："Reputation cannot transfer from previous versions unless both were signed
  using the same publisher identity"）

**注意**：签名**不会**让首次下载的蓝色警告消失 —— 微软自己的表格把有效证书（OV/EV）标注为
"⚠️ 警告 —— 在信誉累积前应用被标记为无法识别"。签名买到的是发布者名称、SAC 通过、
以及**跨版本的信誉延续**。

---

## English (summary)

**Current releases (v0.x) are unsigned.** Azure Trusted Signing is permanently unavailable to a
mainland-China maintainer, so the plan is to apply to [SignPath Foundation](https://signpath.org)
once the project has real users — that programme is free and has no country gate.

**Verify the SHA256 checksum before running** (`Get-FileHash .\tuoen.exe -Algorithm SHA256`).
Then either `Unblock-File .\tuoen.exe` (recommended) or click **More info → Run anyway**.
Self-signed certificates are documented by Microsoft as behaving the same as no signature,
so we do not use one. If you see **Smart App Control blocked this app**, that is a different,
stricter mechanism with no per-app override.
