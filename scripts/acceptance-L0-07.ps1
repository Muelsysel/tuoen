# 票据 #7（shim）的真机验收脚本。
#
# 设计约束（决策 50）：**验收脚本必须能失败**。每一次期望都走 `Check`，
# 退出码跟着失败数走 —— 一个永远 exit 0 的"验收"不是验收。
#
# 三条铁律（见 AGENTS.md「会启动进程的代码」）：
#   1. 本脚本启动的每一个程序都**不会启动它自己**；
#   2. 命令行只由被测程序自己构造；
#   3. 跑之前先算最坏进程数（这里最多同时 4 个），每一段都有硬超时，跑完清场核对。
#
# 用法：pwsh scripts/acceptance-L0-07.ps1            （默认写进 docs/acceptance/L0-07-shim-machine.txt）

[CmdletBinding()]
param(
    [string]$OutFile = "docs/acceptance/L0-07-shim-machine.txt",
    [string]$NodeExe = ""
)

$ErrorActionPreference = "Continue"
$script:failures = 0
$script:checks = 0

function Check {
    param([string]$What, [bool]$Ok, [string]$Detail = "")
    $script:checks++
    if ($Ok) {
        Write-Output ("  [通过] {0}" -f $What)
    } else {
        $script:failures++
        Write-Output ("  [失败] {0}" -f $What)
        if ($Detail) { Write-Output ("         {0}" -f $Detail) }
    }
}

function Section { param([string]$Title) Write-Output ""; Write-Output ("=== {0} ===" -f $Title) }

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"

Write-Output "tuoen · 票据 #7（shim）真机验收"
Write-Output ("时间：{0}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss zzz"))
Write-Output ("工作目录：{0}" -f $root)

# ── 0. 预检：不许有残留进程 ─────────────────────────────────────────────────
Section "0. 预检（跑之前先确认地面是干净的）"
$probeNames = @('ctrlc_host', 'ctrlc_runner', 'naive_forwarder', 'argdump', 'probe-shim', 'gone')
$dirty = @()
foreach ($n in $probeNames) {
    $c = @(Get-Process -Name $n -ErrorAction SilentlyContinue).Count
    if ($c -gt 0) { $dirty += "$n=$c" }
}
Check "没有任何验收探针的残留进程" ($dirty.Count -eq 0) ($dirty -join ", ")
if ($dirty.Count -gt 0) {
    Write-Output "残留进程存在，先清场再验收 —— 不在脏地上做测量。"
    foreach ($n in $probeNames) { Get-Process -Name $n -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue }
}

# ── 1. 构建 ────────────────────────────────────────────────────────────────
Section "1. 构建 release 产物"
& cargo build --release --examples -p tuoen-shim 2>&1 | Out-String | Write-Output
Check "cargo build --release --examples -p tuoen-shim 成功" ($LASTEXITCODE -eq 0) "exit=$LASTEXITCODE"

$template = "target\release\tuoen-shim.exe"
Check "模板存在" (Test-Path $template)
$tplSize = if (Test-Path $template) { (Get-Item $template).Length } else { 0 }
Write-Output ("  模板大小：{0} 字节" -f $tplSize)

# ── 2. 模板的槽位结构（验收标准 ②：目标写死进二进制）────────────────────────
Section "2. 模板里的槽位结构"
& pwsh -NoProfile -File "$PSScriptRoot\dump-pe-sections.ps1" -Path $template 2>&1 | Write-Output
$peOut = & pwsh -NoProfile -File "$PSScriptRoot\dump-pe-sections.ps1" -Path $template 2>&1 | Out-String
# 解析那个**约定好的**机器可读行，不从散文里捞数字。
# 第一版这里是错的：它去匹配一句 `dump-pe-sections.ps1` 从来没打印过的中文，
# 于是给出了一次假失败 —— 验收脚本读错东西，和被测代码错一样严重（决策 50）。
$summary = [regex]::Match($peOut, 'SUMMARY sections=(\d+) magic_hits=(\d+) pristine=(\d+) slots_inside_raw=(\d+)')
Check "PE 转储给出了 SUMMARY 契约行" $summary.Success
if ($summary.Success) {
    $magicHits = [int]$summary.Groups[2].Value
    $pristineCount = [int]$summary.Groups[3].Value
    $insideRaw = [int]$summary.Groups[4].Value
    Write-Output ("  节数={0} 魔数出现={1} 未烘过的槽位={2} 整段落在 raw data 内={3}" -f `
        $summary.Groups[1].Value, $magicHits, $pristineCount, $insideRaw)
    Check "模板里有一处「未烘过」的槽位（否则无槽可烘）" ($pristineCount -ge 1) "pristine=$pristineCount"
    Check "每一处魔数都整段落在一个节的 raw data 窗口内" ($insideRaw -eq $magicHits) "inside=$insideRaw magic=$magicHits"
}

# ── 3. 验收标准 ⑦：启动开销（带器材自检）──────────────────────────────────
Section "3. 启动开销实测（验收标准 ⑦）"
if (-not $NodeExe) {
    $found = @(Get-Command node.exe -ErrorAction SilentlyContinue)
    if ($found.Count -gt 0) { $NodeExe = $found[0].Source }
}
if ($NodeExe) { $env:TUOEN_SHIM_NODE = $NodeExe; Write-Output "  真实工具：$NodeExe" }
$probeOut = & ".\target\release\examples\startup_probe.exe" 2>&1 | Out-String
Write-Output $probeOut
Check "启动开销探针正常结束" ($LASTEXITCODE -eq 0) "exit=$LASTEXITCODE"
Check "器材自检：转发器确实在等子进程" ($probeOut -match '\*\*确实在等\*\*')
Check "器材自检：stdout 与直接跑逐字节一致" ($probeOut -match '逐字节一致')
Check "量到了转发增量" ($probeOut -match '增量：中位')

# ── 4. 验收标准 ⑥：Ctrl-C 对照实验 ────────────────────────────────────────
Section "4. Ctrl-C 对照实验（验收标准 ⑥）"
# 注意不要用 `$host` 当变量名 —— 那是 PowerShell 的自动变量（宿主对象）。
$ctrlcOutFile = Join-Path $env:TEMP "tuoen-ctrlc-acceptance.txt"
Remove-Item $ctrlcOutFile -Force -ErrorAction SilentlyContinue
$hostProc = Start-Process -FilePath ".\target\release\examples\ctrlc_host.exe" `
    -NoNewWindow -PassThru -RedirectStandardOutput $ctrlcOutFile
$timedOut = $false
try { Wait-Process -Id $hostProc.Id -Timeout 90 -ErrorAction Stop } catch { $timedOut = $true }
if ($timedOut) {
    Write-Output "  !! 90 秒没结束，强杀整棵树"
    & taskkill.exe /F /T /PID $hostProc.Id 2>&1 | Select-Object -Last 1 | Write-Output
}
$ctrlcOut = if (Test-Path $ctrlcOutFile) { Get-Content $ctrlcOutFile -Raw } else { "" }
Write-Output $ctrlcOut
Check "Ctrl-C 实验在 90 秒内结束" (-not $timedOut)
Check "Ctrl-C 实验退出码 0" ($hostProc.ExitCode -eq 0) "exit=$($hostProc.ExitCode)"
Check "实验确实跑在私有控制台上" ($ctrlcOut -match '私有控制台窗口句柄：0x[1-9a-f]')
Check "反例（少写一行的转发器）**先死了**" ($ctrlcOut -match '转发器先死了')
Check "反例的退出码是 STATUS_CONTROL_C_EXIT(0xC000013A)" ($ctrlcOut -match '0xC000013A')
Check "我们的 shim **活到了最后**" ($ctrlcOut -match '转发器活到了最后')
Check "我们的 shim 交出了子进程的退出码 7" ($ctrlcOut -match '退出码 0x00000007')

# 事后清场 + 核对
$leftovers = @()
foreach ($n in $probeNames) {
    $c = @(Get-Process -Name $n -ErrorAction SilentlyContinue)
    if ($c.Count -gt 0) {
        $leftovers += "$n=$($c.Count)"
        $c | Stop-Process -Force -ErrorAction SilentlyContinue
    }
}
Check "实验没有留下任何残留进程" ($leftovers.Count -eq 0) ($leftovers -join ", ")

# ── 5. 门禁 ────────────────────────────────────────────────────────────────
Section "5. 工作区门禁"
$testOut = & cargo test --workspace 2>&1 | Out-String
$testLines = ($testOut -split "`n") | Where-Object { $_ -match 'test result:' }
Write-Output ($testLines -join "`n")
$totalPassed = 0; $totalFailed = 0
foreach ($l in $testLines) {
    if ($l -match '(\d+) passed; (\d+) failed') { $totalPassed += [int]$Matches[1]; $totalFailed += [int]$Matches[2] }
}
Write-Output ("  合计：{0} passed / {1} failed" -f $totalPassed, $totalFailed)
Check "cargo test --workspace 退出码 0" ($LASTEXITCODE -eq 0) "exit=$LASTEXITCODE"
Check "cargo test --workspace 零失败" ($totalFailed -eq 0)
Check "cargo test --workspace 至少跑了 500 条" ($totalPassed -ge 500) "passed=$totalPassed"

& cargo test --release -p tuoen-shim 2>&1 | Out-String | Write-Output
Check "cargo test --release -p tuoen-shim 退出码 0（常量折叠只在 release 下出现）" ($LASTEXITCODE -eq 0) "exit=$LASTEXITCODE"

& cargo clippy --workspace --all-targets -- -D warnings 2>&1 | Out-String | Write-Output
Check "cargo clippy -D warnings 退出码 0" ($LASTEXITCODE -eq 0) "exit=$LASTEXITCODE"

& cargo fmt --all --check 2>&1 | Out-String | Write-Output
Check "cargo fmt --all --check 退出码 0" ($LASTEXITCODE -eq 0) "exit=$LASTEXITCODE"

# ── 6. 不许碰真实机器 ──────────────────────────────────────────────────────
Section "6. 验收没有碰真实机器"
$realHome = Join-Path $env:LOCALAPPDATA "tuoen"
$realShims = Join-Path $realHome "shims"
Check "真实的 %LOCALAPPDATA%\tuoen\shims 不存在" (-not (Test-Path $realShims)) "$realShims"
if (Test-Path $realHome) {
    $top = @(Get-ChildItem $realHome -Force | Select-Object -ExpandProperty Name)
    Write-Output ("  真实家目录顶层：{0}" -f ($top -join ", "))
}

# ── 结论 ───────────────────────────────────────────────────────────────────
Section "结论"
Write-Output ("检查 {0} 项，失败 {1} 项" -f $script:checks, $script:failures)
if ($script:failures -eq 0) {
    Write-Output "全部通过。"
    exit 0
} else {
    Write-Output "**有失败项 —— 这张验收不能算通过。**"
    exit 1
}
