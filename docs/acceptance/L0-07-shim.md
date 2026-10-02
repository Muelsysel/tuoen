# L0-07 · shim（真 `.exe` 转发器）验收记录

**票据**：[#7](https://github.com/Muelsysel/tuoen/issues/7) · **状态**：通过
**代码**：`crates/shim/`（lib + bin）、`crates/core/src/shim.rs`、`crates/cli/src/shim*.rs`
**机器可读工件**：`docs/acceptance/L0-07-shim-machine.txt`（由 `scripts/acceptance-L0-07.ps1` 生成，25 项检查全通过、exit 0）

> 这份文档只写**怎么验的**与**验出了什么**。设计的理由在 `docs/DESIGN.md` 决策 52–59。

---

## 1. 验收标准逐条

| # | 标准 | 判据（可失败） | 证据 | 结果 |
|---|---|---|---|---|
| ① | 独立的极小二进制，不把 GUI/CLI 依赖图拖进启动路径 | `cargo tree -p tuoen-shim` 的依赖只有 `windows-sys`（dev-deps 不算） | `crates/shim/Cargo.toml` 的 `[dependencies]` 只有一行；lib 里手写 `Display`，不用 `thiserror`，也不用 `serde` | **通过** |
| ② | 目标路径**生成时写死进 shim** | 模板里有一处「未烘过」的槽位；生成之后**一处都不剩**，且每个原偏移都能解出新前缀 | `every_slot_in_the_template_gets_baked`（`tests/shim_contract.rs`）；`scripts/dump-pe-sections.ps1` 的 `SUMMARY` 行给出 `pristine=N` | **通过**（release 模板 `magic_hits=1 pristine=1`） |
| ③ | 只发 `.exe` | 目录里只有 `tuoen-shim.exe` 与生成的 `<name>.exe`；`.cmd` / `.ps1` 从不产生 | `ShimSpec::file_name()` 硬编码 `.exe`；`crates/shim/src/lib.rs` 的 crate 文档写明理由（决策 6 / CVE-2024-27980） | **通过** |
| ④ | 切原始 `GetCommandLineW` 而不是重新引号化 argv | 重现用户敲的**原文**后逐字节转发 | `raw_command_lines_are_sliced_like_the_crt_would`、`arguments_reach_the_target_verbatim`（13 个边界用例：空格 / 引号 / `=` / 尾部反斜杠 / 空参数 / tab） | **通过** |
| ⑤ | 退出码透传，含异常码 | `0xFFFFFFFF` 与真异常码 `0xC0000005` 都逐位交回 | `exit_codes_are_forwarded_including_the_all_ones_code`、`exception_codes_are_forwarded_bit_for_bit` | **通过** |
| ⑥ | Ctrl-C 正确透传 | **对照实验**：反例（少写一行的转发器）在信号后 1–2 ms 以 `0xC000013A` 结束；我们的 shim 在 1114 ms 后带着子进程的退出码 `7` 结束 | `ctrlc_host.exe` / `ctrlc_runner.exe` / `naive_forwarder.exe`，输出见工件 §4 | **通过** |
| ⑦ | 实测启动开销 | 先过器材自检（确实在等 / stdout 逐字节一致），再报**增量**而不是总量 | `startup_probe.exe`，输出见工件 §3 | **通过**（增量约 9–11 ms） |
| ⑧ | 测试参数转发边界与退出码 | 43 条（24 lib + 19 进程边界），**debug 与 release 各跑一遍** | 工件 §5；决策 59 解释了为什么必须两个 profile | **通过** |
| ⑨ | `cargo test` 与 clippy 通过 | `cargo test --workspace` → **558 passed / 0 failed**；`cargo clippy --workspace --all-targets -- -D warnings` → exit 0；`cargo fmt --all --check` → exit 0 | 工件 §5 | **通过** |

---

## 2. 启动开销的原始数字（验收标准 ⑦）

release profile（`lto=true codegen-units=1 strip=true panic=abort`），每例 40 轮，前 6 轮预热不计，单位毫秒。

| 用例 | 最小 | 中位 | p90 | 最大 |
|---|---|---|---|---|
| `argdump --noop` 直接跑（进程启动的地板） | 4.9 | **5.6** | 6.3 | 19.0 |
| `argdump --noop` 经过 shim | 13.3 | **16.3** | 20.5 | 30.7 |
| `node.exe --version` 直接跑 | 19.6 | **23.1** | 29.9 | 36.6 |
| `node.exe --version` 经过 shim | 29.0 | **33.3** | 37.5 | 43.5 |

**结论：转发增量约 9–11 ms**（两次独立运行：10.2 / 11.1；另一次 9.1）。

三条必须一起看的注记：

1. **这个口径是增量，不是总量。** 调研里那个「Scoop 原生 shim 33–36 ms」是**总量**口径，两者不可直接比较。我们的**总量**（对一个小程序 16 ms、对 `node --version` 33 ms）与它同量级或更好，但那样说仍然是不严谨的 —— 两台机器、两种测法。
2. **地板的约 5 ms 是"一个 Rust 进程起来就退"**（`argdump --noop`）。转发器至少要再多起一个进程并等它，所以 9 ms 里没有"莫名其妙的开销"。
3. **模板 287,744 字节**（`read_volatile` 版；没有它是 262,144）。`no_main` 手工入口能再压，**V1 不做**。

---

## 3. 槽位：三种"元数据全对但程序不对"的坑

这一节是本票花时间最多的地方，三次都是「每个单独的检查都说它是对的」，值得单独记下来。

### 3.1 魔数出现 4 次，只有 1 处是真的

| 文件偏移 | 是「未烘过」的槽位 |
|---|---|
| 710964 | **是** |
| 718096 | 否（其后第 16 字节是 `0x54`） |
| 718112 | 否（`0x5C`） |
| 719048 | 否（`0xE9`） |

debug 模板里魔数出现 4 次，其余三处是编译产物里的常量。第一版生成器「找到第一处就下手」，**碰巧**改对了。判据因此改成「整段 2076 字节逐字节等于 `baked_slot()`」，并且**改写全部候选**（决策 53）。

### 3.2 文件改了，运行时代码看不见

release 下生成的 shim 打印「这是一个**模板**」，而：

- 生成器的文件自检**全过**（读回来确实出现了新前缀）；
- `pristine_slot_offsets` 说"一处都不剩"；
- PE 节表说槽位稳稳落在 `.rdata` 的 raw data 窗口里（`slots_inside_raw == magic_hits`）。

**根因**：LTO 把 `decode_slot(&SLOT)` 常量折叠了 —— `SLOT` 是只读 static，`#[used]` 只保证字节被写进目标文件，**不保证有人真的去读它**。修法是 `read_volatile`（决策 54）。

### 3.3 抓住 3.2 的是一条"器材自检"

第一版探针量出了「经过 shim 的 `node --version` 比直接跑还**快 17 ms**」。一个转发器不可能让它转发的东西变快 —— 于是加了自检（先证明转发是真的、再计时），它立刻把 3.2 逼了出来。**一条不能失败的测量不是测量**（决策 57）。

---

## 4. Ctrl-C 对照实验

```
私有控制台窗口句柄：0xd243a（0 = 没有控制台，实验无效）

── 反例：少写一行的转发器
   GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) = 1，GetLastError=0
   它在信号之后 1ms 结束，退出码 0xC000013A
   判决：转发器先死了 —— 调用者拿回提示符了，而子进程还在后台跑

── 我们的 shim
   GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) = 1，GetLastError=0
   它在信号之后 1114ms 结束，退出码 0x00000007
   判决：转发器活到了最后，等完了子进程，并把它的退出码（7）交了出来
```

**为什么必须这么绕**（两条都被实测堵死了）：

1. **发给组 0 会把发信号的探针自己打死** —— 第一次实验就这么死的，什么都没测到。
2. **`CREATE_NEW_PROCESS_GROUP` 会关掉整个新进程组的 Ctrl-C** —— 两个子进程（一个装了处理器、一个没装）朝该组发信号后**都毫发无伤**（退出码 0 与 7）。用它做实验等于把要测的信号掐掉。

所以只剩「自己造一个私有控制台」。父模式用 `CREATE_NEW_CONSOLE | SW_HIDE` 起执行者，执行者对自己的控制台发组 0 信号。

**只证明"我们的 shim 活下来了"是不够的** —— 那无法排除「这个实验根本送不到信号」。`naive_forwarder.exe` 与真 shim 只差一行 `SetConsoleCtrlHandler(None, 1)`（文件里那行代码被注释掉留着），它的 1 ms 与我们的 1114 ms 之间的差距，就是这条验收的全部内容。

---

## 5. 本票期间的两次事故（都写进了 `AGENTS.md` 的铁律）

两次都不是"逻辑写错"，而是**代价不对称**：一个名字 / 一个字节的差错，换来整机失去响应。

| | 第一次 | 第二次 |
|---|---|---|
| 触发 | 测试让 shim 的**落盘路径与目标路径是同一个文件** | Ctrl-C 探针的宿主"fork 自己"进入子模式 |
| 机制 | 生成的转发器"目标是它自己"，无限自我启动 | `wide()` 自带 NUL 结尾，参数被拼在那个 NUL **后面** → 子进程收到**零个参数** → 回到宿主模式又 fork |
| 规模 | **20580 个进程**，内存吃干 | **409 个进程**，每个带一个不可见控制台 + 继承的 stdout 管道（管道不关，调用方永不返回） |
| 修法 | `DestinationIsTarget` 生成时守卫 + 运行时自指保险丝（决策 55） | 拆成两个二进制（宿主 **绝不**启动自己）；命令行只留一个构造点（决策 58） |

第二次的教训比第一次更结构性：**只要一个程序会启动它自己，参数传递出错就会无限放大**。现在的结构里没有任何程序会启动自己，宿主在动手前还会断言"执行者不是我"。

---

## 6. 诚实未覆盖（这张验收**没有**证明的事）

1. **没有跑过真正的端到端**：`tuoen shim add node` 在**真实机器上装完真 node 再生成 shim** 这条路径没跑，因为那需要真下载（属于 L0-09 真机验收）。本票覆盖的是：库层 43 条测试 + CLI 层 25 条进程边界契约测试（用 `IsolatedHome` 与合成载荷）+ 探针层两次真机实验。
2. **`current` 是悬空 junction 时**（版本目录被单独删掉）的 `payload-missing` 分支没有测试覆盖；那种状态下 `store::active_version` 究竟返回 `Some` 还是 `None` 也未验证 —— 可能先报 `no-active-version`。
3. **发布打包没有任何一步把 `tuoen-shim.exe` 放到 `tuoen.exe` 旁边。** `shim add` 靠 `find_template(exe_dir())`，所以发布形态必须让两个 exe 并列，而仓库里目前没有做这件事的步骤。**建议单独开一张票。**
4. **反例的子进程去向没有核对**：报告里说"子进程还在后台跑"，那是从反例的 1 ms 退出与退出码推断的，没有真去 `Get-Process` 找那个孤儿。
5. **并发没测**：junction 被翻转的**同时**正在运行一个 shim 会发生什么，未验证。
6. **前缀上限是 1024 个 UTF-16 单元**：超过会被 `PrefixTooLong` 拒掉，这是设计如此；但没有实测"一个真实世界里超长的 node 路径到底是什么长度"。
7. **`shim list` / `shim remove` 只认我们自己的 shim 目录**，不认识别的工具放在 `PATH` 上的同名文件 —— 覆盖别的东西由 `DestinationNotAShim` 拦住（这是有意的），但"用户 PATH 上还有另一个 `node.exe` 排在我们前面"这种情况本票没有处理（属于 PATH 重建票 #14 的范围）。

---

## 7. 复现

```powershell
# 前置：MinGW 的 bin 在 PATH（见 AGENTS.md「构建这台机器」）
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"
cd C:\Work\EasyEnvForWin

pwsh scripts/acceptance-L0-07.ps1                       # 25 项检查，exit 0 = 通过
pwsh scripts/dump-pe-sections.ps1 -Path target\release\tuoen-shim.exe
```

单独跑某一件：

```powershell
cargo test -p tuoen-shim                    # 43 条
cargo test --release -p tuoen-shim          # 同上，但能看见只在 LTO 下出现的 bug
cargo run --release -p tuoen-shim --example startup_probe   # 启动开销 + 器材自检
target\release\examples\ctrlc_host.exe                      # Ctrl-C 对照实验
```

*本文件的每条结论都指向一份可以重跑的实测；未覆盖的项目列在第 6 节，没有含糊过去。*
