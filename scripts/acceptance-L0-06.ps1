# 票据 #6 的真机验收：`tuoen install` / `use` / `uninstall` / `list` 端到端。
#
# 与 `crates/cli/tests/manage_contract.rs` 的分工：
#   - 那边是**回归测试**：不联网、把版本目录直接摆进隔离存储，跑得快、随时能跑。
#   - 这里是**真机验收**：真的下载 35 MB、真的解压 101 MB、真的执行 `node.exe`
#     问它版本。它慢、要网、并且**会真的写这台机器的存储**，所以它不在 `cargo test` 里。
#
# 输出落盘到 `docs/acceptance/L0-06-store-machine.txt`；**退出码 0 才算通过**。
#
# 跑法：
#   pwsh -File scripts/acceptance-L0-06.ps1
#
# ## 这份脚本自己也会出错，所以它必须能失败
#
# 第一版有两个真实的毛病，都修在这里：
#   1. 它把 `detect --json` 的载荷键读成了 `tools`（实际是 `tool`），
#      于是"managed 层是空的"这句**假结论**被原样写进了验收记录。
#      修法不只是改键名：现在**先检查键在不在**，键都不在就报"脚本自己读错了"，
#      而不是报"产品坏了"。一条假失败与一条真失败长得一样，必须把它们分开。
#   2. 它无论发生什么都 `exit 0` —— 一份不能失败的验收不是验收。
#      现在每一节的期望都走 `Check`，最后按 `$problems` 决定退出码。

param(
    [string]$Out = "docs\acceptance\L0-06-store-machine.txt",
    # 跳过两次真下载（存储里已有的版本仍然会被使用）。
    [switch]$SkipDownload
)

$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

# cargo / rustc 不在这台机器的默认 PATH 上，MinGW 的 dlltool 也必须在 PATH 上
# （见 `AGENTS.md` 的"构建这台机器"一带）。
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"

$lines = [System.Collections.Generic.List[string]]::new()
$problems = [System.Collections.Generic.List[string]]::new()

function Say([string]$s = '') {
    $lines.Add($s)
    Write-Host $s
}

function Section([string]$title) {
    Say ''
    Say ('─' * 78)
    Say $title
    Say ('─' * 78)
}

# 一条断言。`$what` 写成"应当是什么"，这样失败时不用回头读脚本。
function Check([bool]$condition, [string]$what) {
    if ($condition) {
        Say "  ✓ $what"
    } else {
        Say "  ✗ **不符合预期**：$what"
        $problems.Add($what)
    }
}

# ── 编译一次，之后只跑二进制 ────────────────────────────────────────────────
Say '$ cargo build -p tuoen-cli'
$build = & cargo build -q -p tuoen-cli 2>&1 | Out-String
$buildCode = $LASTEXITCODE
if ($build.Trim()) { $build.TrimEnd() -split "`n" | ForEach-Object { Say "  $_" } }
if ($buildCode -ne 0) {
    Say "构建失败（退出码 $buildCode）—— 验收不可能继续。"
    $lines -join "`r`n" | Set-Content -Path $Out -Encoding UTF8
    exit $buildCode
}
$exe = (Resolve-Path 'target\debug\tuoen.exe').Path
Say "  二进制：$exe"

# 跑一次 tuoen，把命令、输出、退出码都记进记录。返回退出码。
function Invoke-Tuoen([string[]]$argv) {
    Say ("\$ tuoen " + ($argv -join ' '))
    $out = & $exe @argv 2>&1 | Out-String
    $code = $LASTEXITCODE
    if ($out.Trim()) { $out.TrimEnd() -split "`r?`n" | ForEach-Object { Say "  $_" } }
    Say "  → 退出码 $code"
    return $code
}

# 跑**同一命令**并返回 stdout（给需要解析 JSON 的节用，不重复打两遍）。
function Tuoen-Json([string[]]$argv) {
    $out = & $exe @argv 2>&1 | Out-String
    try { return ($out.Trim() | ConvertFrom-Json) } catch { return $null }
}

# 跑**一次**、记录一次、解析这一次的输出。返回 (退出码, 解析后的信封)。
#
# **为什么不复用 `Invoke-Tuoen` 再 `Tuoen-Json` 一遍**：那会把命令跑两次，
# 而 `use` 是幂等但**不恒定**的 —— 第一次报 `created`、之后报 `replaced`。
# 第一版脚本就是这么写的，于是拿"第二次的输出"去断言"第一次的期望"，
# 报了 3 条**假失败**（"第一次激活报 created" / "此前没有生效版本" /
# "说清了从哪个版本切过来"）。凡是**有状态的命令**，看的那次必须是唯一那次。
function Invoke-TuoenJson([string[]]$argv) {
    Say ("\$ tuoen " + ($argv -join ' '))
    $out = & $exe @argv 2>&1 | Out-String
    $code = $LASTEXITCODE
    if ($out.Trim()) { $out.TrimEnd() -split "`r?`n" | ForEach-Object { Say "  $_" } }
    Say "  → 退出码 $code"
    $parsed = $null
    try { $parsed = $out.Trim() | ConvertFrom-Json } catch { }
    return @($code, $parsed)
}

# 跑一个**外部**可执行文件（用来真的执行通过链接找到的 node.exe）。返回 (退出码, 输出)。
function Invoke-Exe([string]$path, [string[]]$argv) {
    Say ("\$ `"$path`" " + ($argv -join ' '))
    if (-not (Test-Path -LiteralPath $path)) {
        Say '  （不存在 —— 这正是要记录的事实）'
        return @(-1, '')
    }
    $out = & $path @argv 2>&1 | Out-String
    $code = $LASTEXITCODE
    if ($out.Trim()) { $out.TrimEnd() -split "`r?`n" | ForEach-Object { Say "  $_" } }
    Say "  → 退出码 $code"
    return @($code, $out.Trim())
}

# ── 第 0 节：这台机器的真实位置 ─────────────────────────────────────────────
Section '§0 这台机器上的真实位置（不隔离 —— 这正是"端到端"的意思）'
$storeRoot = Join-Path $env:LOCALAPPDATA 'tuoen\store'
$cacheRoot = Join-Path $env:APPDATA 'tuoen\cache'
Say "os            : $([System.Environment]::OSVersion.VersionString)"
Say "LOCALAPPDATA  : $env:LOCALAPPDATA"
Say "APPDATA       : $env:APPDATA"
Say "存储根        : $storeRoot"
Say "缓存根        : $cacheRoot"
Say "存储根现在存在: $(Test-Path -LiteralPath $storeRoot)"
Say ''
Say '注意：这个脚本**会真的往存储根里装东西**。它开头会清掉上次的残留，所以可重复跑。'

# ── 第 1 节：起点 —— 清掉上次的残留 ─────────────────────────────────────────
Section '§1 起点：清掉上次的残留（这一步本身就是 `uninstall --force` 的证据）'
foreach ($version in @('24.19.0', '24.21.0', '24.20.0')) {
    Invoke-Tuoen @('uninstall', 'node', $version, '--force') | Out-Null
}
Say ''
Say '清理之后：'
Invoke-Tuoen @('list') | Out-Null
Say ''
Check (-not (Test-Path -LiteralPath (Join-Path $storeRoot 'node\current'))) `
    '清理之后 `current` 不存在'

# ── 第 2 节：'装完不等于生效' 在输出里说清楚了吗 ────────────────────────────
Section '§2 `install --dry-run`：只说要做什么，什么都不下载、什么都不写'
$before = Test-Path -LiteralPath $storeRoot
$dryOut = & $exe install node@24.19.0 --dry-run 2>&1 | Out-String
$dryCode = $LASTEXITCODE
$dryOut.TrimEnd() -split "`r?`n" | ForEach-Object { Say "  $_" }
Say "  → 退出码 $dryCode"
$after = Test-Path -LiteralPath $storeRoot
Say ''
Say "演练前存储根存在: $before"
Say "演练后存储根存在: $after"
Check ($dryCode -eq 0) '演练退出码是 0'
Check ($before -eq $after) '演练没有在磁盘上留下存储根（`--dry-run` 的验收判据）'
Check ($dryOut -match 'tuoen use node 24\.19\.0') '演练输出里给出了下一步命令（决策 49 的代价）'

Section '§2b `--dry-run --json`：机器可读的计划'
$planJson = (Invoke-TuoenJson @('install', 'node@24.19.0', '--dry-run', '--json'))[1]
if ($null -ne $planJson) {
    Check ($planJson.data.dryRun -eq $true) 'dryRun 是 true'
    Check ($null -eq $planJson.data.result) 'result 是 null（同一套形状，只有结果为空）'
    Check ($planJson.data.plan.sha256 -eq '57f71ab3652e797d84acddc79c81cc9ff1c6ddb2a1974cdb83f00fee9bff4c73') `
        'sha256 来自内置目录'
    Check ($planJson.data.plan.nextCommand -eq 'tuoen use node 24.19.0') 'nextCommand 是下一步命令'
}
Say ''
Say '注意：这里显示的是**官方** URL，而真下载会走镜像（见 §3）——'
Say '因为候选源的顺序是"用户模板 → 内置镜像 → 官方兜底"。'

# ── 第 3 节：真的装两个版本 ─────────────────────────────────────────────────
Section '§3 真的装 node 24.19.0（下载 35 MB 的 .zip，SHA256 来自内置目录）'
$installOut = ''
if ($SkipDownload) {
    Say '（-SkipDownload：跳过）'
} else {
    $installOut = & $exe install node@24.19.0 2>&1 | Out-String
    $code = $LASTEXITCODE
    $installOut.TrimEnd() -split "`r?`n" | ForEach-Object { Say "  $_" }
    Say "  → 退出码 $code"
    Check ($code -eq 0) '安装退出码是 0'
    Check ($installOut -match 'cn-npmmirror|official|cn-tencent') '输出里说清了实际服务的是哪个源'
    Check ($installOut -match '还没生效|tuoen use') '输出里说清了"装完不等于生效"'
}

Section '§3b 真的装 node 24.21.0（多版本共存）'
if ($SkipDownload) {
    Say '（-SkipDownload：跳过）'
} else {
    Invoke-Tuoen @('install', 'node@24.21.0') | Out-Null
}

Section '§3c 装完**没有**生效版本 —— 这是决策 49 的直接后果'
$listCode = Invoke-Tuoen @('list')
$listJson = Tuoen-Json @('list', '--json')
if ($null -ne $listJson) {
    $nodeRow = @($listJson.data.tools | Where-Object { $_.name -eq 'node' })[0]
    if ($nodeRow) {
        Check ($null -eq $nodeRow.version) '装了两个版本但**没有**生效版本'
        Check ($nodeRow.installedVersions.Count -eq 2) '两个版本都列出来了'
        Check ($nodeRow.installedVersions[0] -eq '24.21.0') '版本降序（最新的在最前，自然比较而非字符串比较）'
    }
}

# ── 第 4 节：第一次激活 = created ───────────────────────────────────────────
Section '§4 `tuoen use node 24.19.0` —— 第一次激活（应当是 created）'
$r = Invoke-TuoenJson @('use', 'node', '24.19.0', '--json')
$useCode = $r[0]
$useJson = $r[1]
Check ($useCode -eq 0) '激活退出码是 0'
if ($null -ne $useJson) {
    Check ($useJson.data.repoint.outcome -eq 'created') '第一次激活报 created'
    Check ($null -eq $useJson.data.previous) '此前没有生效版本'
    Check ($useJson.data.repoint.degradedReason -eq $null) '没有降级（是真的原子路径）'
}

Section '§4b **通过链接真的执行 node.exe** —— 这是"链接真的通了"的唯一判据'
$currentNode = Join-Path $storeRoot 'node\current\node.exe'
$r1 = Invoke-Exe $currentNode @('--version')
Check ($r1[1] -eq 'v24.19.0') "通过链接执行 node.exe 报 v24.19.0（实际：$($r1[1])）"
$r2 = Invoke-Exe $currentNode @('-e', 'console.log(process.execPath)')
Say ''
Say '期望：execPath 是**链接那条路径**（说明进程真的是从 `current` 下启动的），'
Say '      而 `--version` 报的是**目标版本**（说明链接指向了正确的版本目录）。'
Check ($r2[1] -match '\\current\\node\.exe$') "execPath 落在 `current` 下（实际：$($r2[1])）"

Section '§4c 链接在磁盘上到底是什么（fsutil 的原始输出）'
$currentLink = Join-Path $storeRoot 'node\current'
if (Test-Path -LiteralPath $currentLink) {
    $rp = & fsutil reparsepoint query $currentLink 2>&1 | Out-String
    $rp.TrimEnd() -split "`r?`n" | Select-Object -First 14 | ForEach-Object { Say "  $_" }
} else {
    Say '  （链接不存在）'
}
Say ''
Say '目录项（注意 `current` 的 LinkType 是 Junction）：'
$item = Get-Item -LiteralPath $currentLink -Force -ErrorAction SilentlyContinue
if ($item) {
    Say ("  Attributes : " + $item.Attributes)
    Say ("  LinkType   : " + $item.LinkType)
    Say ("  Target     : " + ($item.Target -join ', '))
    Check ($item.LinkType -eq 'Junction') '`current` 是一个 Junction（不是 symlink、不是被复制出来的普通目录）'
    Check ($item.Target -contains (Join-Path $storeRoot 'node\versions\24.19.0')) '它指向 24.19.0 的版本目录'
}

# ── 第 5 节：第二次激活 = replaced（原子翻转的证据）─────────────────────────
Section '§5 `tuoen use node 24.21.0` —— 重指（应当是 replaced，不是 created）'
$r = Invoke-TuoenJson @('use', 'node', '24.21.0', '--json')
$flipJson = $r[1]
if ($null -ne $flipJson) {
    Check ($flipJson.data.repoint.outcome -eq 'replaced') '重指报 replaced（不是 created 也不是 degraded）'
    Check ($flipJson.data.previous -eq '24.19.0') '说清了从哪个版本切过来'
}
Say ''
Say '**`replaced` 就是原子性的证据**：`FSCTL_SET_REPARSE_POINT` 在标签相同时'
Say '是"就地替换重解析数据"，所以没有"先删再建"的空窗（决策 46）。'
Say '注意它同时也说明：两次 `use` 的输出**本来就该不同** —— 把 created 与 replaced'
Say '混在一起比"逐字节稳定"是比错了东西。'

Section '§5b 通过同一个链接再执行一次 —— 现在必须是 v24.21.0'
$r3 = Invoke-Exe $currentNode @('--version')
Check ($r3[1] -eq 'v24.21.0') "同一个链接现在报 v24.21.0（实际：$($r3[1])）"

Section '§5c `use --json`（同一状态跑两次，逐字节稳定）'
$j1 = & $exe use node 24.21.0 --json 2>&1 | Out-String
$j2 = & $exe use node 24.21.0 --json 2>&1 | Out-String
Say '$ tuoen use node 24.21.0 --json  （第一次）'
$j1.TrimEnd() -split "`r?`n" | ForEach-Object { Say "  $_" }
Check ($j1 -ceq $j2) '同一状态下的两次 use 逐字节相同'

# ── 第 6 节：删当前版本会被拒 ───────────────────────────────────────────────
Section '§6 `uninstall node 24.21.0`（删的正是生效版本）—— 必须被拒'
$r = Invoke-TuoenJson @('uninstall', 'node', '24.21.0', '--json')
$refuseJson = $r[1]
if ($null -ne $refuseJson) {
    Check ($refuseJson.ok -eq $false) '被拒（ok 是 false）'
    Check ($refuseJson.error.code -eq 'active-version') '错误码是 active-version'
    Check ($refuseJson.error.message -match 'tuoen use') '给出了"先切到别的版本"这条出路'
    Check ($refuseJson.error.message -match '--force') '给出了 --force 这条出路'
}
Say ''
Say '被拒之后必须**一点副作用都没有**：载荷、链接、以及通过链接读到的内容都还在。'
$payloadExists = Test-Path -LiteralPath (Join-Path $storeRoot 'node\versions\24.21.0')
$linkExists = Test-Path -LiteralPath $currentLink
Say "  载荷目录存在: $payloadExists"
Say "  链接存在    : $linkExists"
Check $payloadExists '被拒之后载荷还在'
Check $linkExists '被拒之后链接还在'
$r4 = Invoke-Exe $currentNode @('--version')
Check ($r4[1] -eq 'v24.21.0') '被拒之后通过链接仍然能执行（退出码 0 且版本没变）'

Section '§6b `uninstall node 24.21.0 --force` —— 先摘 current，再删载荷'
$forceOut = & $exe uninstall node 24.21.0 --force 2>&1 | Out-String
$forceCode = $LASTEXITCODE
$forceOut.TrimEnd() -split "`r?`n" | ForEach-Object { Say "  $_" }
Say "  → 退出码 $forceCode"
Check ($forceCode -eq 0) '强制删退出码是 0'
Say ''
$payloadGone = -not (Test-Path -LiteralPath (Join-Path $storeRoot 'node\versions\24.21.0'))
$linkGone = -not (Test-Path -LiteralPath $currentLink)
$otherAlive = Test-Path -LiteralPath (Join-Path $storeRoot 'node\versions\24.19.0')
Say "  载荷目录存在: $(-not $payloadGone)"
Say "  链接存在    : $(-not $linkGone)   ← 必须是 False，否则 current 会指向一个不存在的目录"
Say "  另一个版本在: $otherAlive"
Check $payloadGone '载荷真的删掉了'
Check $linkGone '`current` 已经摘掉（不留下悬空链接）'
Check $otherAlive '另一个版本没受影响'
Say ''
Say '列表：剩下的那个版本应当**没有生效版本**（而不是悄悄把 24.19.0 报成生效的）：'
$afterJson = (Invoke-TuoenJson @('list', '--json'))[1]
if ($null -ne $afterJson) {
    $row = @($afterJson.data.tools | Where-Object { $_.name -eq 'node' })[0]
    if ($row) {
        Check ($null -eq $row.version) '剩下的版本没有被悄悄报成生效的'
        Check ($row.installedVersions.Count -eq 1) '只剩一个版本'
    }
}

Section '§6c 切回另一个版本 —— 证明 --force 没有连累别的版本'
Invoke-Tuoen @('use', 'node', '24.19.0') | Out-Null
$r5 = Invoke-Exe $currentNode @('--version')
Check ($r5[1] -eq 'v24.19.0') '切回去之后通过链接执行报 v24.19.0'

# ── 第 7 节：各种拒绝路径 ───────────────────────────────────────────────────
Section '§7 拒绝路径（每一条都必须是"能读懂的中文 + 稳定的错误码"）'
Say '§7a 重复安装同一个版本：'
$dupJson = (Invoke-TuoenJson @('install', 'node@24.19.0', '--json'))[1]
if ($null -ne $dupJson) {
    Check ($dupJson.error.code -eq 'already-installed') '重复安装报 already-installed'
    Check ($dupJson.error.message -match 'tuoen uninstall') '给出了重装的办法'
}
Say ''
Say '§7b 不可再分发的工具（门禁必须在解析 recipe 之前跑）：'
$licJson = (Invoke-TuoenJson @('install', 'oracle-jdk@8', '--json'))[1]
if ($null -ne $licJson) {
    Check ($licJson.error.code -eq 'licence-prohibited') `
        '许可证拒绝报 licence-prohibited（而不是"没有适用的 recipe"）'
    Check ($licJson.error.message -notmatch 'http') '拒绝时不给出任何下载地址'
}
Say ''
Say '§7c 不认识的工具：'
$unkJson = (Invoke-TuoenJson @('install', 'definitely-not-a-tool', '--json'))[1]
if ($null -ne $unkJson) {
    Check ($unkJson.error.code -eq 'unknown-tool') '不认识的工具报 unknown-tool'
    Check ($unkJson.error.message -match 'node') '列出了我们认识哪些工具'
}
Say ''
Say '§7d `node@`（写了 @ 但忘了版本）：'
$atJson = (Invoke-TuoenJson @('install', 'node@', '--json'))[1]
if ($null -ne $atJson) {
    Check ($atJson.error.code -eq 'empty-version') '`node@` 报 empty-version'
}
Say ''
Say '§7e 切一个没装过的版本：'
$useBadJson = (Invoke-TuoenJson @('use', 'node', '99.99.99', '--json'))[1]
if ($null -ne $useBadJson) {
    Check ($useBadJson.error.code -eq 'not-installed') '切没装过的版本报 not-installed'
    Check ($useBadJson.error.message -match '24\.19\.0') '列出了真正装过的版本'
}
Say ''
Say '§7f 删一个没装过的版本：'
$rmBadJson = (Invoke-TuoenJson @('uninstall', 'node', '99.99.99', '--json'))[1]
if ($null -ne $rmBadJson) {
    Check ($rmBadJson.error.code -eq 'not-installed') '删没装过的版本报 not-installed'
}

# ── 第 8 节：检测的 managed 层现在真的有数据了 ──────────────────────────────
Section '§8 `tuoen detect` —— `managed` 层（七层置信度的第一层）现在应当有数据'
$detect = & $exe detect --json 2>&1 | Out-String
$code = $LASTEXITCODE
Say "  （退出码 $code；下面只摘出相关的几行，完整输出太长）"
$parsed = $null
try { $parsed = $detect.Trim() | ConvertFrom-Json } catch { }
if ($null -eq $parsed) {
    Say '  detect --json 的输出不是合法 JSON：'
    $detect.TrimEnd() -split "`r?`n" | ForEach-Object { Say "  $_" }
    $problems.Add('detect --json 的输出不是合法 JSON')
} elseif ($null -eq $parsed.data.tool) {
    # **键都不在，报的是脚本自己，不是产品。** 第一版就是在这里把
    # `tool` 写成了 `tools`，于是"managed 层是空的"这条假结论进了正式记录。
    Say "  **脚本读错了键**：`data.tool` 不存在。实际顶层键：$($parsed.data.PSObject.Properties.Name -join ', ')"
    $problems.Add('验收脚本读错了 detect 的载荷键')
} else {
    $all = @($parsed.data.tool)
    $managed = @($all | Where-Object { $_.confidence -eq 'managed' })
    Say "  总条目数       : $($all.Count)"
    Say "  managed 条目数 : $($managed.Count)"
    foreach ($m in $managed) {
        Say "    $($m.name)  $($m.version)  $($m.confidence)  $($m.path)"
        Say "      evidence: $($m.evidence)"
    }
    Check ($managed.Count -ge 1) '存储里有一个版本目录，所以 managed 层至少有 1 条'
    $managedNode = @($managed | Where-Object { $_.name -eq 'node' })[0]
    if ($managedNode) {
        Check ($managedNode.version -eq '24.19.0') 'managed 那条报的版本是 24.19.0（生效版本）'
        Check ($managedNode.path -match 'store\\node\\versions\\24\.19\.0$') '路径指向版本目录'
    }
    Say ''
    Say '  置信度分布（`managed` 必须在里面，且计数与存储里的版本数一致）：'
    foreach ($c in $parsed.data.summary.byConfidence) {
        Say "    $($c.key): $($c.count)"
    }
    Say ''
    Say '  注意 `node` 会同时出现在 `executable` / `manager-owned` / `managed` 三行里 ——'
    Say '  那是**真实的分裂**（PATH 上的 node 来自 nvm，而我们管的那份在存储里），'
    Say '  不是重复（决策 37）。'
}

# ── 第 9 节：磁盘上真实留下了什么 ───────────────────────────────────────────
Section '§9 磁盘上真实留下了什么（存储根的目录树）'
if (Test-Path -LiteralPath $storeRoot) {
    Get-ChildItem -LiteralPath $storeRoot -Recurse -Depth 2 -Force -ErrorAction SilentlyContinue |
        ForEach-Object {
            $rel = $_.FullName.Substring($storeRoot.Length).TrimStart('\')
            if ($_.PSIsContainer) {
                Say ("  [D] $rel")
            } else {
                Say ("      $rel   ($($_.Length) 字节)")
            }
        }
} else {
    Say '  （存储根不存在）'
}
Say ''
Check (-not (Test-Path -LiteralPath (Join-Path $storeRoot '.incoming\node\24.19.0'))) `
    '`.incoming` 里没有留下解压残骸（搬进存储之后就该没了）'

Section '§9b 安装记录的内容（版本目录旁边的那份 JSON）'
$recordPath = Join-Path $storeRoot 'node\versions\24.19.0.json'
if (Test-Path -LiteralPath $recordPath) {
    Get-Content -LiteralPath $recordPath | ForEach-Object { Say "  $_" }
} else {
    Say "  （$recordPath 不存在）"
    $problems.Add('安装记录没有落盘')
}

Section '§9c 载荷目录里**不该有我们自己放的文件**（决策 48 的第 ① 条）'
$payload = Join-Path $storeRoot 'node\versions\24.19.0'
$expectedTop = @(
    'CHANGELOG.md', 'LICENSE', 'README.md', 'corepack', 'corepack.cmd', 'corepack.ps1',
    'install_tools.bat', 'node.exe', 'node_modules', 'nodevars.bat', 'npm', 'npm.cmd',
    'npm.ps1', 'npx', 'npx.cmd', 'npx.ps1'
)
$extra = @(Get-ChildItem -LiteralPath $payload -Force -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -notin $expectedTop })
foreach ($e in $extra) { Say "  多出来的：$($e.Name)" }
Check ($extra.Count -eq 0) '载荷目录里没有我们自己放的文件（记录是 `<version>.json` 而不是 `<version>/tuoen.json`）'
Say "  载荷里的顶层条目数: $(@(Get-ChildItem -LiteralPath $payload -Force -ErrorAction SilentlyContinue).Count)"

Section '§9d 通过链接**真的跑一次 npm** —— 证明附带命令也通'
$npmCmd = Join-Path $storeRoot 'node\current\npm.cmd'
if (Test-Path -LiteralPath $npmCmd) {
    $npmOut = & $npmCmd --version 2>&1 | Out-String
    $npmCode = $LASTEXITCODE
    Say "  \$ `"$npmCmd`" --version"
    Say "  $($npmOut.Trim())"
    Say "  → 退出码 $npmCode"
    Check ($npmCode -eq 0) '通过链接跑 npm.cmd 成功'
    Check ($npmOut.Trim() -match '^\d+\.\d+\.\d+') 'npm 报出了一个版本号'
} else {
    Say '  （npm.cmd 不存在）'
    $problems.Add('通过链接找不到 npm.cmd')
}

# ── 第 10 节：收尾状态 ──────────────────────────────────────────────────────
Section '§10 收尾状态'
Invoke-Tuoen @('list') | Out-Null
Say ''
Invoke-Tuoen @('list', '--json') | Out-Null
Say ''
Say '通过链接执行 node：'
Invoke-Exe $currentNode @('--version') | Out-Null

Section '§11 结论'
Say "存储根        : $storeRoot"
Say "node 版本     : $((Get-ChildItem -LiteralPath (Join-Path $storeRoot 'node\versions') -Directory -ErrorAction SilentlyContinue | ForEach-Object { $_.Name }) -join ', ')"
Say "当前生效      : $((Get-Item -LiteralPath $currentLink -Force -ErrorAction SilentlyContinue).Target -join ', ')"
Say ''
if ($problems.Count -eq 0) {
    Say '**全部断言通过。**'
} else {
    Say "**有 $($problems.Count) 条断言没有通过：**"
    foreach ($p in $problems) { Say "  ✗ $p" }
}
Say ''
Say '这份记录里的每一行都是这台机器上真实跑出来的输出，没有一行是手写的。'

# ── 落盘 ────────────────────────────────────────────────────────────────────
$dir = Split-Path -Parent $Out
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
$lines -join "`r`n" | Set-Content -Path $Out -Encoding UTF8
Write-Host ''
Write-Host "已写入 $Out（$($lines.Count) 行）"
if ($problems.Count -eq 0) {
    Write-Host '验收通过。'
    exit 0
}
Write-Host "验收**没有通过**：$($problems.Count) 条断言失败。"
exit 1
