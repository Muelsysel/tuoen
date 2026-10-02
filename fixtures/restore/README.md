# `fixtures/restore/**` —— `tuoen restore` 的固定装置

**这些都是自造的 `tuoen.d/` 快照**，格式与 `crates/core/src/capture/files.rs` 的
`SchemaFile` / `ToolsFile` / `PathFile` / `EnvFile` / `WslFile` / `SkippedFile`
一一对应（真 TOML，由 `toml::from_str` 直接反序列化）。

**它们不是任何一台真机的捕获结果**，也不含任何真实凭据。每一个数字都是**故意选的**，
目的是让集成测试能**自己从这些文件里数出期望值**，而不是抄产品自己的输出。

## 目录

| 目录 | 是什么 | 用来测什么 |
|---|---|---|
| `machine-a/` | 目标侧：一台完整的机器（6 个文件齐全） | `plan(target, target)` 接近空；`plan(target, local)` 造出差异 |
| `machine-a-current/` | 本机侧：**与 `machine-a` 状态相同**，只有 `captured_at` 不同 | `captured_at` 是元数据，**不许**造成任何差异 |
| `machine-b-current/` | 本机侧：缺东西、也多东西 | `install` / `set-user` / `set-machine` / `unsupported` / 提权 |
| `halfway-local/` | 本机侧：**上一次 apply 做到一半**（path/env/wsl 已落地，工具装了 3 个缺 4 个） | 决策 155：**重跑就是继续**，只列出剩下那 4 件 |
| `path-only/` | 只有 `schema.toml` + `path.toml` | `--only tools` 指向**快照里不存在**的 section → `Skipped` |
| `empty/` | 只有 `schema.toml`，`sections = []` | 空快照 → `RestoreError::empty-snapshot` |

## `machine-a/` 里刻意造出来的形态（每一种都对应一条真实教训）

| 形态 | 在哪 | 为什么要有 |
|---|---|---|
| 第三方版本管理器管的工具 | `tools.toml` 的 `node` ×2（`manager = "nvm4w"`）、`uv`（`manager = "uv"`） | 决策 154：**不接管**别人的管理器。两个管理器 → `third-party-manager` 待办**按管理器去重**成 2 条，而不是按工具行 3 条 |
| 不可复现的工具 | `oracle-jdk`（`reproducible = false`）、`python`（`registered-missing`） | 缺了也只能报 `unsupported`；`oracle-jdk8` 另报 `licence-blocked` |
| 占位符路径 | `python` 的 `path = "<无 InstallLocation，卸载键 {…}>"` | 真机上 `detect` 就是这么写的。它含 `{}` 与中文，**不许原样进 plan**（决策 152：`subject`/`detail` 必须是稳定 slug）。唯一能定位它的卸载键必须留下 → `uninstall-key={GUID}` |
| 中文散文 | 每一行的 `evidence` | 真机上是 `"PATH 第 1 条（process-only）里有 cargo.exe"`。**plan 的任何字段都不许含 CJK** |
| 重复的 `PATH` 条目 | `path.toml` 的 `user` 第 2 条（`dup_index = 1`） | 决策 151：本机自己的健康问题是 `fix` 类，**默认不选中**，只报告（`note = "fix-not-selected"`） |
| 硬编码用户名 | `path.toml` 的 `C:\Users\dev\…`（`has_username = true`） | 换账号名后静默失效的那一类 |
| 第三方 reparse point | `path.toml` 的 `C:\nvm4w\nodejs`（`junction` + `link_target`） | `owner = "third-party"`：别人的版本切换器 |
| 三重目标存在性 | `env.toml` 的 `target_exists` 取 `yes` / `no` / `not-a-path` | 三态（含"根本不是路径"那一态）不许合并 |
| 需提权的机器级变量 | `env.toml` 的 `M2_HOME`（`scope = "machine"`），本机侧缺 | 决策 12/136：机器级要提权，tuoen 不静默提权 |
| 凭据命名的环境变量 | `skipped.toml` 的 `ARK_API_KEY`（`kind = "credential-named"`） | 决策 153：`credential-reconfigure` **只写意图**，材料绝不进 plan |
| **故意种下的 canary** | `env.toml` 的 `CANARY_LEAKED_TOKEN` | 见下 |
| WSL 差异 | `wsl.toml`：共享发行版 `base_path` 不同 + 目标独有 `Arch-Linux-current` | `path-differs` / `missing-distro` / `extra-distro` 三种都要有 |
| 非标准 WSL 路径 | `Arch-Linux-current` 的 `base_path = C:\linux\…` | 真机实测：手工导入的发行版不在默认位置 |
| 进程注入的 `PATH` 条目 | `path.toml` 的 `scope = "process-only"`（`reg_type` 缺省 = `None`） | 它不在任何注册表值里，**不是任何一方能改写的东西** → 只能计数，不许进差异。两侧的注入项**故意不同**（`pwsh.exe` vs `wt.exe`）：拿注入项去 diff 的实现会红 |

### `CANARY_LEAKED_TOKEN` 是什么

`machine-a/env.toml` 里有一条**故意种下的假凭据**：

```toml
value_raw = "TuoenCanary0000AbCdEfGhIjKlMnOpQrSt"
```

它的存在是为了让"plan 里不含任何 token 材料"这条断言**不是空的**：固定装置里**真的有**
一段 token 形状的字符串（一个写坏了的 `capture` 就会产出这种东西），而 `plan` 的输出里
**一个字节都不许出现**它 —— 连长度 8 的子串都不许（检查所有 8 字符窗口就足够：
更长的子串必然包含一个 8 字符窗口）。删掉这一行，那条断言就退化成"两份空串相同"
（§1.14 规矩 5）。

**它的形状是刻意的**：没有 `glpat-` / `ghp_` / `sk-` / `AKIA` 这类**真厂商前缀**。
理由不是洁癖 —— 本仓库被 GitHub push protection（GH013）拦过一次，原因就是
`crates/core/src/capture/secrets.rs` 里我们自己写的检测形状被它当成了真 token。
所以：固定装置里只放"高熵但明显是假的"字符串（里面就写着 `Canary`），
而测试里要查的 `glpat-` 前缀用 `concat!("glpat", "-")` 拼出来，不写成字面量。

它**不是**真实凭据，且只出现在这两份本机侧/目标侧快照里（两份都是假的）。

## 怎么从这些文件数出期望值

集成测试（`crates/core/tests/restore_plan.rs`）自己 `toml::from_str` 这些文件，
然后数：

- `tools.toml`：行数、`manager` 非空的行数、`manager` 的**不同取值**个数、
  `reproducible == false` 的行数、`confidence` 的分布；
- `path.toml`：条数、作用域分布、`dup_index > 0` 的条数、`has_username` 的条数、
  以及**自己的字符数**（与 `[budget]` 的那两个数对照 —— 固定装置自己也要自洽）；
- `env.toml`：行数、作用域分布、`target_exists` 分布；
- `wsl.toml`：发行版个数；
- `skipped.toml`：行数与 `kind` 分布。

**不从 `plan()` 的输出反推期望**（那等于拿产品自己的输出当答案）。

## 空快照为什么只有 `schema.toml`

git 不跟踪空目录，所以"一个 section 文件都没有"这个形状必须有一个**能被提交**的表示：
只放 `schema.toml` 且 `sections = []`。集成测试里另外用 `TempDir` 造一个**真正空**的
目录，两种形状都断言 `empty-snapshot`。
