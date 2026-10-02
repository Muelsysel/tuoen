<#
.SYNOPSIS
    票据 #15（L1 `path diff` / `path apply`：PATH 逐条 diff + 重建 + 选择性应用）的真机验收。

.DESCRIPTION
    这一票的立场是"本机 `PATH` 已经是坏的，**原样搬运会把旧病一起移植**"，所以做的是
    **重建 + 逐条 diff + 选择性应用**。它的失效形态不是崩溃，而是：

    * **顺手重排**：用户只想 `--only add`，结果整条 `PATH` 按目标顺序被重排 —— 重排就是改优先级；
    * **预览与执行不一致**：用户照着 `--dry-run` 的结论点了"确认"，写下去的却是另一份；
    * **写进去就静默截断**：超过 8191 之后 `cmd.exe` 整条忽略 `PATH`。

    所以这条脚本的核心判据是三条：
    ① **脚本自己**从注册表（不展开读）与 `path.toml`（用 Python 的 `tomllib` 独立解析，
       不经过 tuoen 的任何代码）把六个 diff 类**重算一遍**，再与 `--json` 逐行对照；
    ② `--dry-run` 前后，两个作用域的 `Path` 的**原文 + 类型 + 字符数 + SHA-256** 逐项相同，
       且 `%LOCALAPPDATA%\tuoen` 清单逐项相同；
    ③ **真的写一次**（`--only add` 一条真实存在的目录），然后用 `scripts/fresh-terminal.ps1`
       在一个"新终端会拿到的环境"里证明它**真的生效了**（决策 141 / AGENTS.md 规矩 6），
       再原样写回并用哈希证明**一个字节都没留下**。

    第 ③ 条会真的改一次 `HKCU\Environment\Path`（773 字符，写回原值）—— 这是票据要求的
    "选择性应用的一次真实演示"。写与还原包在 `try/finally` 里：脚本中途死掉也会还原。

    **一条不能失败的验收不是验收**：`-SelfTest` 会让最后一条检查故意失败（exit 1）。

.PARAMETER SelfTest
    故意让最后一条检查失败，用来证明这条脚本**会**报失败（exit 1）。

.PARAMETER SkipBuild
    跳过 §0 的 release 构建（只在已经构建过、且想快速重跑时用）。

.PARAMETER SkipWrite
    跳过 §6 的**真实写入**（只验只读部分）。用来在调试脚本本身时不动 `HKCU` ——
    权威的那一次必须**不带**这个开关跑。

.PARAMETER Version
    要验收的版本号，只进摘要行。

.EXAMPLE
    pwsh -File scripts/acceptance-L1-15.ps1
    pwsh -File scripts/acceptance-L1-15.ps1 -SelfTest   # 必须 exit 1
#>
[CmdletBinding()]
param(
    [switch]$SelfTest,
    [switch]$SkipBuild,
    [switch]$SkipWrite,
    [string]$Version = '0.1.0'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repo 'target\release\tuoen.exe'
$root = Join-Path $env:TEMP 'tuoen-acceptance-L1-15'
Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $root | Out-Null

$script:Passed = 0
$script:Failed = 0
$script:SkipCount = 0
$script:Failures = @()

function Check {
    param([string]$Name, [bool]$Ok, [string]$Detail = '')
    if ($Ok) {
        $script:Passed++
        Write-Host ("  [ok]   {0}{1}" -f $Name, $(if ($Detail) { "  $Detail" } else { '' }))
    } else {
        $script:Failed++
        $script:Failures += $Name
        Write-Host ("  [FAIL] {0}{1}" -f $Name, $(if ($Detail) { "  $Detail" } else { '' })) -ForegroundColor Red
    }
}

function Skip {
    param([string]$Name, [string]$Why)
    $script:SkipCount++
    Write-Host ("  [skip] {0}  ({1})" -f $Name, $Why) -ForegroundColor Yellow
}

function Section {
    param([string]$Title)
    Write-Host ''
    Write-Host "── $Title" -ForegroundColor Cyan
}

# ── 器材：注册表（不展开读；写只用于"还原"这一处） ───────────────────────

$ENV_KEYS = @{
    'user'    = 'HKCU:\Environment'
    'machine' = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
}

Add-Type -Namespace Win32 -Name EnvBroadcast -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, IntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out IntPtr lpdwResult);
'@

# 原文 + 类型。**必须 `DoNotExpandEnvironmentNames`**：展开是不可逆的信息损失（AGENTS.md 规矩 2）。
function Get-PathValue {
    param([string]$Scope)
    $item = Get-Item $ENV_KEYS[$Scope]
    [pscustomobject]@{
        Raw  = [string]$item.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        Kind = [string]$item.GetValueKind('Path')
    }
}

# **这条脚本里唯一一处写注册表**，只用于把 §6 的那次真实写入还原回去。
#
# **绝不能用 `(Get-Item 'HKCU:\…').SetValue(…)`**：PowerShell 的注册表 provider 返回的是
# **只读**句柄，`SetValue` 会抛 `Cannot write to the registry key.` —— 而这条函数正是那次
# 真实写入的**还原**路径，于是"写进去了、还原不了"这个最坏的组合真的发生过一次：
# 真机 `HKCU\Environment\Path` 被留在 808 字符的改动值上，直到按跑前备份的原文与类型
# （`%TEMP%\_l115_hkcu_backup.txt`，773 字符 / `REG_SZ` / sha16 `a12fc582e90513a3`）
# 用**可写子键句柄**手工写回并逐字核对。证据见
# `docs/acceptance/L1-15-path-rebuild-restore-incident.txt`。
function Set-PathValue {
    param([string]$Scope, [string]$Raw, [string]$Kind)
    $sub = if ($Scope -eq 'user') {
        [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    } else {
        # 机器级要提权；这条脚本从不写机器级（`--only add` 的目标是用户级），留在这里只为
        # 万一：拿不到可写句柄时**报出来**，而不是静默什么都没写。
        [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey(
            'SYSTEM\CurrentControlSet\Control\Session Manager\Environment', $true)
    }
    if ($null -eq $sub) { throw "打不开可写的 $Scope 环境键（机器级需要提权）" }
    try {
        $value_kind = if ($Kind -eq 'ExpandString') { [Microsoft.Win32.RegistryValueKind]::ExpandString }
                      else { [Microsoft.Win32.RegistryValueKind]::String }
        $sub.SetValue('Path', $Raw, $value_kind)
    } finally {
        $sub.Close()
    }
    $out = [IntPtr]::Zero
    # 广播 `WM_SETTINGCHANGE`，与产品做的是同一件事（`SMTO_ABORTIFHUNG`，5 秒）。
    [Win32.EnvBroadcast]::SendMessageTimeout([IntPtr]0xffff, 0x1a, [IntPtr]::Zero, 'Environment', 0x0002, 5000, [ref]$out) | Out-Null
}

function Get-Sha16 {
    param([string]$Text)
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
    $hash = [System.Security.Cryptography.SHA256]::Create().ComputeHash($bytes)
    (([System.BitConverter]::ToString($hash)) -replace '-', '').ToLowerInvariant().Substring(0, 16)
}

function Get-Snapshot {
    $u = Get-PathValue 'user'
    $m = Get-PathValue 'machine'
    [pscustomobject]@{
        UserRaw    = $u.Raw
        UserKind   = $u.Kind
        UserChars  = $u.Raw.Length
        UserSha    = Get-Sha16 $u.Raw
        MachineRaw = $m.Raw
        MachineKind = $m.Kind
        MachineChars = $m.Raw.Length
        MachineSha = Get-Sha16 $m.Raw
    }
}

function Get-TreeListing {
    param([string]$Root, [int]$Depth = 2)
    if (-not (Test-Path -LiteralPath $Root)) { return @('<absent>') }
    @(Get-ChildItem -LiteralPath $Root -Force -Recurse -Depth $Depth |
            Sort-Object FullName | ForEach-Object { $_.FullName.Substring($Root.Length) })
}

function Invoke-Tuoen {
    param([string[]]$Arguments, [string]$Name = 'run', [hashtable]$ExtraEnv = @{})
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $exe
    foreach ($a in $Arguments) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    # 子进程的 PATH = **新终端口径**（从注册表重建），而不是本脚本继承来的那份：
    # 否则"本机侧"会被 cargo / PowerShell 注入的条目污染（分母必须说出来）。
    $psi.EnvironmentVariables['PATH'] = $script:Pristine
    $psi.EnvironmentVariables['Path'] = $script:Pristine
    foreach ($k in $ExtraEnv.Keys) { $psi.EnvironmentVariables[$k] = $ExtraEnv[$k] }
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stdout = $proc.StandardOutput.ReadToEnd()
    $stderr = $proc.StandardError.ReadToEnd()
    $proc.WaitForExit()
    [pscustomobject]@{
        Exit   = $proc.ExitCode
        Stdout = $stdout
        Stderr = $stderr
        Args   = ($Arguments -join ' ')
    }
}

function Get-Json {
    param([object]$Result)
    try { $Result.Stdout | ConvertFrom-Json } catch { $null }
}

function Test-NoCjk {
    param([string]$Text)
    -not [bool]($Text -match '[^\x00-\x7F]')
}

# 取一个可能不存在的属性。`Set-StrictMode -Version Latest` 下访问不存在的属性是**终止错误** ——
# 而这条脚本的职责恰恰是"产品没给这个键时**报一条 FAIL**"，不是当场死掉。
function Prop {
    param([object]$Obj, [string]$Name)
    if ($null -eq $Obj) { return $null }
    if (@($Obj.PSObject.Properties.Name) -contains $Name) { $Obj.$Name } else { $null }
}

# 成功载荷的形状是 `{schemaVersion, command, ok, data:{…}}`。**这一层信封在断言里不该出现**：
# 第一版每条成功断言都直接读信封，于是 `Prop $dj 'rows'` 返回 `$null`，而 `@($null).Count` 是
# **1**（不是 0）—— 五条断言同时报"产品 1 条 / 脚本 50 条"，看起来像产品只输出了一行。
function Get-Payload {
    param([object]$Result)
    $j = Get-Json $Result
    if ($null -eq $j) { return $null }
    Prop $j 'data'
}

# 数一个"可能不在"的数组：`Prop` 对不存在的键返回 `$null`、对**空数组**返回"什么都没有"，
# 两种都会被 `@(...)` 变成一个元素的数组。要数条数的地方一律走这里。
function Arr {
    param([object]$Obj, [string]$Name)
    @(Prop $Obj $Name | Where-Object { $null -ne $_ })
}

# ── 独立解析：`path.toml` 用 Python 的 `tomllib`，注册表用 .NET ─────────────

$PY = Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\python.exe'
if (-not (Test-Path -LiteralPath $PY)) {
    # 只认真实的解释器：`WindowsApps\python.exe` 是 App Execution Alias（0 字节重解析点），
    # 而"绝不执行 App Execution Alias"是本仓库的硬约束。
    throw "找不到真实 Python：$PY（不接受 WindowsApps 的别名）"
}

$tomlHelper = Join-Path $root '_rows.py'
@'
import json, sys, tomllib
with open(sys.argv[1], "rb") as fh:
    doc = tomllib.load(fh)
rows = []
for r in doc.get("entry", []):
    rows.append({k: r.get(k) for k in ("scope", "index", "raw", "expanded", "exists", "empty",
                                       "dup_index", "has_username", "owner", "reparse", "reg_type")})
print(json.dumps({"entry": rows, "budget": doc.get("budget", {})}, ensure_ascii=False))
'@ | Set-Content -Path $tomlHelper -Encoding utf8 -NoNewline

function Get-TomlRows {
    param([string]$File)
    $json = & $PY $tomlHelper $File
    if ($LASTEXITCODE -ne 0) { throw "tomllib 解析失败：$File" }
    $json | ConvertFrom-Json
}

# 造一份"目标里少了最后 N 条用户级条目"的快照：**只删 `[[entry]]` 块**，其余文本逐字保留
# （`[budget]` / `[[effective]]` / 注释都不动），再追加一条 add。用它把 `remove` 类真的造出来 ——
# 本机真快照里 `remove` 是 0 条，而票据点名的场景恰恰是"只选 add，remove 类条目不许被改动"。
$dropHelper = Join-Path $root '_droptarget.py'
@'
import re, sys

src, dst, drop, add = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
with open(src, encoding="utf-8") as fh:
    text = fh.read()
lines = text.splitlines(keepends=True)

starts = [i for i, ln in enumerate(lines) if ln.strip() == "[[entry]]"]
ranges = []
for k, s in enumerate(starts):
    e = starts[k + 1] if k + 1 < len(starts) else len(lines)
    for j in range(s + 1, e):          # 块尾：遇到下一个表头就停（`[budget]` 之类）
        t = lines[j].lstrip()
        if t.startswith("[") and not t.startswith("[["):
            e = j
            break
    ranges.append((s, e))

def body(r):
    return "".join(lines[r[0]:r[1]])

user = [r for r in ranges if 'scope = "user"' in body(r)]
drop_ranges = user[-drop:]
keep = [ln for i, ln in enumerate(lines) if not any(s <= i < e for (s, e) in drop_ranges)]

maxidx = -1
for r in ranges:
    if r in drop_ranges or 'scope = "user"' not in body(r):
        continue
    m = re.search(r"^index = (\d+)", body(r), re.M)
    if m:
        maxidx = max(maxidx, int(m.group(1)))
idx = maxidx + 1
esc = add.replace("\\", "\\\\")
block = (
    "\n# 由验收脚本追加（4d）：一条本机没有的目录\n[[entry]]\n"
    'scope = "user"\nindex = %d\nowner = "unknown"\nraw = "%s"\nexpanded = "%s"\n'
    'quoted = false\nexists = "yes"\nreparse = "none"\nhas_vars = false\n'
    'has_username = false\ndup_index = 0\nempty = false\nreg_type = "sz"\n'
) % (idx, esc, esc)
with open(dst, "w", encoding="utf-8", newline="\n") as fh:
    fh.write("".join(keep) + block)
print(idx)
'@ | Set-Content -Path $dropHelper -Encoding utf8 -NoNewline

# 一条条目在**本机**的事实 —— 判据逐条对齐 `collect/path.rs` 的模块文档：
# `sz` 的值不展开（原文就是答案）、展开后还留 `%` 就不问磁盘、空条目不问磁盘。
function Get-LocalRows {
    param([string]$Raw, [string]$Kind)
    $rows = @()
    $parts = $Raw -split ';'
    for ($i = 0; $i -lt $parts.Count; $i++) {
        $part = $parts[$i]
        $trimmed = $part.Trim()
        $inner = $trimmed
        $quoted = $false
        if ($trimmed.Length -ge 2 -and $trimmed.StartsWith('"') -and $trimmed.EndsWith('"')) {
            $inner = $trimmed.Substring(1, $trimmed.Length - 2)
            $quoted = $true
        }
        # `normalize_entry`：去首尾空白 + 去多余的结尾反斜杠（保留 `X:\`）。
        $v = $inner.Trim()
        while ($v.Length -gt 0 -and ($v.EndsWith('\') -or $v.EndsWith('/'))) {
            if ($v.Length -eq 3 -and $v[1] -eq ':') { break }
            $v = $v.Substring(0, $v.Length - 1)
        }
        $expanded = if ($Kind -eq 'ExpandString') { [Environment]::ExpandEnvironmentVariables($v) } else { $v }
        $empty = ($v -eq '')
        $exists = if ($empty) { 'unknown' }
                  elseif ($expanded.Contains('%')) { 'unknown' }
                  elseif (Test-Path -LiteralPath $expanded) { 'yes' } else { 'no' }
        $rows += [pscustomobject]@{
            Index   = $i
            Raw     = $part
            Value   = $v
            Folded  = $v.ToLowerInvariant()
            Empty   = $empty
            Exists  = $exists
            HasUser = [bool](Get-HardcodedUser $v)
        }
    }
    , $rows
}

# `hardcoded_username` 的判据（`\Users\<名字>\`，名字里含 `%` 不算）。
function Get-HardcodedUser {
    param([string]$Value)
    $lowered = $Value.ToLowerInvariant()
    $at = $lowered.IndexOf('\users\')
    if ($at -lt 0) { return $null }
    $rest = $Value.Substring($at + 7)
    if ($rest.Length -eq 0) { return $null }
    $end = $rest.IndexOfAny([char[]]@('\', '/'))
    $name = if ($end -lt 0) { $rest } else { $rest.Substring(0, $end) }
    $name = $name.Trim()
    if ($name -eq '' -or $name -eq '.' -or $name -eq '..' -or $name.Contains('%')) { return $null }
    $name
}

# `dup_index`：按**本文件 `[[entry]]` 的输出顺序**（机器级 → 用户级 → 进程注入），
# 对规范化 + 小写后的值计数 —— "这个值第几次出现"。
function Add-DupIndex {
    param([object[]]$Machine, [object[]]$User)
    $seen = @{}
    $all = @()
    foreach ($r in @($Machine) + @($User)) {
        $key = $r.Folded
        $n = if ($seen.ContainsKey($key)) { $seen[$key] } else { 0 }
        $r | Add-Member -NotePropertyName DupIndex -NotePropertyValue $n -Force
        $seen[$key] = $n + 1
        $all += $r
    }
    , $all
}

# ── 独立重算六个 diff 类 ────────────────────────────────────────────────
#
# 判据（决策 127，优先级：存在性 → 健康 → 位置 → 大小写 → 相同）：
#   fix/empty-segment   任一侧是空条目
#   fix/duplicate       本机侧 `dup_index > 0`
#   fix/dangling        本机侧 `exists == no`
#   fix/username        本机侧 `has_username`
#   move                两侧都有、位置不同
#   case-only           两侧都有、位置相同、原文不同
#   keep                其余
# `add` / `remove` 是"只有一侧有"。**每条只归一"类"，类由"理由"决定。**
function Get-ExpectedDiff {
    param([object[]]$LocalMachine, [object[]]$LocalUser, [object[]]$TargetRows, [string]$CurrentUsername)
    $out = @()
    foreach ($scope in @('machine', 'user')) {
        $local = @(if ($scope -eq 'machine') { $LocalMachine } else { $LocalUser })
        $target = @($TargetRows | Where-Object { $_.scope -eq $scope } | Sort-Object index)

        # **配对必须按"值 + 出现序号"**，不能只按值：同一个值出现两次时（本机有 12 组重复），
        # 第 2 次的目标行若配到第 1 次的本机行，下标必然不同 —— 于是每一条重复都会被
        # 误报成 `move`。第一版就是这么错的（真机上多出 11 条假 move），而**假差异与
        # "代码错了"长得一模一样**（AGENTS.md「会写报告的代码」第 4 条）。
        $localOcc = @{}
        foreach ($r in $local) {
            if (-not $localOcc.ContainsKey($r.Folded)) { $localOcc[$r.Folded] = @() }
            $localOcc[$r.Folded] += , $r
        }
        $targetOcc = @{}
        foreach ($t in $target) {
            $key = Normalize-RowValue $t.expanded
            if (-not $targetOcc.ContainsKey($key)) { $targetOcc[$key] = @() }
            $targetOcc[$key] += , $t
        }

        # 第一遍：配对。第二遍才判定 —— 因为 `move` 用的是"**共同条目序列**里的序号"
        # （决策 127 的落地定义，由 core 侧确认）：目标在前面插一条不该让后面每一行都报 move，
        # 而 `local.index != target.index` 会。两种定义在本夹具上一致（新增行在末尾），
        # 但判据必须只有一种。
        $pairs = @()
        $extras = @()
        foreach ($key in $targetOcc.Keys) {
            $ts = @($targetOcc[$key])
            # `@(...)` 必须包在**整个 if 外面**：`$x = if (…) { @(…) } else { @() }` 会被
            # PowerShell 拆掉数组（空数组变成 `$null`），于是 StrictMode 下 `.Count` 报
            # "在此对象上找不到属性 Count"（AGENTS.md 里记的同一个坑）。
            $ls = @(if ($localOcc.ContainsKey($key)) { $localOcc[$key] } else { @() })
            $n = [Math]::Min($ts.Count, $ls.Count)
            for ($i = 0; $i -lt $n; $i++) {
                $pairs += [pscustomobject]@{ Local = $ls[$i]; Target = $ts[$i] }
            }
            for ($i = $n; $i -lt $ts.Count; $i++) {
                $extras += [pscustomobject]@{ Side = 'target'; Row = $ts[$i] }
            }
            for ($i = $n; $i -lt $ls.Count; $i++) {
                $extras += [pscustomobject]@{ Side = 'local'; Row = $ls[$i] }
            }
        }
        $pairedIdx = @{}
        foreach ($p in $pairs) { $pairedIdx["$($p.Local.Index)"] = $true }
        foreach ($r in $local) {
            if ($pairedIdx.ContainsKey("$($r.Index)")) { continue }
            $extras += [pscustomobject]@{ Side = 'local'; Row = $r }
        }

        # 共同条目序列里的序号（各自排序后的位次）。
        $localRank = @{}
        $byLocal = @($pairs | Sort-Object { $_.Local.Index })
        for ($i = 0; $i -lt $byLocal.Count; $i++) { $localRank["$($byLocal[$i].Local.Index)"] = $i }
        $targetRank = @{}
        $byTarget = @($pairs | Sort-Object { $_.Target.index })
        for ($i = 0; $i -lt $byTarget.Count; $i++) { $targetRank["$($byTarget[$i].Target.index)"] = $i }

        foreach ($p in $pairs) {
            $l = $p.Local
            $t = $p.Target
            $id = "local:$($l.Index)"
            # 重写只在"真的有旧名可换"时才算这一类（决策 134 + 本轮的裁决）：
            # 名字就是当前用户名时，这一行按它其余的性质归类，可移植性留在 `also` 里。
            $oldName = [string](Get-HardcodedUser $l.Value)
            $needsRewrite = ($oldName -ne '') -and ($CurrentUsername -ne '') -and `
                ($oldName.ToLowerInvariant() -ne $CurrentUsername.ToLowerInvariant())
            if ($t.empty -or $l.Empty) {
                $out += [pscustomobject]@{ Scope = $scope; Id = $id; Class = 'fix'; Reason = 'empty-segment' }
            } elseif ($l.DupIndex -gt 0) {
                $out += [pscustomobject]@{ Scope = $scope; Id = $id; Class = 'fix'; Reason = 'duplicate' }
            } elseif ($needsRewrite) {
                $out += [pscustomobject]@{ Scope = $scope; Id = $id; Class = 'fix'; Reason = 'username-hardcoded' }
            } elseif ($l.Exists -eq 'no') {
                $out += [pscustomobject]@{ Scope = $scope; Id = $id; Class = 'fix'; Reason = 'dangling' }
            } elseif ([int]$localRank["$($l.Index)"] -ne [int]$targetRank["$($t.index)"]) {
                $out += [pscustomobject]@{ Scope = $scope; Id = $id; Class = 'move'; Reason = 'position-differs' }
            } elseif ($l.Raw.Trim() -cne ([string]$t.raw).Trim()) {
                $out += [pscustomobject]@{ Scope = $scope; Id = $id; Class = 'case-only'; Reason = 'case-differs' }
            } else {
                $out += [pscustomobject]@{ Scope = $scope; Id = $id; Class = 'keep'; Reason = 'identical' }
            }
        }
        foreach ($e in $extras) {
            $r = $e.Row
            if ($e.Side -eq 'target') {
                if ($r.empty) {
                    $out += [pscustomobject]@{ Scope = $scope; Id = "target:$($r.index)"; Class = 'fix'; Reason = 'empty-segment' }
                } else {
                    $out += [pscustomobject]@{ Scope = $scope; Id = "target:$($r.index)"; Class = 'add'; Reason = 'only-in-target' }
                }
            } else {
                $cls = if ($r.Empty) { 'fix' } else { 'remove' }
                $rsn = if ($r.Empty) { 'empty-segment' } else { 'only-in-local' }
                $out += [pscustomobject]@{ Scope = $scope; Id = "local:$($r.Index)"; Class = $cls; Reason = $rsn }
            }
        }
    }
    , $out
}

function Normalize-RowValue {
    param([string]$Text)
    $v = ([string]$Text).Trim().Trim('"').Trim()
    while ($v.Length -gt 0 -and ($v.EndsWith('\') -or $v.EndsWith('/'))) {
        if ($v.Length -eq 3 -and $v[1] -eq ':') { break }
        $v = $v.Substring(0, $v.Length - 1)
    }
    $v.ToLowerInvariant()
}

# ── §0 构建 ────────────────────────────────────────────────────────────

if (-not $SkipBuild) {
    Section '0 构建'
    Push-Location $repo
    $env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"
    $build = & cargo build --release -p tuoen-cli 2>&1
    $code = $LASTEXITCODE
    Pop-Location
    Check 'cargo build --release -p tuoen-cli 成功' ($code -eq 0) (($build | Select-Object -Last 1) -join '')
}

Check 'tuoen.exe 存在' (Test-Path -LiteralPath $exe) $exe

# 新终端口径的 `PATH`：机器级原文 + `;` + 用户级原文（一个字节都不写注册表）。
$machineNow = Get-PathValue 'machine'
$userNow = Get-PathValue 'user'
$script:Pristine = "$($machineNow.Raw);$($userNow.Raw)"

# 当前用户名：`HKCU\Environment` 里**没有** `USERPROFILE` / `USERNAME`（真机实测过），
# 所以这一票的来源是**进程环境** —— 与产品里 `current_username_from_process` 的口径一致。
$currentUsername = Split-Path -Leaf $env:USERPROFILE
if (-not $currentUsername) { $currentUsername = $env:USERNAME }
Write-Host ("       当前用户名（脚本自己从进程环境取）：{0}" -f $currentUsername)

# ── §1 准备：真快照 + 脚本自己造的"目标快照"（多一条 add） ───────────────

Section '1 准备：真快照、目标快照、前后快照'

$before = Get-Snapshot
$realStore = Join-Path $env:LOCALAPPDATA 'tuoen'
$beforeStore = Get-TreeListing -Root $realStore -Depth 2
$beforeRealAppData = Get-TreeListing -Root (Join-Path $env:APPDATA 'tuoen') -Depth 2

$captureDir = Join-Path $root 'tuoen.d'
$cap = Invoke-Tuoen -Arguments @('capture', '--only', 'path', '--out', $captureDir, '--json') -Name 'capture'
Check 'capture --only path 退出 0' ($cap.Exit -eq 0) "exit=$($cap.Exit)"
$realToml = Join-Path $captureDir 'path.toml'
Check 'path.toml 写出来了' (Test-Path -LiteralPath $realToml) $realToml

$doc = Get-TomlRows $realToml
$targetRows = @($doc.entry)
$machineRows = @($targetRows | Where-Object { $_.scope -eq 'machine' })
$userRows = @($targetRows | Where-Object { $_.scope -eq 'user' })
Write-Host ("       快照：machine {0} 条 / user {1} 条 / 其它 {2} 条；budget {3}+{4}={5}" -f `
        $machineRows.Count, $userRows.Count, ($targetRows.Count - $machineRows.Count - $userRows.Count), `
        $doc.budget.raw_user_chars, $doc.budget.raw_machine_chars, $doc.budget.effective_chars)

# **脚本自己**从注册表数一遍两个作用域的条目数，与快照对照（不经过 tuoen）。
$localMachine = Get-LocalRows -Raw $machineNow.Raw -Kind $machineNow.Kind
$localUser = Get-LocalRows -Raw $userNow.Raw -Kind $userNow.Kind
Add-DupIndex -Machine $localMachine -User $localUser | Out-Null
Check '快照的 machine 条目数 = 脚本自己数出来的' ($machineRows.Count -eq $localMachine.Count) `
    ("快照 {0} / 脚本 {1}" -f $machineRows.Count, $localMachine.Count)
Check '快照的 user 条目数 = 脚本自己数出来的' ($userRows.Count -eq $localUser.Count) `
    ("快照 {0} / 脚本 {1}" -f $userRows.Count, $localUser.Count)

# 目标快照 = 真快照 + **一条真实存在的目录**（本机 Maven 的 `bin`，它不在任何 `PATH` 上）。
# 这样 `add` 类才有内容可演示，而"新终端里真的生效"也有一个可观察的判据（`where mvn`）。
$mavenBin = 'C:\Dev\Tool\apache-maven-3.9.5\bin'
$mavenOk = Test-Path -LiteralPath (Join-Path $mavenBin 'mvn.cmd')
Check '演示用的目录真的存在（mvn.cmd）' $mavenOk $mavenBin
$targetDir = Join-Path $root 'target'
New-Item -ItemType Directory -Path $targetDir | Out-Null
$targetToml = Join-Path $targetDir 'path.toml'
$nextUserIndex = ($userRows | Measure-Object -Property index -Maximum).Maximum + 1
# **单引号 here-string**：双引号里 `` `a `` 是 BEL、`` ` `` + 空格是转义 —— 上一版把 "add"
# 写成了 `\x07dd`，于是 tomllib 在 870 行报 "Found invalid character"（脚本自己踩的坑）。
$block = @'

# 由验收脚本追加：一条**本机没有**的目录（决策 131 的 add 类要能被真的产生出来）。
[[entry]]
scope = "user"
index = __INDEX__
owner = "unknown"
raw = "__VALUE__"
expanded = "__VALUE__"
quoted = false
exists = "yes"
reparse = "none"
has_vars = false
has_username = false
dup_index = 0
empty = false
reg_type = "sz"
'@
$block = $block.Replace('__INDEX__', "$nextUserIndex").Replace('__VALUE__', ($mavenBin -replace '\\', '\\'))
Copy-Item -LiteralPath $realToml -Destination $targetToml
Add-Content -LiteralPath $targetToml -Value $block -Encoding utf8

$doc2 = Get-TomlRows $targetToml
Check '目标快照能被独立解析，且比真快照多一条' (@($doc2.entry).Count -eq $targetRows.Count + 1) `
    ("{0} → {1}" -f $targetRows.Count, @($doc2.entry).Count)
$targetRows2 = @($doc2.entry)

# ── §2 独立重算六个类，与 `path diff --json` 逐行对照 ────────────────────

Section '2 逐条 diff：脚本自己算一遍，再与产品对照'

$expected = Get-ExpectedDiff -LocalMachine $localMachine -LocalUser $localUser -TargetRows $targetRows2 `
    -CurrentUsername $currentUsername
$expCounts = @{}
foreach ($c in @('keep', 'add', 'remove', 'move', 'fix', 'case-only')) { $expCounts[$c] = 0 }
foreach ($e in $expected) { $expCounts[$e.Class]++ }
Write-Host ("       脚本自己数：keep={0} add={1} remove={2} move={3} fix={4} case-only={5}（共 {6} 条）" -f `
        $expCounts['keep'], $expCounts['add'], $expCounts['remove'], $expCounts['move'], $expCounts['fix'], `
        $expCounts['case-only'], $expected.Count)
# `fix` 的明细：票据的取证报告写的是"10 组重复、7 条失效、11 条用户名依赖"，
# 而**优先级**（决策 127）会让一条既重复又失效的条目只算一次 —— 所以这里必须把明细印出来，
# 否则"20 条 fix"与"7 条失效"看起来就是矛盾的（它们不矛盾，只是分母不同）。
$fixReasons = @{}
foreach ($e in $expected) { if ($e.Class -eq 'fix') { $fixReasons[$e.Reason] = 1 + [int]$fixReasons[$e.Reason] } }
Write-Host ("       fix 明细：" + (($fixReasons.GetEnumerator() | Sort-Object Name | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join '  '))
$dupRows = @($localMachine + $localUser | Where-Object { $_.DupIndex -gt 0 })
$dangRows = @($localMachine + $localUser | Where-Object { -not $_.Empty -and $_.Exists -eq 'no' })
$userRows2 = @($localMachine + $localUser | Where-Object { $_.HasUser })
Write-Host ("       本机独立计数：dup_index>0 {0} 条 / 失效 {1} 条 / 硬编码用户名 {2} 条 / 空条目 {3} 条" -f `
        $dupRows.Count, $dangRows.Count, $userRows2.Count, @($localMachine + $localUser | Where-Object { $_.Empty }).Count)
# 票据的取证报告用的是**组数**与**作用域分布**（"10 组重复""7 条失效""11 条用户名依赖，其中 2 条在机器级"），
# 而实现侧的 `dup_index` 数的是**富余条数**。两个分母都要印出来，否则"18"与"10 组"看起来是矛盾的。
$groups = @{}
foreach ($r in @($localMachine + $localUser)) {
    if ($r.Empty) { continue }
    $groups[$r.Folded] = 1 + [int]$groups[$r.Folded]
}
$dupGroups = @($groups.Values | Where-Object { $_ -gt 1 })
Write-Host ("       重复的组数 {0} 组 / 富余 {1} 条；失效：机器级 {2} 条 + 用户级 {3} 条；用户名依赖：机器级 {4} 条 + 用户级 {5} 条" -f `
        $dupGroups.Count, $dupRows.Count, `
        @($localMachine | Where-Object { -not $_.Empty -and $_.Exists -eq 'no' }).Count, `
        @($localUser | Where-Object { -not $_.Empty -and $_.Exists -eq 'no' }).Count, `
        @($localMachine | Where-Object { $_.HasUser }).Count, `
        @($localUser | Where-Object { $_.HasUser }).Count)

$diff = Invoke-Tuoen -Arguments @('path', 'diff', $targetToml, '--json') -Name 'diff'
Check 'path diff 退出 0' ($diff.Exit -eq 0) "exit=$($diff.Exit)"
$dj = Get-Payload $diff
Check 'path diff 的 --json 能解析' ($null -ne $dj) ''
if ($null -eq $dj) {
    Write-Host $diff.Stderr -ForegroundColor Red
} else {
    $rows = @(Arr $dj 'rows')
    Check 'diff 的行数与脚本自己算的一致' ($rows.Count -eq $expected.Count) `
        ("产品 {0} / 脚本 {1}" -f $rows.Count, $expected.Count)
    foreach ($c in @('keep', 'add', 'remove', 'move', 'fix', 'case-only')) {
        $key = if ($c -eq 'case-only') { 'caseOnly' } else { $c }
        $got = Prop (Prop $dj 'counts') $key
        Check ("counts.{0} 存在且 = 脚本自己算的" -f $key) ($null -ne $got -and [int]$got -eq [int]$expCounts[$c]) `
            ("产品 {0} / 脚本 {1}" -f $got, $expCounts[$c])
    }
    # 逐行对照：`(scope, index)` → 类
    $prodByKey = @{}
    foreach ($r in $rows) {
        $rl = Prop $r 'local'
        $rt = Prop $r 'target'
        $side = if ($null -ne $rl) { "local:$($rl.index)" } elseif ($null -ne $rt) { "target:$($rt.index)" } else { '<none>' }
        $scope = if ($null -ne $rl) { $rl.scope } else { $rt.scope }
        $prodByKey["$scope|$side"] = Prop $r 'class'
    }
    $mismatch = @()
    foreach ($e in $expected) {
        $k = "$($e.Scope)|$($e.Id)"
        $got = if ($prodByKey.ContainsKey($k)) { $prodByKey[$k] } else { '<missing>' }
        if ($got -ne $e.Class) { $mismatch += "$k 脚本=$($e.Class) 产品=$got" }
    }
    Check '每一行的类都与脚本自己算的相同' ($mismatch.Count -eq 0) (($mismatch | Select-Object -First 5) -join ' ; ')

    # 硬不变量：类必须与理由一致（脚本自己按 slug 前缀表算）
    $reasonClass = @{
        'identical' = 'keep'; 'only-in-target' = 'add'; 'only-in-local' = 'remove'; 'position-differs' = 'move'
        'case-differs' = 'case-only'; 'empty-segment' = 'fix'; 'duplicate' = 'fix'; 'dangling' = 'fix'; 'username-hardcoded' = 'fix'
    }
    $bad = @($rows | Where-Object { $reasonClass[[string](Prop $_ 'reason')] -ne (Prop $_ 'class') })
    Check 'class == reason.class()（全量行）' ($bad.Count -eq 0) `
        (($bad | Select-Object -First 3 | ForEach-Object { "$(Prop $_ 'id'):$(Prop $_ 'reason')→$(Prop $_ 'class')" }) -join ' ; ')

    # 每条都要带理由、来源快照的 owner/reparse/has_username
    $noReason = @($rows | Where-Object { -not (Prop $_ 'reason') })
    Check '每条都有 reason' ($noReason.Count -eq 0) "$($noReason.Count) 条没有"
    $noFacts = @($rows | Where-Object {
            $t = Prop $_ 'target'
            $null -ne $t -and (($null -eq (Prop $t 'owner')) -or ($null -eq (Prop $t 'reparse')))
        })
    Check '每条都带来源快照的 owner / reparse / hasUsername' ($noFacts.Count -eq 0) "$($noFacts.Count) 条缺事实"

    # id 的形状：`{scope}:{index}`（本机侧）或 `{scope}:+{index}`（只有目标侧的行）。
    # `+` 是必需的：同下标既可能有一条 remove、又可能有一条 add，两种都用 `user:0`
    # 会让 `--pick user:0` 一次选中两条（想删一条却顺手加了一条）。
    $badId = @($rows | Where-Object { [string](Prop $_ 'id') -notmatch '^(machine|user):\+?\d+$' })
    Check 'id 形状是 {scope}:{index} 或 {scope}:+{index}' ($badId.Count -eq 0) `
        (($badId | Select-Object -First 3 | ForEach-Object { Prop $_ 'id' }) -join ' ; ')
    $dupIds = @(@($rows | ForEach-Object { [string](Prop $_ 'id') }) | Group-Object | Where-Object { $_.Count -gt 1 })
    Check '所有 id 互不相同（--pick 不会一次选中两条）' ($dupIds.Count -eq 0) `
        (($dupIds | Select-Object -First 3 | ForEach-Object { "$($_.Name)×$($_.Count)" }) -join ' ; ')

    # 大小写差异必须单独成类，不许是一个 add + 一个 remove
    $caseRows = @($rows | Where-Object { (Prop $_ 'class') -eq 'case-only' })
    Check 'case-only 行的 reason 是 case-differs' `
        (@($caseRows | Where-Object { (Prop $_ 'reason') -ne 'case-differs' }).Count -eq 0) "$($caseRows.Count) 条"

    # 拼写疑似：**只报告、不纠错**。脚本自己核对每条建议 —— 它必须真的存在于磁盘上，
    # 必须与失效条目的父目录相同，且必须不是同一个名字（否则那就不叫建议了）。
    $danglingRows = @($rows | Where-Object { (Prop $_ 'reason') -eq 'dangling' })
    # 比的是**归并之后**的 dangling 数（决策 127 的优先级会让一条既重复又失效的条目只算一次），
    # 而**不是**原始失效数（那是 7 —— 其中 2 条同时是重复，`C:\Software\tool` 出现 3 次）。
    # 第一版拿原始数比，差一点把"优先级生效"报成"产品少报了两条失效"。
    $expectedDangling = @($expected | Where-Object { $_.Reason -eq 'dangling' }).Count
    Check '失效条目的条数与脚本自己算的（归并后）一致' ($danglingRows.Count -eq $expectedDangling) `
        ("产品 {0} / 脚本 {1}（原始失效 {2} 条，其中 {3} 条同时是重复）" -f `
            $danglingRows.Count, $expectedDangling, $dangRows.Count, ($dangRows.Count - $expectedDangling))
    $suggested = @($rows | Where-Object { $null -ne (Prop $_ 'suggestion') })
    $badSuggest = @()
    foreach ($r in $suggested) {
        $s = [string](Prop $r 'suggestion')
        $v = [string](Prop (Prop $r 'local') 'value')
        $parent = Split-Path -Parent $v
        if (-not (Test-Path -LiteralPath $s -PathType Container)) { $badSuggest += "$s 不存在" }
        elseif ((Split-Path -Parent $s) -ne $parent) { $badSuggest += "$s 的父目录不是 $parent" }
        elseif ((Split-Path -Leaf $s).ToLowerInvariant() -eq (Split-Path -Leaf $v).ToLowerInvariant()) { $badSuggest += "$s 与原文同名" }
    }
    Check '每条拼写建议都真的存在、父目录相同、且不是同一个名字' ($badSuggest.Count -eq 0) (($badSuggest | Select-Object -First 3) -join ' ; ')
    # 本机那个真实的错写（`C:\Software\tool` 在 `C:\Software\tools` 存在时被错写了 3 次，从未生效过）
    Check '抓到了本机真实的错写：C:\Software\tool → C:\Software\tools' `
        (@($suggested | Where-Object { [string](Prop $_ 'suggestion') -eq 'C:\Software\tools' }).Count -ge 1) `
        ("$($suggested.Count) 条建议：" + ((@($suggested | ForEach-Object { Prop $_ 'suggestion' }) | Select-Object -First 4) -join ', '))

    # 只读契约
    Check '--json 成功载荷里没有 message 键' (-not ($diff.Stdout -match '"message"')) ''
    Check '--json 成功载荷是纯 ASCII（中文只进人类输出）' (Test-NoCjk $diff.Stdout) ''
    Check '--json 里没有时间戳（两次必须逐字节相同）' (-not ($diff.Stdout -match 'capturedAt|captured_at|\d{4}-\d{2}-\d{2}T')) ''
    $diff2 = Invoke-Tuoen -Arguments @('path', 'diff', $targetToml, '--json') -Name 'diff2'
    Check '两次 path diff --json 逐字节相同' ($diff.Stdout -ceq $diff2.Stdout) ''
}

# ── §3 `--dry-run` 零副作用 ─────────────────────────────────────────────

Section '3 --dry-run：一个字节都不许写'

$dry = Invoke-Tuoen -Arguments @('path', 'apply', $targetToml, '--only', 'add', '--dry-run', '--json') -Name 'dry'
Check 'path apply --only add --dry-run 退出 0' ($dry.Exit -eq 0) "exit=$($dry.Exit)"
$dryj = Get-Payload $dry
Check '--dry-run 的 --json 能解析' ($null -ne $dryj) ''
if ($null -ne $dryj) {
    Check '--dry-run 的 dryRun = true' ([bool](Prop $dryj 'dryRun') -eq $true) "$(Prop $dryj 'dryRun')"
    Check '--dry-run 的 wrote = false' ([bool](Prop $dryj 'wrote') -eq $false) "$(Prop $dryj 'wrote')"
    $scopes = @(Arr $dryj 'scopes')
    $userScope = @($scopes | Where-Object { (Prop $_ 'scope') -eq 'user' })
    Check '--dry-run 报告里有 user 作用域' ($userScope.Count -eq 1) "$($userScope.Count) 条"
    if ($userScope.Count -eq 1) {
        $applied = @(Arr $userScope[0] 'applied')
        Check '只应用了 1 条（那条 add）' ($applied.Count -eq 1) "$($applied.Count) 条：$((@($applied | ForEach-Object { Prop $_ 'class' })) -join ',')"
        if ($applied.Count -eq 1) {
            Check '被应用的那条是 add 类' ((Prop $applied[0] 'class') -eq 'add') "$(Prop $applied[0] 'class')"
            Check '被应用的值就是演示目录' ((Prop $applied[0] 'value') -eq $mavenBin) "$(Prop $applied[0] 'value')"
        }
        # **脚本自己**按"本机列表 + 追加在末尾"算一遍 after 值
        $expectedAfter = (($localUser | ForEach-Object { $_.Raw.Trim() }) -join ';') + ';' + $mavenBin
        $gotAfter = [string](Prop $userScope[0] 'afterRaw')
        Check 'after 值 = 脚本自己算的（本机列表 + 追加末尾）' ($gotAfter -ceq $expectedAfter) `
            ("产品 {0} 字符 / 脚本 {1} 字符" -f $gotAfter.Length, $expectedAfter.Length)
        $machineApplied = @($scopes | Where-Object { (Prop $_ 'scope') -eq 'machine' -and @(Arr $_ 'applied').Count -gt 0 })
        Check 'machine 作用域没有 applied 行（不静默提权）' ($machineApplied.Count -eq 0) ''
        # 拼写建议**只报告不纠错**：`--only add` 的重建结果里不许出现建议值。
        $afterNow = @(Arr $userScope[0] 'afterEntries')
        Check '--only add 的重建结果里没有出现拼写建议值（只报告不纠错）' `
            (@($afterNow | Where-Object { $_ -eq 'C:\Software\tools' }).Count -eq 0) ''
    }
    Check '--json 成功载荷是纯 ASCII' (Test-NoCjk $dry.Stdout) ''
}
$afterDry = Get-Snapshot
Check '--dry-run 后 HKCU Path 原文逐字未变' ($afterDry.UserRaw -ceq $before.UserRaw) ''
Check '--dry-run 后 HKCU Path 类型未变' ($afterDry.UserKind -ceq $before.UserKind) ''
Check '--dry-run 后 HKCU Path 字符数与哈希未变' (($afterDry.UserChars -eq $before.UserChars) -and ($afterDry.UserSha -ceq $before.UserSha)) `
    "$($before.UserChars)→$($afterDry.UserChars) $($before.UserSha)→$($afterDry.UserSha)"
Check '--dry-run 后 HKLM Path 逐字未变' (($afterDry.MachineRaw -ceq $before.MachineRaw) -and ($afterDry.MachineSha -ceq $before.MachineSha)) ''
Check '--dry-run 后 %LOCALAPPDATA%\tuoen 清单逐项未变' (((Get-TreeListing -Root $realStore -Depth 2) -join '|') -eq ($beforeStore -join '|')) ''

# ── §4 选择性应用：`--only fix` 不许动 remove 类条目的位置 ────────────────

Section '4 选择性应用：只选一类，别的连位置都不动'

$fixDry = Invoke-Tuoen -Arguments @('path', 'apply', $targetToml, '--only', 'fix', '--dry-run', '--json') -Name 'fixdry'
$fixj = Get-Payload $fixDry
Check '--only fix --dry-run 退出 0' ($fixDry.Exit -eq 0) "exit=$($fixDry.Exit)"
if ($null -ne $fixj) {
    $u = @(Arr $fixj 'scopes' | Where-Object { (Prop $_ 'scope') -eq 'user' })
    if ($u.Count -eq 1) {
        $afterEntries = @(Arr $u[0] 'afterEntries')
        $afterList = @($afterEntries | ForEach-Object { [string]$_ })
        $localList = @($localUser | ForEach-Object { $_.Raw.Trim() })
        $removeExpected = @($expected | Where-Object { $_.Scope -eq 'user' -and $_.Class -eq 'remove' })
        $lostRemove = @()
        foreach ($r in $removeExpected) {
            $idx = [int]($r.Id -replace '^local:', '')
            $localRaw = ($localUser | Where-Object { $_.Index -eq $idx }).Raw.Trim()
            # 没被选中的 remove 类条目：值必须还在
            if (-not ($afterList -contains $localRaw)) { $lostRemove += $localRaw }
        }
        # 正确的不变量是**子序列**，不是"同下标"：`--only fix` 会删掉重复与空条目（16 → 7），
        # 删掉之后后面的条目自然往前挪。第一版按"同下标逐字相同"比，于是在**做对了**的时候报红
        # （`第 15 段：'C:\Dev\IDE\VScode\Microsoft VS Code\bin' → '<越界>'`）—— 它把
        # "位置不变"错读成了"下标不变"。这里按"没被 `applied` 碰过的条目依次逐字出现"比：
        # 既查内容，也查它们之间的相对顺序。
        $touched = @(Arr $u[0] 'applied' | Where-Object { $null -ne (Prop $_ 'fromIndex') } |
            ForEach-Object { [int](Prop $_ 'fromIndex') })
        $keptWant = @()
        for ($i = 0; $i -lt $localUser.Count; $i++) {
            if ($touched -contains $i) { continue }
            $keptWant += $localUser[$i].Raw.Trim()
        }
        $seqOk = (($afterList -join ';') -ceq ($keptWant -join ';'))
        $where = ''
        if (-not $seqOk) {
            for ($i = 0; $i -lt [Math]::Max($afterList.Count, $keptWant.Count); $i++) {
                $a = if ($i -lt $afterList.Count) { $afterList[$i] } else { '<越界>' }
                $b = if ($i -lt $keptWant.Count) { $keptWant[$i] } else { '<越界>' }
                if ($a -cne $b) { $where = "第 $i 条：产品 '$a' / 脚本 '$b'"; break }
            }
        }
        Check '只选 fix 时，没被碰过的条目按原顺序逐字留下（子序列，不是同下标）' $seqOk `
            ("after {0} 条 / 期望 {1} 条{2}" -f $afterList.Count, $keptWant.Count, $(if ($where) { "；$where" } else { '' }))
        Check '只选 fix 时，remove 类条目的值一条都没丢（本机 0 条 → 空真，§4d 用合成快照补）' ($lostRemove.Count -eq 0) `
            ("after {0} 条 / 本机 {1} 条 / remove 类 {2} 条" -f $afterList.Count, $localUser.Count, $removeExpected.Count)
        $appliedClasses = @(Arr $u[0] 'applied' | ForEach-Object { Prop $_ 'class' } | Sort-Object -Unique)
        Check '只选 fix 时，applied 里只有 fix 类' (@($appliedClasses | Where-Object { $_ -ne 'fix' }).Count -eq 0) `
            ($appliedClasses -join ',')
    }
    # 机器级：**算出来但不写**（决策 136）。本机机器级有 3 条失效 + 若干重复 + 1 个空条目，
    # 所以 `--only fix` 在机器级一定有选中改动 —— 它们必须进 requiresElevation，且一个字节都不落盘。
    $m = @(Arr $fixj 'scopes' | Where-Object { (Prop $_ 'scope') -eq 'machine' })
    Check '报告里有 machine 作用域（它也要被算出来）' ($m.Count -eq 1) "$($m.Count) 条"
    if ($m.Count -eq 1) {
        $mApplied = @(Arr $m[0] 'applied')
        Check '机器级确实有选中的改动（本机 3 条失效 + 重复 + 空条目）' ($mApplied.Count -gt 0) "$($mApplied.Count) 条"
        Check '机器级标记了 requiresElevation' ([bool](Prop $m[0] 'requiresElevation') -eq $true) "$(Prop $m[0] 'requiresElevation')"
        Check '机器级的 afterRaw 被算出来了（不是空串）' ([string](Prop $m[0] 'afterRaw').Length -gt 0) `
            ("{0} 字符" -f ([string](Prop $m[0] 'afterRaw')).Length)
        $req = @(Arr $fixj 'requiresElevation')
        Check '顶层 requiresElevation 逐条列出机器级的改动' ($req.Count -eq $mApplied.Count) `
            ("顶层 {0} 条 / 机器级 applied {1} 条" -f $req.Count, $mApplied.Count)
    }
}
$afterFixDry = Get-Snapshot
Check '--only fix --dry-run 后 HKCU Path 哈希未变' ($afterFixDry.UserSha -ceq $before.UserSha) ''

# ── §4b 用户名重写：造一份带"旧用户名"的目标快照 ────────────────────────

Section '4b 用户名重写（本机 12 条用户名全是当前用户，所以要造一份带旧名的目标）'

$oldUser = 'tuoen-old-user'
$oldDir = "C:\Users\$oldUser\bin"
$newDir = "C:\Users\$currentUsername\bin"
$oldToml = Join-Path $targetDir 'path-old-user.toml'
$block2 = @'

# 由验收脚本追加：一条硬编码了**旧用户名**的目录（本机没有这条，所以类是 add）。
[[entry]]
scope = "user"
index = __INDEX__
owner = "unknown"
raw = "__VALUE__"
expanded = "__VALUE__"
quoted = false
exists = "no"
reparse = "none"
has_vars = false
has_username = true
dup_index = 0
empty = false
reg_type = "sz"
'@
$block2 = $block2.Replace('__INDEX__', "$nextUserIndex").Replace('__VALUE__', ($oldDir -replace '\\', '\\'))
Copy-Item -LiteralPath $realToml -Destination $oldToml
Add-Content -LiteralPath $oldToml -Value $block2 -Encoding utf8

$od = Invoke-Tuoen -Arguments @('path', 'diff', $oldToml, '--json') -Name 'diffolduser'
$odj = Get-Payload $od
Check '带旧用户名的目标快照：path diff 退出 0' ($od.Exit -eq 0) "exit=$($od.Exit)"
if ($null -ne $odj) {
    $row = @(Arr $odj 'rows' | Where-Object { (Prop (Prop $_ 'target') 'value') -eq $oldDir })
    Check '旧用户名那一行被认出来了（类 add）' ($row.Count -eq 1 -and (Prop $row[0] 'class') -eq 'add') "$($row.Count) 条"
    if ($row.Count -eq 1) {
        Check 'rewriteTo = 当前用户名的那条路径' ((Prop $row[0] 'rewriteTo') -eq $newDir) "$(Prop $row[0] 'rewriteTo')"
    }
    $rw = @(Arr $odj 'rewrites')
    Check 'rewrites 逐条报告了这次重写' `
        ($rw.Count -eq 1 -and (Prop $rw[0] 'from') -eq $oldDir -and (Prop $rw[0] 'to') -eq $newDir) `
        ("{0} 条：{1}" -f $rw.Count, (@($rw | ForEach-Object { "$(Prop $_ 'from')→$(Prop $_ 'to')" }) -join ', '))
}
$odry = Invoke-Tuoen -Arguments @('path', 'apply', $oldToml, '--only', 'add', '--dry-run', '--json') -Name 'dryolduser'
$odryj = Get-Payload $odry
Check '带旧用户名：path apply --only add --dry-run 退出 0' ($odry.Exit -eq 0) "exit=$($odry.Exit)"
if ($null -ne $odryj) {
    $u2 = @(Arr $odryj 'scopes' | Where-Object { (Prop $_ 'scope') -eq 'user' })
    if ($u2.Count -eq 1) {
        $after2 = [string](Prop $u2[0] 'afterRaw')
        Check '插入的值是**重写后**的路径（不是旧用户名那条）' ($after2.Contains($newDir)) ''
        Check '新列表里没有旧用户名那条' (-not $after2.Contains($oldDir)) ''
    }
}
Check '带旧用户名的 --dry-run 之后 HKCU 哈希未变' ((Get-Snapshot).UserSha -ceq $before.UserSha) ''

# ── §4c 选中了却什么都不做的类，必须被说明 ──────────────────────────────

Section '4c noOpClasses：选中的类没产生变换时必须说出来'

$caseDry = Invoke-Tuoen -Arguments @('path', 'apply', $targetToml, '--only', 'case-only', '--dry-run', '--json') -Name 'caseonly'
$cj = Get-Payload $caseDry
Check '--only case-only --dry-run 退出 0' ($caseDry.Exit -eq 0) "exit=$($caseDry.Exit)"
if ($null -ne $cj) {
    $noop = @(Arr $cj 'noOpClasses')
    Check 'noOpClasses 里含 case-only（决策 128：改大小写零收益、纯风险）' ($noop -contains 'case-only') `
        ($(if ($noop.Count) { $noop -join ',' } else { '<空>' }))
    $u3 = @(Arr $cj 'scopes' | Where-Object { (Prop $_ 'scope') -eq 'user' })
    if ($u3.Count -eq 1) {
        Check 'case-only 被选中时 applied 为空（什么都不做）' (@(Arr $u3[0] 'applied').Count -eq 0) `
            "$(@(Arr $u3[0] 'applied').Count) 条"
    }
}

# ── §4d 票据原话的那一种：只选 add，remove 类条目必须原样留着 ────────────

Section '4d 只选 add：remove 类条目必须原样留着（票据原话）'

# 本机真快照里 `remove` 是 **0 条**（目标就是本机），所以"只选 add 时 remove 类不许被删"这条
# 票据点名的判据在真机上是**空真**。这里造一份"目标里少了最后 3 条用户级条目"的快照，
# 把那 3 条真的变成 `remove`，再验一遍。
$dropToml = Join-Path $targetDir 'path-drop.toml'
$dropIdx = & $PY $dropHelper $realToml $dropToml 3 $mavenBin
Check '造出一份"目标少了最后 3 条用户级条目"的快照' ((Test-Path -LiteralPath $dropToml) -and ($LASTEXITCODE -eq 0)) `
    "追加那条的 index=$dropIdx"

$dropDiff = Invoke-Tuoen -Arguments @('path', 'diff', $dropToml, '--json') -Name 'dropdiff'
$dropj = Get-Payload $dropDiff
Check '§4d 快照的 path diff 退出 0' ($dropDiff.Exit -eq 0) "exit=$($dropDiff.Exit)"
$dropRemove = @(Arr $dropj 'rows' | Where-Object { (Prop $_ 'class') -eq 'remove' })
Check '这份快照真的产生了 remove 类（本机真快照里是 0 条 → 空真）' ($dropRemove.Count -eq 3) "$($dropRemove.Count) 条"
Check '这份快照的 counts.remove = 3' ([int](Prop (Prop $dropj 'counts') 'remove') -eq 3) `
    "$(Prop (Prop $dropj 'counts') 'remove')"

$dropDry = Invoke-Tuoen -Arguments @('path', 'apply', $dropToml, '--only', 'add', '--dry-run', '--json') -Name 'dropdry'
$dropdj = Get-Payload $dropDry
Check '只选 add：退出 0' ($dropDry.Exit -eq 0) "exit=$($dropDry.Exit)"
$du = @(Arr $dropdj 'scopes' | Where-Object { (Prop $_ 'scope') -eq 'user' })
Check '只选 add：报告里有 user 作用域' ($du.Count -eq 1) "$($du.Count) 条"
if ($du.Count -eq 1) {
    $dropApplied = @(Arr $du[0] 'applied')
    Check '只选 add：applied 里恰好一条（没有顺手删 remove）' (@($dropApplied).Count -eq 1) "$(@($dropApplied).Count) 条"
    Check '只选 add：那一条是 add 类' (@($dropApplied | Where-Object { (Prop $_ 'class') -ne 'add' }).Count -eq 0) `
        (@($dropApplied | ForEach-Object { Prop $_ 'class' }) -join ',')
    $dropAfter = [string](Prop $du[0] 'afterRaw')
    Check '只选 add：重建结果 = 本机全部条目 + 追加的那条（逐字）' ($dropAfter -ceq $expectedAfter) `
        ("产品 {0} 字符 / 脚本 {1} 字符" -f $dropAfter.Length, $expectedAfter.Length)
    $lost = @()
    foreach ($r in $dropRemove) {
        $v = [string](Prop (Prop $r 'local') 'value')
        if (-not $dropAfter.Contains($v)) { $lost += $v }
    }
    Check '只选 add：3 条 remove 类的值一条都没丢' ($lost.Count -eq 0) (($lost | Select-Object -First 3) -join ' ; ')
}

# ── §4e 人类输出的标签必须与事实一致 ─────────────────────────────────────

Section '4e 人类输出：会写 / 不会写，标签不许说反'

# 这一条是复核期抓出来的既有瑕疵：用户级那一节恒印 `**会写**`，而 `applied` 为空、
# `wrote = false` 时那句话是**错的**（下面 `print_visibility` 那句"计划与现状一致"只是把它
# 盖住了一半）。判据用 `plan.will_write()`，`--json` 的键一个都不许动。
$caseHuman = Invoke-Tuoen -Arguments @('path', 'apply', $targetToml, '--only', 'case-only', '--dry-run') -Name 'caseonlyhuman'
Check 'no-op 计划的人类输出**不许**说"会写"' ($caseHuman.Stdout -notmatch '\*\*会写\*\*') `
    (($caseHuman.Stdout -split "`n" | Where-Object { $_ -match '会写' }) -join ' | ')
Check 'no-op 计划的人类输出说了"计划与现状一致"' ($caseHuman.Stdout -match '现状一致') ''

$fixHuman = Invoke-Tuoen -Arguments @('path', 'apply', $targetToml, '--only', 'fix', '--dry-run') -Name 'fixhuman'
Check '真的会写的那一档仍然印"会写"（别把标签改坏）' ($fixHuman.Stdout -match '\*\*会写\*\*') `
    (($fixHuman.Stdout -split "`n" | Where-Object { $_ -match '会写' }) -join ' | ')
Check '机器级那一行印的是"不会写：机器级要提权"' ($fixHuman.Stdout -match '不会写') ''

# ── §5 空选择与错误路径 ─────────────────────────────────────────────────

Section '5 空选择与错误路径'

$bare = Invoke-Tuoen -Arguments @('path', 'apply', $targetToml, '--json') -Name 'bare'
$bj = Get-Json $bare
Check '不给选择 → 退出 1' ($bare.Exit -eq 1) "exit=$($bare.Exit)"
Check '不给选择 → 错误码是 nothing-selected' ($null -ne $bj -and (Prop (Prop $bj 'error') 'code') -eq 'nothing-selected') `
    ($(if ($null -ne $bj) { Prop (Prop $bj 'error') 'code' } else { '<no json>' }))
Check '不给选择 → 报告里没有 scopes（不是"什么都没做"而是"没执行"）' `
    ($null -ne $bj -and $null -eq (Prop (Prop $bj 'data') 'scopes')) ''

$missing = Invoke-Tuoen -Arguments @('path', 'diff', (Join-Path $root 'nope\path.toml'), '--json') -Name 'missing'
$mj = Get-Json $missing
Check '快照文件不存在 → 退出 1 且错误码是 path-snapshot-io' `
    ($missing.Exit -eq 1 -and $null -ne $mj -and (Prop (Prop $mj 'error') 'code') -eq 'path-snapshot-io') `
    ($(if ($null -ne $mj) { Prop (Prop $mj 'error') 'code' } else { '<no json>' }))

$badToml = Join-Path $root 'bad.toml'
'[[entry]' | Set-Content -LiteralPath $badToml -Encoding utf8
$bad = Invoke-Tuoen -Arguments @('path', 'diff', $badToml, '--json') -Name 'badtoml'
$badj = Get-Json $bad
Check '坏 TOML → 退出 1 且错误码是 path-snapshot-toml' `
    ($bad.Exit -eq 1 -and $null -ne $badj -and (Prop (Prop $badj 'error') 'code') -eq 'path-snapshot-toml') `
    ($(if ($null -ne $badj) { Prop (Prop $badj 'error') 'code' } else { '<no json>' }))

$unknown = Invoke-Tuoen -Arguments @('path', 'apply', $targetToml, '--only', 'nonsense', '--dry-run') -Name 'unknownclass'
Check '--only 给了不认识的类 → 退出码 2（clap 拒绝）' ($unknown.Exit -eq 2) "exit=$($unknown.Exit)"

$help = Invoke-Tuoen -Arguments @('path', 'apply', '--help') -Name 'help'
Check 'path apply --help 里有 --only / --pick / --dry-run' `
    (($help.Stdout -match '--only') -and ($help.Stdout -match '--pick') -and ($help.Stdout -match '--dry-run')) ''

# ── §6 真的写一次，然后原样写回 ─────────────────────────────────────────

Section '6 选择性应用的真实演示（写完立刻还原）'

if ($SkipWrite) {
    Skip '选择性应用的真实演示（真写 + 新终端生效 + 还原）' '-SkipWrite'
} else {
# **还原路径必须先自证能用，再允许写入路径跑。**
# 这里把当前值**原样写回一次**（同字节、同类型、同一个 `RegSetValueExW` + 广播路径），
# 然后断言哈希逐字未变 —— 它证明的是"§6 的还原真的写得进去"，而不是"我们打算还原"。
# 这一条是被真机事故逼出来的：第一版的 `Set-PathValue` 用只读句柄，于是**写进去了、还原不了**，
# `HKCU\Environment\Path` 被留在 808 字符的改动值上（见
# `docs/acceptance/L1-15-path-rebuild-restore-incident.txt`）。
# **一条不能失败的还原不是还原。**
$script:backupPath = Join-Path $env:TEMP '_l115_hkcu_backup_script.txt'
$script:backupKindPath = Join-Path $env:TEMP '_l115_hkcu_backup_script.kind'
[System.IO.File]::WriteAllText($script:backupPath, $before.UserRaw, [System.Text.UTF8Encoding]::new($false))
[System.IO.File]::WriteAllText($script:backupKindPath, $before.UserKind, [System.Text.UTF8Encoding]::new($false))
Write-Host ("  备份（跑之前）：{0} 字符 / {1} / sha16 {2} → {3}" -f `
        $before.UserChars, $before.UserKind, $before.UserSha, $script:backupPath)
Set-PathValue -Scope 'user' -Raw $before.UserRaw -Kind $before.UserKind
$roundTrip = Get-Snapshot
Check '还原机制自检：原样写回一次后哈希与类型逐字相同（§6 的还原路径真的能写）' `
    (($roundTrip.UserSha -ceq $before.UserSha) -and ($roundTrip.UserKind -ceq $before.UserKind)) `
    "$($before.UserSha) → $($roundTrip.UserSha) / $($before.UserKind)"

$written = $null
$realApply = $null
try {
    $realApply = Invoke-Tuoen -Arguments @('path', 'apply', $targetToml, '--only', 'add', '--json') -Name 'apply'
    Check 'path apply --only add 退出 0' ($realApply.Exit -eq 0) "exit=$($realApply.Exit)"
    $rj = Get-Payload $realApply
    Check '真写：wrote = true' ($null -ne $rj -and [bool](Prop $rj 'wrote') -eq $true) "$(Prop $rj 'wrote')"
    $written = Get-PathValue 'user'
    $expectedAfter = (($localUser | ForEach-Object { $_.Raw.Trim() }) -join ';') + ';' + $mavenBin
    Check '写进去的值 = 脚本自己算的期望值（逐字）' ($written.Raw -ceq $expectedAfter) `
        ("{0} 字符 / 期望 {1} 字符" -f $written.Raw.Length, $expectedAfter.Length)
    Check '写进去的类型仍是 sz（值里没有 %）' ($written.Kind -eq 'String') $written.Kind
    Check '新值比旧值正好长 1 + 目录长度' ($written.Raw.Length -eq $before.UserRaw.Length + 1 + $mavenBin.Length) `
        ("{0} → {1}" -f $before.UserRaw.Length, $written.Raw.Length)

    # **新终端**里真的生效了吗（AGENTS.md 规矩 6：只能在重建出来的环境里验）
    $fresh = & pwsh -NoProfile -File (Join-Path $repo 'scripts\fresh-terminal.ps1') 'where mvn' 2>&1
    $freshText = ($fresh -join "`n")
    Check '新终端里 where mvn 找到了 mvn.cmd' ($freshText -match 'apache-maven-3\.9\.5\\bin\\mvn\.cmd') `
        (($fresh | Select-Object -Last 3) -join ' | ')

    $km = Invoke-Tuoen -Arguments @('path', 'diff', $targetToml, '--json') -Name 'diffafter'
    $kj = Get-Payload $km
    if ($null -ne $kj) {
        $addRows = @(Arr $kj 'rows' | Where-Object { (Prop $_ 'class') -eq 'add' })
        Check '写完之后再 diff：add 类归零' ($addRows.Count -eq 0) "$($addRows.Count) 条"
        $keepRows = @(Arr $kj 'rows' | Where-Object { (Prop $_ 'class') -eq 'keep' })
        Check '写完之后再 diff：keep 类增加了（多了一条）' ($keepRows.Count -ge $expCounts['keep'] + 1) `
            ("{0} → {1}" -f $expCounts['keep'], $keepRows.Count)
    }
} finally {
    # 还原：**原文 + 原类型**逐字写回，再广播。这是本脚本唯一一处写注册表。
    Set-PathValue -Scope 'user' -Raw $before.UserRaw -Kind $before.UserKind
    # 还原**必须当场自证**：如果这一步没成，下面那些 `Check` 可能根本跑不到（异常会中断脚本），
    # 而这台机器的 `PATH` 就停在改动值上了 —— 那就必须**大声**说出来，并指出备份在哪。
    $justRestored = Get-Snapshot
    if ($justRestored.UserSha -cne $before.UserSha) {
        Write-Host "!!! 还原失败：HKCU Path 现在是 $($justRestored.UserSha)，跑之前是 $($before.UserSha)" -ForegroundColor Red
        Write-Host "!!! 请手工按备份还原：$script:backupPath（类型在 $script:backupKindPath）" -ForegroundColor Red
    }
}

$restored = Get-Snapshot
Check '还原后 HKCU Path 哈希与跑之前逐字相同' ($restored.UserSha -ceq $before.UserSha) `
    "$($before.UserSha) → $($restored.UserSha)"
Check '还原后 HKCU Path 字符数与类型都相同' (($restored.UserChars -eq $before.UserChars) -and ($restored.UserKind -ceq $before.UserKind)) `
    "$($restored.UserChars) / $($restored.UserKind)"
$fresh2 = & pwsh -NoProfile -File (Join-Path $repo 'scripts\fresh-terminal.ps1') 'where mvn' 2>&1
$fresh2Text = ($fresh2 -join "`n")
Check '还原后新终端里 where mvn 又找不到了' ($fresh2Text -notmatch 'apache-maven') (($fresh2 | Select-Object -Last 2) -join ' | ')
}

# ── §7 长度预算与可见性说明 ─────────────────────────────────────────────

Section '7 长度预算与"请重启终端"'

$plan = Invoke-Tuoen -Arguments @('path', 'apply', $targetToml, '--only', 'add', '--dry-run') -Name 'human'
Check '人类输出里有"重启终端"这句平台限制' ($plan.Stdout -match '重启终端') ''
Check '人类输出里说明了"已经在跑的进程拿不到"' ($plan.Stdout -match '已经在运行的|已经在跑|已在运行') ''
Check '人类输出里有长度预算（8191 悬崖）' ($plan.Stdout -match '8191') ''
Check '人类输出是中文' ([bool]($plan.Stdout -match '[\u4e00-\u9fff]')) ''
if ($null -ne $dryj) {
    $u = @(Arr $dryj 'scopes' | Where-Object { (Prop $_ 'scope') -eq 'user' })
    if ($u.Count -eq 1) {
        $afterRaw = [string](Prop $u[0] 'afterRaw')
        $budget = Prop $dryj 'budget'
        Check '预算的 rawUserChars = 重建后的用户级原文长度' ([int](Prop $budget 'rawUserChars') -eq $afterRaw.Length) `
            ("产品 {0} / 脚本 {1}" -f (Prop $budget 'rawUserChars'), $afterRaw.Length)
        Check '预算的 rawMachineChars = 机器级原文长度（脚本自己从注册表读的）' `
            ([int](Prop $budget 'rawMachineChars') -eq $before.MachineChars) `
            ("产品 {0} / 脚本 {1}" -f (Prop $budget 'rawMachineChars'), $before.MachineChars)
        Check '预算的 effectiveChars = 两者之和（注册表口径的下界）' `
            ([int](Prop $budget 'effectiveChars') -eq ($afterRaw.Length + $before.MachineChars)) `
            ("产品 {0} / 脚本 {1}" -f (Prop $budget 'effectiveChars'), ($afterRaw.Length + $before.MachineChars))
        Check '预算的 cliff 是 8191' ([int](Prop $budget 'cliff') -eq 8191) "$(Prop $budget 'cliff')"
    }
}

# ── §8 收尾核对：这台机器没有被留下任何改动 ─────────────────────────────

Section '8 收尾核对'

$final = Get-Snapshot
Check 'HKCU Path 与跑之前逐字相同' ($final.UserSha -ceq $before.UserSha) "$($before.UserSha) → $($final.UserSha)"
Check 'HKLM Path 与跑之前逐字相同' ($final.MachineSha -ceq $before.MachineSha) ''
Check '%LOCALAPPDATA%\tuoen 清单逐项相同（含 store 两层）' (((Get-TreeListing -Root $realStore -Depth 2) -join '|') -eq ($beforeStore -join '|')) ''
Check '真实的 %APPDATA%\tuoen 清单逐项相同' (((Get-TreeListing -Root (Join-Path $env:APPDATA 'tuoen') -Depth 2) -join '|') -eq ($beforeRealAppData -join '|')) ''
Check '源码里没有带引号的 setx 字面量（硬约束：绝不 setx）' `
    (@(Select-String -Path (Join-Path $repo 'crates\*\src\*.rs') -Pattern '"setx"' -ErrorAction SilentlyContinue).Count -eq 0) ''

if ($SelfTest) {
    Check '自检：这一条必须失败（证明脚本会报失败）' $false
}

# ── §9 摘要 ─────────────────────────────────────────────────────────────

Write-Host ''
$verdict = if ($script:Failed -eq 0) { 'PASS' } else { 'FAIL' }
Write-Host ("SUMMARY checks_passed={0} checks_failed={1} checks_skipped={2} verdict={3} version={4}" -f `
        $script:Passed, $script:Failed, $script:SkipCount, $verdict, $Version) `
    -ForegroundColor $(if ($script:Failed -eq 0) { 'Green' } else { 'Red' })
Write-Host ("        path_machine_chars={0} path_user_chars={1} path_entries={2}" -f `
        $before.MachineChars, $before.UserChars, ($localMachine.Count + $localUser.Count))
Write-Host ("        diff_counts keep={0} add={1} remove={2} move={3} fix={4} case_only={5}" -f `
        $expCounts['keep'], $expCounts['add'], $expCounts['remove'], $expCounts['move'], $expCounts['fix'], $expCounts['case-only'])
Write-Host ("        real_write_restored={0} user_sha_before={1} user_sha_after={2} store_untouched={3} out_root={4}" -f `
        $(if ($SkipWrite) { 'skipped' } else { ($final.UserSha -ceq $before.UserSha) }), $before.UserSha, $final.UserSha, `
        (((Get-TreeListing -Root $realStore -Depth 2) -join '|') -eq ($beforeStore -join '|')), $root)
if ($script:Failed -gt 0) {
    Write-Host '失败项：' -ForegroundColor Red
    $script:Failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
}
if ($script:Failed -gt 0) { exit 1 }
exit 0
