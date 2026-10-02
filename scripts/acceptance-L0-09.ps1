#requires -Version 7.0
<#
票据 #9 的真机验收脚本：在作者本机上**真的装一个工具链并让它生效**，然后留下证据。

这一票不写新功能，它写的是**证据** —— 证明 L0 不是"框架搭好了"。

它在**这台真机**上做的事（每一件都可重复、跑完自己收场）：

  1. 预检：源码里 grep 不到带引号的 `setx`；构建真正会发出去的 release 二进制。
  2. 快照两个作用域的 `Path`（raw + 类型 + SHA256）与 nvm4w 的状态（ADR-0004 的不变量）。
  3. `tuoen install` **真下载真解压**（计时），并与 `--dry-run` 的计划**逐字节对比**。
  4. 生成真 shim、真执行它、再 `tuoen use` 翻版本 —— 断言**同一批 shim 文件**报出新版本
     （决策 11：shim 指向 `current` 这个链接，切版本不碰 shim）。
  5. 模拟"一个全新终端"（`scripts/fresh-terminal.ps1` 从注册表重建 PATH）：
     断言 `where node` 落到了哪里，以及 tuoen 是否**如实报告**了遮蔽。
  6. 再把遮蔽者从模拟里去掉，断言这一次真的是我们的 shim 赢。
  7. 清理：删 shim、删自己装的版本、复核两个作用域的 `Path` **逐字节没变**、
     复核 nvm4w 一个字节没动、复核没有残留目录。

**它不做真实 `PATH` 写入。** 原因是本票跑出来的一条真问题（见脚本末尾 §6 的观察）：
`tuoen path add <shim 目录>` 能做，但 `tuoen path remove <shim 目录>` 会被拒绝，
所以验收脚本自己也没有干净的退路。真写一次的端到端证据在
`docs/acceptance/L0-09-real-machine.md` §5，那一次是我手工跑完并手工复原的。

每一条期望都走 `Check`，**退出码跟随失败数**（决策 50）。
`-SelfTest` 故意加一条必然失败的期望，用来证明"退出码真的会变"。

跑法：`pwsh -File scripts/acceptance-L0-09.ps1`
自检：`pwsh -File scripts/acceptance-L0-09.ps1 -SelfTest`（必须 exit 1）
#>
[CmdletBinding()]
param(
    [switch]$SelfTest,
    [switch]$SkipBuild,
    # 装哪个版本。默认取 `install node` 演练报出的那个（目录里最新的可安装版本）。
    [string]$Version = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repo = Split-Path -Parent $PSScriptRoot
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"

$script:passed = 0
$script:failed = @()
$script:skipped = @()
$script:observations = @()

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

# **跳过不是通过。** 一条"因为环境不满足所以没测"的期望必须单独计数，
# 否则报告会把"没测"读成"测过了"。
function Skip {
    param([string]$What, [string]$Why)
    $script:skipped += $What
    Write-Host "  [跳过] $What —— $Why" -ForegroundColor Yellow
}

# 观察：既不是通过也不是失败，是"这台机器上确实是这个样子"的记录。
function Note {
    param([string]$Text)
    $script:observations += $Text
    Write-Host "  · $Text" -ForegroundColor DarkGray
}

$machineKey = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
$userKey = 'HKCU:\Environment'
$shimDir = Join-Path $env:LOCALAPPDATA 'tuoen\shims'
$storeRoot = Join-Path $env:LOCALAPPDATA 'tuoen\store'

function Get-PathSnapshot {
    # **不展开**（DoNotExpandEnvironmentNames）：这是能看出"值里字面含有 %VAR%"
    # 还是"已经被冻成展开后的字面量"的唯一读法。
    $rows = foreach ($key in @($machineKey, $userKey)) {
        $item = Get-Item $key
        $kind = $item.GetValueKind('Path')
        $value = $item.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
        if ($null -eq $value) { $value = '' }
        $sha = (Get-FileHash -InputStream ([IO.MemoryStream]::new([Text.Encoding]::Unicode.GetBytes($value))) -Algorithm SHA256).Hash
        "$key|$kind|$($value.Length)|$sha"
    }
    return ($rows -join "`n")
}

function Get-RawPath {
    param([string]$Key)
    $value = (Get-Item $Key).GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    if ($null -eq $value) { return '' }
    return $value
}

function Get-NvmState {
    # ADR-0004：第三方版本管理器是**只采纳、不管理**。这个函数存在的意义是
    # "跑完再比一次"，而不是"顺便检查一下"。
    $link = Get-Item 'C:\nvm4w\nodejs' -Force -ErrorAction SilentlyContinue
    $target = if ($link) { "$($link.Target)" } else { '<不存在>' }
    $item = Get-Item $userKey
    $vars = foreach ($name in @('NVM_HOME', 'NVM_SYMLINK')) {
        if ($item.GetValueNames() -contains $name) {
            "$name=$($item.GetValue($name, '', 'DoNotExpandEnvironmentNames'))"
        }
        else { "$name=<未设置>" }
    }
    return ("$target|" + ($vars -join '|'))
}

function Invoke-Tool {
    # 返回 @{ Exit; Out }。**stderr 也收进来**：我们的错误信息全在 stderr 上。
    param([string]$Exe, [string[]]$Argv)
    $out = (& $Exe @Argv 2>&1) -join "`n"
    return @{ Exit = $LASTEXITCODE; Out = $out }
}

Write-Host '═══ 0. 二进制 ═══'
if (-not $SkipBuild) {
    & cargo build --release -p tuoen-cli -p tuoen-shim 2>&1 | Select-Object -Last 2 | Write-Host
    Check 'release 构建成功' ($LASTEXITCODE -eq 0) "cargo 退出码 $LASTEXITCODE"
}
$cli = Join-Path $repo 'target\release\tuoen.exe'
$shimTemplate = Join-Path $repo 'target\release\tuoen-shim.exe'
Check 'release 的 tuoen.exe 在' (Test-Path $cli)
Check 'release 的 tuoen-shim.exe 在（shim 模板与 tuoen.exe 同目录）' (Test-Path $shimTemplate)
if (-not (Test-Path $cli)) { Write-Host '没有二进制，停。' -ForegroundColor Red; exit 2 }

$help = (Invoke-Tool $cli @('--help')).Out
$knows = @(@('install', 'use', 'uninstall', 'list', 'shim', 'path') |
    Where-Object { $help -notmatch [regex]::Escape($_) })
Check '这个二进制认识这一票要用的全部子命令' ($knows.Count -eq 0) "缺：$($knows -join ', ')"

Write-Host ''
Write-Host '═══ 1. 预检与快照 ═══'
$sources = Get-ChildItem -Path (Join-Path $repo 'crates') -Recurse -Include *.rs -File |
    Where-Object { $_.FullName -notmatch '\\target\\' }
$setxHits = @($sources | Select-String -Pattern '"setx' -SimpleMatch)
# 判据必须是**带引号**的形态：源码里到处用反引号写 `setx` 来警告不许调用它。
Check '源码里没有调用 setx（带引号的字面量命中 0 处）' ($setxHits.Count -eq 0) "命中 $($setxHits.Count)"

$snapshotBefore = Get-PathSnapshot
$nvmBefore = Get-NvmState
$userRawBefore = Get-RawPath $userKey
$machineRawBefore = Get-RawPath $machineKey
Note "机器级 Path $($machineRawBefore.Length) 字符；用户级 Path $($userRawBefore.Length) 字符"
Note "nvm4w：$nvmBefore"
$storeVersionsBefore = @(Get-ChildItem (Join-Path $storeRoot 'node\versions') -Directory -ErrorAction SilentlyContinue |
        Select-Object -ExpandProperty Name)
Note "存储里本来就有：$(if ($storeVersionsBefore.Count -eq 0) { '（空）' } else { $storeVersionsBefore -join ', ' })"
$shimsExistedBefore = Test-Path $shimDir
$shimsBefore = @(Get-ChildItem $shimDir -File -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name)
if ($shimsExistedBefore) {
    Note "shim 目录本来就存在，里面有 $($shimsBefore.Count) 个文件（跑完要回到这个样子）"
}

Write-Host ''
Write-Host '═══ 2. 真装一个版本 ═══'
$planDry = Invoke-Tool $cli @('install', 'node', '--dry-run', '--json')
Check 'install --dry-run 退出 0' ($planDry.Exit -eq 0) "退出码 $($planDry.Exit)"
$planDryJson = $planDry.Out | ConvertFrom-Json
$target = if ($Version) { $Version } else { $planDryJson.data.plan.version }
Note "目标版本：$target"
$alreadyThere = $storeVersionsBefore -contains $target

$installSeconds = ''
$planRealJson = $null
if ($alreadyThere) {
    Skip "真下载安装 $target" "存储里已经有这个版本了 —— 跳过安装这一段，但下面的 shim/切版本照跑"
    $planRealJson = $planDryJson
}
else {
    Note "上游地址：$($planDryJson.data.plan.url)"
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $install = Invoke-Tool $cli @('install', "node@$target", '--json')
    $sw.Stop()
    $installSeconds = [math]::Round($sw.Elapsed.TotalSeconds, 2)
    Check "真装 node@$target 退出 0" ($install.Exit -eq 0) "退出码 $($install.Exit)：$($install.Out)"
    if ($install.Exit -eq 0) {
        $planRealJson = $install.Out | ConvertFrom-Json
        Note "安装耗时 $installSeconds 秒（含下载 + 校验 + 解压 + 落盘）"
        $a = $planDryJson.data.plan | ConvertTo-Json -Depth 10 -Compress
        $b = $planRealJson.data.plan | ConvertTo-Json -Depth 10 -Compress
        Check '演练的计划与真跑的计划**逐字节相同**' ($a -ceq $b) '两份 plan 不一致'
        $attempts = @($planRealJson.data.result.attempts)
        if ($planRealJson.data.result.fromCache) {
            # 命中下载缓存是**正常路径**（票据 #4 的验收项），这时根本不会去取源，
            # 所以 attempts 是空的。把它当失败就是把缓存当成了错误。
            Note '命中下载缓存：这一次没有产生任何网络流量（所以取源尝试是 0 次）'
        }
        else {
            Note "取源尝试 $($attempts.Count) 次："
            foreach ($attempt in $attempts) {
                $mark = if ($attempt.ok) { '成功' } else { '失败' }
                Note ("  {0} {1} {2}ms{3}" -f $mark, $attempt.source, $attempt.millis,
                    $(if ($attempt.ok) { '' } else { " —— $($attempt.message)" }))
            }
            Check '至少有一个源成功' (@($attempts | Where-Object { $_.ok }).Count -ge 1)
        }
    }
}

$listed = (Invoke-Tool $cli @('list', '--json')).Out | ConvertFrom-Json
$installedNow = @($listed.data.tools[0].installedVersions)
Check "list 里看得到 $target" ($installedNow -contains $target) "实际：$($installedNow -join ', ')"

Write-Host ''
Write-Host '═══ 3. 真 shim、真执行、真切版本 ═══'
# **只收拾自己弄乱的东西。** 这个 shim 目录如果在跑之前就已经有文件，
# 那是别人的（或者上一次没跑完的），本脚本不覆盖也不删。
$shimsCreatedByUs = $false
if ($shimsBefore.Count -gt 0) {
    Skip '生成 shim' '这个 shim 目录本来就有文件；验收脚本不覆盖别人的东西（先在干净目录上跑）'
}
else {
    $shimPlan = Invoke-Tool $cli @('shim', 'add', 'node', '--dry-run', '--json')
    Check 'shim add --dry-run 退出 0' ($shimPlan.Exit -eq 0) "退出码 $($shimPlan.Exit)"
    $planned = @(($shimPlan.Out | ConvertFrom-Json).data.commands | ForEach-Object { $_.command })
    $shimAdd = Invoke-Tool $cli @('shim', 'add', 'node')
    Check 'shim add 真生成退出 0' ($shimAdd.Exit -eq 0) "退出码 $($shimAdd.Exit)：$($shimAdd.Out)"
    $onDisk = @(Get-ChildItem $shimDir -Filter *.exe -File -ErrorAction SilentlyContinue)
    Check '盘上的 shim 条数等于计划里的命令数' ($onDisk.Count -eq $planned.Count) "计划 $($planned.Count) 条，盘上 $($onDisk.Count) 条"
    $shimsCreatedByUs = $true

    $stateAfterAdd = (Invoke-Tool $cli @('list', '--json')).Out | ConvertFrom-Json
    $activeNow = $stateAfterAdd.data.tools[0].version
    Note "当前生效版本：$activeNow"

    $shimNode = Join-Path $shimDir 'node.exe'
    $viaShim = ((& $shimNode -v) 2>&1) -join ''
    Check "同一批 shim 里直接跑 node.exe 报 v$activeNow" ($viaShim.Trim() -eq "v$activeNow") "实际：$viaShim"

    $fingerprintBefore = @(Get-ChildItem $shimDir -Filter *.exe -File |
            ForEach-Object { "$($_.Name):$($_.Length):$($_.LastWriteTimeUtc.Ticks)" }) -join '|'

    # 挑一个**另一个**已装的版本切过去，证明"切版本不碰 shim"。
    $other = @($installedNow | Where-Object { $_ -ne $activeNow })
    if ($other.Count -eq 0) {
        Skip '切到另一个版本再看一次' "存储里只有一个版本"
    }
    else {
        $switchTo = $other[0]
        $useResult = Invoke-Tool $cli @('use', 'node', $switchTo)
        Check "use node $switchTo 退出 0" ($useResult.Exit -eq 0) "退出码 $($useResult.Exit)"
        $viaShim2 = ((& $shimNode -v) 2>&1) -join ''
        Check "**同一批 shim**（一个字节没重新生成）现在报 v$switchTo" ($viaShim2.Trim() -eq "v$switchTo") "实际：$viaShim2"
        $viaNpm = ((& (Join-Path $shimDir 'npm.exe') --version) 2>&1) -join ''
        Note "同一个 npm.exe 报的版本：$($viaNpm.Trim())"
        $fingerprintAfter = @(Get-ChildItem $shimDir -Filter *.exe -File |
                ForEach-Object { "$($_.Name):$($_.Length):$($_.LastWriteTimeUtc.Ticks)" }) -join '|'
        Check 'shim 文件的长度与修改时间一个都没变' ($fingerprintBefore -ceq $fingerprintAfter)

        $back = Invoke-Tool $cli @('use', 'node', $activeNow)
        Check "use 回 $activeNow 退出 0" ($back.Exit -eq 0)
        $viaShim3 = ((& $shimNode -v) 2>&1) -join ''
        Check "切回去之后 shim 又报 v$activeNow" ($viaShim3.Trim() -eq "v$activeNow") "实际：$viaShim3"
    }
}

Write-Host ''
Write-Host '═══ 4. 一个全新终端会拿到什么（只读） ═══'
$freshScript = Join-Path $PSScriptRoot 'fresh-terminal.ps1'
$fresh = (& pwsh -NoProfile -File $freshScript 'where node & node -v' 2>&1) -join "`n"
$freshExit = $LASTEXITCODE
Check '新终端模拟跑得动' ($freshExit -eq 0) $fresh
$freshFirst = ($fresh -split "`n" | Where-Object { $_ -match '\.exe$' } | Select-Object -First 1)
$freshPathChars = [int]([regex]::Match($fresh, '(?m)^PATH_CHARS=(\d+)').Groups[1].Value)
$expectedChars = $machineRawBefore.Length + 1 + $userRawBefore.Length
Check '模拟出来的 PATH 恰好是「机器级 + ; + 用户级」' ($freshPathChars -eq $expectedChars) "实际 $freshPathChars，期望 $expectedChars"
Note "新终端里 `where node` 第一个命中的是：$freshFirst"

$shadowDry = Invoke-Tool $cli @('path', 'add', $shimDir, '--dry-run')
Check 'path add <shim 目录> --dry-run 退出 0' ($shadowDry.Exit -eq 0) "退出码 $($shadowDry.Exit)"
$ourShimWins = $freshFirst -eq (Join-Path $shimDir 'node.exe')
if ($ourShimWins) {
    Check '我们的 shim 在新终端里赢了名字冲突' $true
}
else {
    # 被遮蔽**不是失败** —— 票据原文允许这条路，但要求 tuoen 如实报告并给下一步。
    Check '被遮蔽时 tuoen 报告了遮蔽' ($shadowDry.Out -match '遮蔽') 'path add --dry-run 里没有遮蔽那一段'
    Check '被遮蔽时 tuoen 点名了是谁抢的' ($shadowDry.Out -match [regex]::Escape($freshFirst)) "输出里没出现 $freshFirst"
    Check '被遮蔽时 tuoen 给了可执行的下一步' ($shadowDry.Out -match '要管理员权限|path remove|新终端') '没看到下一步'
    Note '被遮蔽 —— 这正是票据原文预料到的那条路'
}
Write-Host ''
Write-Host '═══ 5. 把遮蔽者从模拟里去掉之后 ═══'
if ($ourShimWins) {
    Skip '去掉遮蔽者再试' '本来就没被遮蔽'
}
else {
    $shadower = ($freshFirst | Split-Path -Parent)
    # 这一步模拟**两件**事，都只发生在这个子进程的环境块里，注册表一个字节不动：
    #   ① `tuoen path add <shim 目录>` 真的执行了（追加在末尾）；
    #   ② 用户把机器级那条遮蔽者去掉了。
    # 为什么必须显式模拟 ①：验收脚本自己**不做**真实 PATH 写入（见 §6 与脚本头注释），
    # 所以此刻注册表里根本没有我们的目录 —— 不模拟的话"去掉遮蔽者"只会得到"什么都找不到"。
    $without = (& pwsh -NoProfile -File $freshScript -RemoveEntry $shadower -AppendEntry $shimDir 'where node & node -v' 2>&1) -join "`n"
    Note '这一步模拟了：① `tuoen path add <shim 目录>` 已执行；② 遮蔽者已被去掉。两者都只在子进程环境块里'
    $withoutFirst = ($without -split "`n" | Where-Object { $_ -match '\.exe$' } | Select-Object -First 1)
    Check '去掉遮蔽者之后，第一个命中的是我们的 shim' ($withoutFirst -eq (Join-Path $shimDir 'node.exe')) "实际：$withoutFirst"
    $active = ((Invoke-Tool $cli @('list', '--json')).Out | ConvertFrom-Json).data.tools[0].version
    $reported = (($without -split "`n") | Where-Object { $_ -match '^v\d' } | Select-Object -First 1)
    Check "它报的版本就是当前生效的那个（v$active）" ($null -ne $reported -and $reported.Trim() -eq "v$active") "实际：$reported"
    $withoutChars = [regex]::Match($without, '(?m)^PATH_CHARS=(\d+)').Groups[1].Value
    Note "新终端模拟的 PATH：不去掉遮蔽者时 $freshPathChars 字符，去掉 $shadower 并追加 shim 目录之后 $withoutChars 字符"
}

Write-Host ''
Write-Host '═══ 6. 观察：path remove 拒绝摘掉 shim 目录 ═══'
if (-not (Test-Path $shimDir)) {
    Skip 'path remove <shim 目录> 的形状' 'shim 目录还不存在（这一段要在 §3 真的生成过之后才有意义）'
}
else {
    $removeShimDir = Invoke-Tool $cli @('path', 'remove', $shimDir)
    if ($removeShimDir.Exit -ne 0 -and $removeShimDir.Out -match 'protected-shim-dir') {
        Note 'path remove <shim 目录> 被拒绝（exit 1，protected-shim-dir）—— **这是现状，不是本脚本的失败**'
        Note '含义：`path add <shim 目录>` 能做，但没有命令化的退路；复原只能手工改环境变量'
        Note '已记录为 issue（见 docs/acceptance/L0-09-real-machine.md §5.2）'
    }
    else {
        Check 'path remove <shim 目录> 应当被拒绝（protected-shim-dir）' $false "退出码 $($removeShimDir.Exit)：$($removeShimDir.Out)"
    }
}

Write-Host ''
Write-Host '═══ 7. 清理与复核 ═══'
& cargo build --release -p tuoen-cli 2>&1 | Out-Null
$shimNode = Join-Path $shimDir 'node.exe'
if (Test-Path $shimNode) {
    # 先量一下启动开销：直跑 vs 经 shim。**两次都在同一个进程里量**，
    # 所以 PowerShell 自己的启动开销在两边都出现，差值才是 shim 的增量。
    $directNode = Join-Path $storeRoot 'node\current\node.exe'
    function Measure-Median {
        param([string]$Exe, [string[]]$Argv, [int]$Rounds = 20, [int]$Warm = 4)
        $times = @()
        for ($i = 0; $i -lt ($Rounds + $Warm); $i++) {
            $sw = [Diagnostics.Stopwatch]::StartNew()
            & $Exe @Argv | Out-Null
            $sw.Stop()
            if ($i -ge $Warm) { $times += $sw.Elapsed.TotalMilliseconds }
        }
        $sorted = @($times | Sort-Object)
        return [math]::Round($sorted[[int]($sorted.Count / 2)], 2)
    }
    $directMs = Measure-Median $directNode @('--version')
    $shimMs = Measure-Median $shimNode @('--version')
    Note "node --version：直跑 $directMs ms，经 shim $shimMs ms，**增量 $([math]::Round($shimMs - $directMs, 2)) ms**"

    if ($shimsCreatedByUs) {
        $names = @(Get-ChildItem $shimDir -Filter *.exe -File | ForEach-Object { $_.BaseName })
        $rm = Invoke-Tool $cli (@('shim', 'remove') + $names)
        Check 'shim remove 退出 0' ($rm.Exit -eq 0) "退出码 $($rm.Exit)：$($rm.Out)"
        if (-not $shimsExistedBefore) {
            Remove-Item $shimDir -Recurse -Force -ErrorAction SilentlyContinue
        }
        $after = @(Get-ChildItem $shimDir -File -ErrorAction SilentlyContinue | ForEach-Object { $_.Name })
        Check 'shim 目录回到了跑之前的样子' (($after -join ',') -ceq ($shimsBefore -join ',')) "跑前 [$($shimsBefore -join ',')]，跑后 [$($after -join ',')]"
    }
}

if (-not $alreadyThere -and $installSeconds -ne '') {
    $un = Invoke-Tool $cli @('uninstall', 'node', $target)
    Check "uninstall node $target 退出 0" ($un.Exit -eq 0) "退出码 $($un.Exit)：$($un.Out)"
    Check "存储里不再有 $target" (-not (Test-Path (Join-Path $storeRoot "node\versions\$target")))
}
$residue = @(Get-ChildItem $storeRoot -Recurse -Force -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like '.staging-*' })
Check '存储里没有 .staging- 残留' ($residue.Count -eq 0) "残留 $($residue.Count) 个"
$tempResidue = @(Get-ChildItem $env:TEMP -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like 'tuoen-*' -and $_.Name -notin @('tuoen-l009', 'tuoen-facts') })
Check '%TEMP% 里没有解压/测试残留目录' ($tempResidue.Count -eq 0) "残留 $($tempResidue.Count) 个：$((@($tempResidue | ForEach-Object { $_.Name })) -join ', ')"

$snapshotAfter = Get-PathSnapshot
Check '两个作用域的 Path **逐字节没有变**' ($snapshotAfter -ceq $snapshotBefore) '快照不一致'
$nvmAfter = Get-NvmState
Check 'nvm4w 的链接目标与环境变量一个字节没动（ADR-0004）' ($nvmAfter -ceq $nvmBefore) "跑前 $nvmBefore / 跑后 $nvmAfter"

if ($SelfTest) {
    Write-Host ''
    Write-Host '═══ 自检：故意加一条必然失败的期望 ═══' -ForegroundColor Magenta
    Check '自检用：这一条必须失败，用来证明退出码真的会跟着失败数走' $false '自检'
}

Write-Host ''
Write-Host '═══ 摘要 ═══'
Write-Host ("checks_passed={0} checks_failed={1} checks_skipped={2}" -f $script:passed, $script:failed.Count, $script:skipped.Count)
Write-Host ("SUMMARY checks_passed={0} checks_failed={1} checks_skipped={2} install_seconds={3} shim_dir={4} path_untouched={5} nvm_untouched={6}" -f `
        $script:passed, $script:failed.Count, $script:skipped.Count, $(if ($installSeconds -eq '') { 'n/a' } else { $installSeconds }), `
        $shimDir, ($snapshotAfter -ceq $snapshotBefore), ($nvmAfter -ceq $nvmBefore))
if ($script:skipped.Count -gt 0) {
    Write-Host "跳过（**不是通过**）：$($script:skipped -join ' / ')" -ForegroundColor Yellow
}
if ($script:failed.Count -gt 0) {
    Write-Host "失败：`n  - $($script:failed -join "`n  - ")" -ForegroundColor Red
    exit 1
}
exit 0
