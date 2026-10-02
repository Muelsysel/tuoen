#requires -Version 7.0
<#
票据 #8 的真机验收脚本：PATH 读写（禁 setx）+ 8191 预算告警 + 遮蔽与用户名依赖检测。

它在**这台真机**上做的事：
  1. 预检：源码里 grep 不到 `setx`；`crates/**` 里没有黑名单的注册表写点。
  2. 用 `GetValue(..., DoNotExpandEnvironmentNames)` 独立快照两个作用域的 `Path`
     （类型 + 原始字节），**不经过我们的代码**。
  3. 跑 `cargo run -p tuoen-platform --example path_probe`，解析它的 `SUMMARY` 契约行。
  4. 再快照一次：两份必须**逐字节相同** —— 这是"绝不碰真实 Path"的独立证据。
  5. 确认真机上没有留下 `TUOEN_PATH_PROBE` 残留。
  6. 跑 CLI 的 `tuoen path show --json` / `--dry-run`，确认只读路径与 dry-run 不写盘。

每一条期望都走 `Check`，**退出码跟随失败数**（设计决策 50：一条不会失败的验收脚本
不是验收脚本）。`-SelfTest` 会故意加一条必然失败的期望，用来证明"退出码真的会变"。

跑法：`pwsh -File scripts/acceptance-L0-08.ps1`
自检：`pwsh -File scripts/acceptance-L0-08.ps1 -SelfTest`（必须 exit 1）
#>
[CmdletBinding()]
param(
    [switch]$SelfTest,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = Split-Path -Parent $PSScriptRoot
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"

$script:passed = 0
$script:failed = @()

function Check {
    param([string]$What, [bool]$Ok, [string]$Detail = '')
    if ($Ok) {
        $script:passed++
        Write-Host "  [通过] $What"
    }
    else {
        $script:failed += $What
        Write-Host "  [失败] $What $(if ($Detail) { "—— $Detail" })" -ForegroundColor Red
    }
}

$machineKey = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
$userKey = 'HKCU:\Environment'
$probeName = 'TUOEN_PATH_PROBE'

function Get-PathSnapshot {
    # **不展开**（DoNotExpandEnvironmentNames）：这是 `setx` 事故的关键判据 ——
    # 值里字面含有 `%VAR%` 还是已经被冻成展开后的字面量，只有不展开读才看得出来。
    $rows = foreach ($key in @($machineKey, $userKey)) {
        $item = Get-Item $key
        $kind = $item.GetValueKind('Path')
        $value = $item.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
        "$key|$kind|$value"
    }
    return ($rows -join "`n")
}

function Get-ProbeValue {
    param([string]$Name)
    $item = Get-Item $userKey
    if ($item.GetValueNames() -contains $Name) {
        return ($item.GetValue($Name, '', 'DoNotExpandEnvironmentNames'),
            $item.GetValueKind($Name))
    }
    return $null
}

Write-Host '═══ 1. 预检 ═══'
$sources = Get-ChildItem -Path (Join-Path $repo 'crates') -Recurse -Include *.rs -File |
    Where-Object { $_.FullName -notmatch '\\target\\' }
Check '源码树里扫得到 .rs 文件' (@($sources).Count -gt 50) "只找到 $(@($sources).Count) 个"

# 「绝不调用 setx」是本票的硬约束之一，用 grep 钉住它。**`setx` 一次都没被跑过**，
# 因为跑它就是在改这台机器 —— 能被 grep 钉住的东西不需要真跑一遍去证明。
#
# 判据必须是**带引号的字面量** `"setx`：源码里到处都在用反引号写 `` `setx` `` 来
# **警告**不许调用它（14 处全是文档）。一个会被当程序名传出去的东西必然带引号。
$setxMentions = @(Select-String -Path $sources.FullName -Pattern 'setx' -SimpleMatch)
$setxLiterals = @(Select-String -Path $sources.FullName -Pattern '"setx' -SimpleMatch)
Write-Host "  （讯息：源码里提到 setx $($setxMentions.Count) 次，全部是文档/警告）"
Check '源码里没有带引号的 `setx` 字面量（那才会被当程序名跑）' ($setxLiterals.Count -eq 0) "$($setxLiterals.Count) 处：$($setxLiterals | ForEach-Object { "$($_.Filename):$($_.LineNumber)" } | Select-Object -First 5)"
Check '“绝不调用 setx”这件事在源码里有明确的文字约束' ($setxMentions.Count -ge 5) "只提到 $($setxMentions.Count) 次"

# 注册表写点只允许出现在 sys.rs 的一节里（那节是唯一服务于"把用户级 PATH 整条写回"的）。
$writeHits = @(Select-String -Path $sources.FullName -Pattern 'RegSetValueExW|RegCreateKeyExW|RegDeleteValueW' -AllMatches)
$outsideSys = @($writeHits | Where-Object { $_.Filename -ne 'sys.rs' })
Check '注册表写调用只出现在 platform/src/sys.rs 里' ($outsideSys.Count -eq 0) "越界：$($outsideSys | ForEach-Object { $_.Filename } | Select-Object -Unique)"

Write-Host '═══ 2. 写之前的快照（不经过我们的代码） ═══'
$before = Get-PathSnapshot
$machineBefore = ($before -split "`n")[0]
$userBefore = ($before -split "`n")[1]
Write-Host "  机器级：$($machineBefore.Substring(0, 60))…"
Write-Host "  用户级：$($userBefore.Substring(0, 60))…"
Check "跑之前没有 $probeName 残留" ($null -eq (Get-ProbeValue $probeName))

Write-Host '═══ 3. 真机探针 ═══'
if (-not $SkipBuild) {
    & cargo build -q -p tuoen-platform --example path_probe 2>&1 | Write-Host
    Check '探针编得出来' ($LASTEXITCODE -eq 0) "cargo build 退出码 $LASTEXITCODE"
}
$probeOutput = & cargo run -q -p tuoen-platform --example path_probe -- --shadow-check 2>&1
$probeExit = $LASTEXITCODE
$probeOutput | ForEach-Object { Write-Host "  | $_" }

Check '探针自己退出 0' ($probeExit -eq 0) "退出码 $probeExit"
$summaryLine = $probeOutput | Where-Object { $_ -like 'SUMMARY *' } | Select-Object -First 1
Check '探针打出了 SUMMARY 契约行' ($null -ne $summaryLine)

$summary = @{}
if ($summaryLine) {
    foreach ($pair in ($summaryLine -replace '^SUMMARY ', '' -split '\s+')) {
        $kv = $pair -split '=', 2
        if ($kv.Count -eq 2) { $summary[$kv[0]] = $kv[1] }
    }
}
foreach ($key in @('checks_passed', 'checks_failed', 'path_untouched', 'probe_residue',
        'budget_level', 'effective_chars', 'registry_chars', 'duplicates', 'dangling',
        'username_deps', 'username_deps_machine', 'process_only', 'shadowed',
        'broadcast_replies', 'shadow_experiment', 'shadow_found')) {
    Check "SUMMARY 里有 $key" ($summary.ContainsKey($key))
}
if ($summary.ContainsKey('checks_failed')) {
    Check '探针里没有失败项' ($summary['checks_failed'] -eq '0') "checks_failed=$($summary['checks_failed'])"
}
if ($summary.ContainsKey('checks_passed')) {
    Check '探针至少跑了 16 项自检' ([int]$summary['checks_passed'] -ge 16) "checks_passed=$($summary['checks_passed'])"
}
if ($summary.ContainsKey('path_untouched')) {
    Check '探针自证：真实 Path 一个字节都没变' ($summary['path_untouched'] -eq '1')
}
if ($summary.ContainsKey('probe_residue')) {
    Check '探针自证：没有留下探测值残留' ($summary['probe_residue'] -eq '0')
}
if ($summary.ContainsKey('budget_level')) {
    Check '预算档位是四个合法 slug 之一' ($summary['budget_level'] -in @('ok', 'warning', 'critical', 'exceeded')) $summary['budget_level']
}
if ($summary.ContainsKey('username_deps_machine')) {
    Check '机器级的用户名依赖被单独标出来了' ([int]$summary['username_deps_machine'] -ge 1) "username_deps_machine=$($summary['username_deps_machine'])"
}
if ($summary.ContainsKey('shadow_experiment')) {
    Check '真机遮蔽实验真的跑了' ($summary['shadow_experiment'] -eq '1') "shadow_experiment=$($summary['shadow_experiment'])"
}
if ($summary.ContainsKey('shadow_found')) {
    # 票据 ⑥ 点名了本机的两个例子（Oracle `java8path` 与 `C:\nvm4w\nodejs`）。
    # 至少要在真机上证明"遮蔽报得出来"，一条都没有就说明实验没制造出遮蔽。
    Check '真机上报出了遮蔽（≥1 条）' ([int]$summary['shadow_found'] -ge 1) "shadow_found=$($summary['shadow_found'])"
}

Write-Host '═══ 4. 写之后的快照：必须与之前逐字节相同 ═══'
$after = Get-PathSnapshot
Check '两个作用域的 Path（类型 + 原始字节）前后完全相同' ($before -eq $after) 'PATH 被改了 —— 这是最严重的一种失败'
Check "跑完没有 $probeName 残留" ($null -eq (Get-ProbeValue $probeName)) '注册表里还能读到探测值'

Write-Host '═══ 5. CLI 的只读路径与 dry-run ═══'
$cli = Join-Path $repo 'target\debug\tuoen.exe'
# **无条件重建**。这一条是拿一次真实的假阳性换来的：第一版只在二进制**不存在**时才构建，
# 于是它拿着一个还没有 `path` 子命令的旧二进制去跑，clap 一律答 exit 2 —— 三条"失败"
# 全是假的，而假的失败和真的失败长得一模一样（设计决策 50）。
& cargo build -q -p tuoen-cli 2>&1 | Write-Host
Check 'CLI 重建成功' ($LASTEXITCODE -eq 0) "cargo build 退出码 $LASTEXITCODE"
$cliTime = if (Test-Path $cli) { (Get-Item $cli).LastWriteTime } else { $null }
Write-Host "  二进制：$cli"
Write-Host "  时间戳：$cliTime"
Check 'CLI 二进制存在' (Test-Path $cli) $cli

if (Test-Path $cli) {
    # 先确认这个二进制**认识** `path` 子命令。不认识就说明我们测错了对象，
    # 而不是"功能坏了"—— 这两种结论必须分开报。
    $help = (& $cli path --help 2>&1) -join "`n"
    $knowsPath = ($LASTEXITCODE -eq 0) -and ($help -match 'show') -and ($help -match 'add') -and ($help -match 'remove')
    Check '这个二进制认识 `path` 子命令（show/add/remove 都在帮助里）' $knowsPath "退出码 $LASTEXITCODE"

    $showBefore = (& $cli path show --json 2>&1) -join "`n"
    Check 'show --json 退出 0' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE"
    $showAgain = (& $cli path show --json 2>&1) -join "`n"
    Check 'show --json 两次逐字节相同' ($showBefore -eq $showAgain) '两次输出不同 —— 说明 JSON 里有不稳定顺序'
    try {
        $json = $showBefore | ConvertFrom-Json
        Check 'show --json 是合法 JSON' ($null -ne $json)
        Check 'show --json 的 ok 是 true' ($json.ok -eq $true) "ok=$($json.ok)"
        Check 'budget.level 是四个 slug 之一' ($json.data.budget.level -in @('ok', 'warning', 'critical', 'exceeded')) $json.data.budget.level
        Check 'show --json 报了遮蔽条数' ($null -ne $json.data.shadowedShimCount) ' 缺 shadowedShimCount'
    }
    catch {
        Check 'show --json 是合法 JSON' $false $_.Exception.Message
    }

    $dry = (& $cli path add 'C:\tuoen-acceptance-dry-run' --dry-run --json 2>&1) -join "`n"
    Check 'add --dry-run 退出 0' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE"
    $showAfterDryRun = (& $cli path show --json 2>&1) -join "`n"
    Check 'dry-run 之后 PATH 一个字节都没变' ($showAfterDryRun -eq $showBefore) 'dry-run 竟然写了盘'

    $semicolon = (& $cli path add 'C:\x;y' --dry-run --json 2>&1) -join "`n"
    Check '参数含 `;` 时非零退出' ($LASTEXITCODE -ne 0) "退出码 $LASTEXITCODE"
}

Write-Host '═══ 5.5 真 shim → 遮蔽报告（端到端，需要真实 store 里装过工具） ═══'
# 这一段是**子代理当时明确说它做不了**的那件事：`cargo test` 不许在真实家目录造 shim，
# 所以"遮蔽"在测试里只有单测覆盖。这里补上**活的**一次 —— 但它会往真实的
# `%LOCALAPPDATA%\tuoen\shims` 写文件，所以：
#   ①先快照那个目录；②只生成、只删我们自己生成的那几条；③跑完必须**核实还原**。
# 真实 store 里没有已安装的工具时**跳过**（报 skip，不报通过）—— 那说明这台机器
# 还没到能跑这一段的阶段，不是功能坏了。
if (Test-Path $cli) {
    $shimDir = Join-Path $env:LOCALAPPDATA 'tuoen\shims'
    $shimListingBefore = @(if (Test-Path $shimDir) { Get-ChildItem $shimDir -File | Select-Object -ExpandProperty Name } else { @() })
    Write-Host "  shim 目录：$shimDir（跑之前 $($shimListingBefore.Count) 个文件）"

    # 找一个真的装过的工具：`shim add <tool> --dry-run --json` 退出 0 就说明它可解析。
    $installedTool = $null
    foreach ($candidate in @('node')) {
        & $cli shim add $candidate --dry-run --json *> $null
        if ($LASTEXITCODE -eq 0) { $installedTool = $candidate; break }
    }

    if ($null -eq $installedTool) {
        Write-Host '  [跳过] 真实 store 里没有已安装的工具 —— 这一段无法在本机成立' -ForegroundColor Yellow
    }
    else {
        Write-Host "  用真实工具：$installedTool"
        & $cli shim add $installedTool *> $null
        Check '真机生成 shim 退出 0' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE"

        $plannedNames = @()
        $planJson = (& $cli shim add $installedTool --dry-run --json 2>&1) -join "`n"
        try { $plannedNames = @(($planJson | ConvertFrom-Json).data.commands | ForEach-Object { $_.command }) } catch { }
        $made = @(if (Test-Path $shimDir) { Get-ChildItem $shimDir -File | Select-Object -ExpandProperty Name } else { @() })
        Check '生成之后 shim 文件真的出现了' ($made.Count -gt 0) "目录里只有 $($made.Count) 个文件"
        Check '生成的文件数等于计划里的命令数' ($made.Count -eq $plannedNames.Count) "计划 $($plannedNames.Count) 条、盘上 $($made.Count) 条"

        # ① 分母：`show --json` 必须报出我们发布了哪几条命令。
        $withShims = ((& $cli path show --json 2>&1) -join "`n") | ConvertFrom-Json
        $commands = @($withShims.data.shimCommands)
        Check 'show --json 报出了 shimCommands（分母）' ($commands.Count -eq $made.Count) "shimCommands=$($commands.Count)、盘上 $($made.Count)"
        Check 'shimCommands 里有 node' ($commands -contains 'node') ($commands -join ',')

        # ② 遮蔽报告：`path add <shims> --dry-run` 的人类输出必须点名叫出抢名字的目录，
        #    并且条数与 `show --json` 的计数**一致**（两处判据必须同一份）。
        $human = (& $cli path add $shimDir --dry-run 2>&1) -join "`n"
        Check 'path add <shims> --dry-run 退出 0' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE"
        Check '人类输出里有遮蔽那一段' ($human -match '遮蔽') ' 没有"遮蔽"这一段'

        # ③ 删掉**这个工具**的一条命令之后，必须提示还剩哪几条兄弟。
        #    这是实测抓出来的不对称：`shim add node` 生成 4 条，而 `shim remove node`
        #    只删 `node.exe`（remove 收的是**命令名**）。
        $first = $plannedNames | Select-Object -First 1
        if ($first) {
            $removed = (& $cli shim remove $first 2>&1) -join "`n"
            Check 'shim remove <一条命令> 退出 0' ($LASTEXITCODE -eq 0) "退出码 $LASTEXITCODE"
            if ($plannedNames.Count -gt 1) {
                Check '删一条之后提示了剩下的兄弟命令' ($removed -match '还有') $removed
                Check '提示里给出了可抄的完整命令' ($removed -match 'tuoen shim remove ') $removed
            }
            else {
                Check '只发布过一条命令时不该提兄弟' ($removed -notmatch '还有') $removed
            }
        }

        # ④ 收尾：只删我们自己生成的那几条，随后必须核实还原。
        $extra = @(if (Test-Path $shimDir) { Get-ChildItem $shimDir -File | Select-Object -ExpandProperty Name } else { @() })
        if ($extra.Count -gt 0) {
            & $cli shim remove @($extra | ForEach-Object { $_ -replace '\.exe$', '' }) *> $null
        }
        $shimListingAfter = @(if (Test-Path $shimDir) { Get-ChildItem $shimDir -File | Select-Object -ExpandProperty Name } else { @() })
        Check '这一段跑完之后 shim 目录回到了跑之前的样子' (($shimListingAfter -join ',') -eq ($shimListingBefore -join ',')) "之前[$($shimListingBefore -join ',')] 之后[$($shimListingAfter -join ',')]"
    }
}

Write-Host '═══ 6. 收尾 ═══'
$finalSnapshot = Get-PathSnapshot
Check '整场验收之后 Path 依然与最开始相同' ($before -eq $finalSnapshot) 'PATH 被整个验收过程改动了'

if ($SelfTest) {
    Write-Host '  [自检] 故意加一条必然失败的期望 —— 退出码必须变成 1'
    Check '自检：这一条必须失败' $false '故意造的失败'
}

Write-Host ''
Write-Host "通过 $($script:passed) 项，失败 $($script:failed.Count) 项"
if ($script:failed.Count -gt 0) {
    foreach ($item in $script:failed) { Write-Host "  失败：$item" -ForegroundColor Red }
    if ($SelfTest) { Write-Host 'SELFTEST=expect-failure（这就是自检要的结果）' }
    exit 1
}
if ($SelfTest) {
    Write-Host 'SELFTEST=unexpected-pass —— 自检失败了：退出码没有跟随失败数' -ForegroundColor Red
    exit 2
}
Write-Host 'SELFTEST=not-requested'
exit 0
