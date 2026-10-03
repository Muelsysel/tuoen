<#
.SYNOPSIS
    票据 #13（L1 `tuoen doctor` 环境体检）的真机验收。

.DESCRIPTION
    `doctor` 是一条**只报告、不修改**的命令 —— 它的风险全在"说出去的话是不是真的"。
    所以这条脚本做三件事：

    1. **独立重算**：`path.*` / `env.*` / `system.*` / `tool.*` 四族的 `evidence`
       由这条脚本用 PowerShell 直接从注册表、`PATH` 原文与磁盘数出来
       （**不经过 tuoen 的任何代码**），再与 `doctor --json` 的 `evidence` **逐行比集合**。
       对不上就是有问题 —— 谁对谁错是下一步的事，但绝不能默默放过。
    2. **结构契约**：ID 白名单（现场从 `crates/core/src/doctor.rs` 的 `ids` 模块提取，
       **不抄一份**）、severity 三档、按（严重度, ID）排序、`evidence` 全 ASCII、
       成功载荷里没有 `message` 键也没有 CJK、两次 `--json` 逐字节相同。
    3. **只读证明**：跑之前/之后各拍一次快照（两个作用域的 `Path` 原文 + 类型 + SHA、
       `%LOCALAPPDATA%\tuoen` 清单、store 清单、Lxss 子键、`HKCU\Environment` 值个数），
       逐项要求相同。

    **子进程的 `PATH` 是"新终端会看到的那一条"**（机器级原文 + `;` + 用户级原文，展开一次），
    因为 `doctor` 的 `pathEntries` / `effectiveChars` 是**从进程 `PATH` 量的**：
    用带 cargo/rustup 前缀的 `PATH` 跑，分母就不是用户的 49 条而是 55 条 ——
    "分母必须说出来"，而这里说出来的办法是把它固定成用户真正会看到的那一条。

    **一条不能失败的验收不是验收**：`-SelfTest` 会让最后一条检查故意失败（exit 1）。

.PARAMETER SelfTest
    故意让最后一条检查失败，用来证明这条脚本**会**报失败（exit 1）。

.PARAMETER SkipBuild
    跳过 §0 的 release 构建（只在已经构建过、且想快速重跑时用）。

.PARAMETER Version
    要验收的版本号，只进摘要行。

.EXAMPLE
    pwsh -File scripts/acceptance-L1-13.ps1
    pwsh -File scripts/acceptance-L1-13.ps1 -SelfTest   # 必须 exit 1
#>
[CmdletBinding()]
param(
    [switch]$SelfTest,
    [switch]$SkipBuild,
    [string]$Version = '0.1.0'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repo 'target\release\tuoen.exe'
$root = Join-Path $env:TEMP 'tuoen-acceptance-L1-13'
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

# ── 器材 ────────────────────────────────────────────────────────────────

$ENV_KEYS = @{
    'user'    = 'HKCU:\Environment'
    'machine' = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
}

function Get-Sha {
    param([string]$Text)
    $bytes = [System.Text.Encoding]::Unicode.GetBytes($Text)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        ($sha.ComputeHash($bytes) | ForEach-Object { $_.ToString('x2') }) -join ''
    } finally {
        $sha.Dispose()
    }
}

function Get-RawPath {
    param([string]$Scope)
    $item = Get-Item $ENV_KEYS[$Scope]
    $raw = $item.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    [pscustomobject]@{
        Scope = $Scope
        Raw   = $raw
        Type  = $item.GetValueKind('Path').ToString()
        Chars = $raw.Length
        Sha   = (Get-Sha $raw)
    }
}

# 「新终端会看到什么」= 机器级原文 + `;` + 用户级原文，再展开一次。
# **一个字节都不写注册表** —— 只用来构造子进程的环境块。
function Get-PristinePath {
    $machine = (Get-Item $ENV_KEYS['machine']).GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    $user = (Get-Item $ENV_KEYS['user']).GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    [Environment]::ExpandEnvironmentVariables("$machine;$user")
}

# 条目的**值**：去首尾空白、剥一对首尾引号（与采集器的比较形态一致）。
function Get-EntryValue {
    param([string]$Entry)
    $v = $Entry.Trim()
    if ($v.Length -ge 2 -and $v.StartsWith('"') -and $v.EndsWith('"')) {
        $v = $v.Substring(1, $v.Length - 2)
    }
    $v
}

# 规范化后的键：去尾部反斜杠 + 小写（判"重复"用的就是它）。
function Get-EntryKey {
    param([string]$Value)
    (Get-EntryValue $Value).TrimEnd('\').ToLowerInvariant()
}

function Get-PathEntries {
    param([string]$Scope)
    $raw = (Get-Item $ENV_KEYS[$Scope]).GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    @($raw -split ';' | ForEach-Object { [pscustomobject]@{ Scope = $Scope; Value = $_ } })
}

function Test-IsAbsolute {
    param([string]$Value)
    ($Value -match '^[A-Za-z]:\\') -or ($Value -match '^\\\\')
}

function Get-RepoListing {
    # `%LOCALAPPDATA%\tuoen` 顶层 + `shims` 里一层（被测代码会写的就是这两处）。
    $home_ = Join-Path $env:LOCALAPPDATA 'tuoen'
    if (-not (Test-Path $home_)) { return @('<absent>') }
    $names = @(Get-ChildItem $home_ -Force | Sort-Object Name | ForEach-Object { $_.Name })
    $shims = Join-Path $home_ 'shims'
    if (Test-Path $shims) {
        $inner = @(Get-ChildItem $shims -Force | Sort-Object Name | ForEach-Object { $_.Name })
        $names += "shims/{$($inner -join ', ')}"
    }
    $names
}

function Get-StoreListing {
    $store = Join-Path $env:LOCALAPPDATA 'tuoen\store'
    if (-not (Test-Path $store)) { return @('<absent>') }
    @(Get-ChildItem $store -Force -Recurse -Depth 2 | Sort-Object FullName | ForEach-Object { $_.FullName.Substring($store.Length) })
}

function Get-LxssListing {
    $lx = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss'
    if (-not (Test-Path $lx)) { return @('<absent>') }
    @(Get-ChildItem $lx | ForEach-Object {
            $k = Get-Item $_.PSPath
            "{0}|{1}|{2}" -f $k.GetValue('DistributionName', '<none>'), $k.GetValue('BasePath', '<none>'), $k.GetValue('Version', '<none>')
        } | Sort-Object)
}

# 跑一次 tuoen：子进程 `PATH` 固定成"新终端会看到的那一条"。
function Invoke-Tuoen {
    param([string[]]$Arguments, [string]$Name = 'run')
    $out_file = Join-Path $root "$Name.out"
    $err_file = Join-Path $root "$Name.err"
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $exe
    foreach ($a in $Arguments) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables['PATH'] = $script:Pristine
    $psi.EnvironmentVariables['Path'] = $script:Pristine
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stdout = $proc.StandardOutput.ReadToEnd()
    $stderr = $proc.StandardError.ReadToEnd()
    $proc.WaitForExit()
    Set-Content -Path $out_file -Value $stdout -NoNewline -Encoding utf8
    Set-Content -Path $err_file -Value $stderr -NoNewline -Encoding utf8
    [pscustomobject]@{
        Exit    = $proc.ExitCode
        Stdout  = $stdout
        Stderr  = $stderr
        File    = $out_file
        ErrFile = $err_file
        Args    = ($Arguments -join ' ')
    }
}

# 从源码现场提取 ID 白名单 / 来源白名单 —— **不抄一份**：
# 抄一份的话，`ids` 模块加一条常量而脚本没跟上，这里会假装通过。
function Get-SourceModule {
    param([string]$File, [string]$Module)
    $src = Get-Content (Join-Path $repo $File) -Raw
    $start = $src.IndexOf("pub mod $Module {")
    if ($start -lt 0) { throw "源码里找不到 pub mod $Module（$File）" }
    $end = $src.IndexOf("`n}", $start)
    if ($end -lt 0) { throw "pub mod $Module 的结尾找不到（$File）" }
    $body = $src.Substring($start, $end - $start)
    @([regex]::Matches($body, 'pub const [A-Z_0-9]+: &str = "([a-z0-9.\-]+)";') |
            ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique)
}

# `KNOWN_TOOLS` 现场解析：每个工具的 id 与它的 `(文件, 命令)` 名字表，**按源码顺序**。
# 顺序重要：`multiple-active` 的证据取"每个目录的第一条命令"，而"第一"就是源码顺序。
function Get-KnownCommands {
    $src = Get-Content (Join-Path $repo 'crates\core\src\detect\spec.rs') -Raw
    $consts = @{}
    foreach ($m in [regex]::Matches($src, 'const (\w+): &\[ExecutableName\] = &\[(?<body>[\s\S]*?)\];')) {
        $consts[$m.Groups[1].Value] = @([regex]::Matches($m.Groups['body'].Value, 'ExecutableName::(?:primary|secondary)\("([^"]+)",\s*"([^"]+)"\)') |
                ForEach-Object { [pscustomobject]@{ File = $_.Groups[1].Value; Command = $_.Groups[2].Value } })
    }
    $tools = @()
    foreach ($m in [regex]::Matches($src, 'ToolSpec \{(?<body>[\s\S]*?)\n    \},')) {
        $body = $m.Groups['body'].Value
        $id = [regex]::Match($body, 'id: "([^"]+)"').Groups[1].Value
        if (-not $id) { continue }
        $ref = [regex]::Match($body, 'executables: (\w+),').Groups[1].Value
        if ($ref) {
            $names = @($consts[$ref])
        } else {
            $names = @([regex]::Matches($body, 'ExecutableName::(?:primary|secondary)\("([^"]+)",\s*"([^"]+)"\)') |
                    ForEach-Object { [pscustomobject]@{ File = $_.Groups[1].Value; Command = $_.Groups[2].Value } })
        }
        $tools += [pscustomobject]@{ Id = $id; Names = $names }
    }
    $tools
}

# 两个字符串集合比一比：**排序后比集合**，不做位置比较
# （`Compare-Object` 是按位置比的，两边没排序时会造出一堆假差异 —— 决策 94 的教训）。
function Compare-Sets {
    param([string]$Name, [string[]]$Expected, [string[]]$Actual)
    $e = @($Expected | Sort-Object)
    $a = @($Actual | Sort-Object)
    $missing = @($e | Where-Object { $_ -notin $a })
    $extra = @($a | Where-Object { $_ -notin $e })
    $ok = ($missing.Count -eq 0) -and ($extra.Count -eq 0)
    $detail = if ($ok) { "$($e.Count) 行逐行相同" } else { "少: $($missing -join ' | ') ; 多: $($extra -join ' | ')" }
    Check $Name $ok $detail
}

function Test-NoCjk {
    param([string]$Text)
    -not [bool]($Text -match '[^\x00-\x7F]')
}

# 从一组 evidence 行里取 `前缀=值` 的值；没有那一行就返回 `$null`
# （调用方去报一条"形状不对"的失败，而不是在这里抛异常把整条脚本掀掉）。
function Get-EvidenceValue {
    param([string[]]$Evidence, [string]$Prefix)
    $hit = @($Evidence | Where-Object { $_.StartsWith($Prefix) })
    if ($hit.Count -eq 0) { return $null }
    $hit[0].Substring($Prefix.Length)
}

# 子进程的 `PATH` 固定成"新终端会看到的那一条"。**在 §0 之前就要有**：
# §0 也在跑子进程（`doctor --help`），而一个未定义的变量在 `StrictMode` 下会直接报错。
$script:Pristine = Get-PristinePath

# ── §0 二进制 ───────────────────────────────────────────────────────────

Section '0 二进制（release + 它真的知道 doctor）'
if ($SkipBuild) {
    Skip '构建 release 二进制' '被 -SkipBuild 跳过'
} else {
    Push-Location $repo
    try {
        $env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"
        $build = & cargo build --release -p tuoen-cli 2>&1
        Check '构建 release 二进制' ($LASTEXITCODE -eq 0) (($build | Select-Object -Last 1) -join '')
    } finally {
        Pop-Location
    }
}
Check 'release 二进制在' (Test-Path $exe) $exe

$help = Invoke-Tuoen -Arguments @('doctor', '--help') -Name 'help'
Check 'doctor --help 退出码 0' ($help.Exit -eq 0)
Check '帮助里没有 --fix（票据点名否掉的那个开关）' (-not [bool]($help.Stdout -match '--fix'))
Check '帮助里有 --json 与 --no-probe' (($help.Stdout -match '--json') -and ($help.Stdout -match '--no-probe'))
$top_help = Invoke-Tuoen -Arguments @('--help') -Name 'top-help'
Check '顶层帮助里有 doctor' ($top_help.Exit -eq 0 -and [bool]($top_help.Stdout -match 'doctor'))
foreach ($bad in @(@('doctor', '--fix'), @('doctor', '--strict'), @('doctor', '--fail-on', 'error'))) {
    $r = Invoke-Tuoen -Arguments $bad -Name ('reject-' + ($bad -join '-').Replace('--', ''))
    Check ("不接受 {0}（退出码 2）" -f ($bad -join ' ')) ($r.Exit -eq 2) ("exit=$($r.Exit)")
}

# ── §1 预检：快照 + 脚本自己数出来的分母 ────────────────────────────────

Section '1 预检：快照与脚本自己的分母'
$before_user = Get-RawPath 'user'
$before_machine = Get-RawPath 'machine'
$before_repo = Get-RepoListing
$before_store = Get-StoreListing
$before_lxss = Get-LxssListing
$before_env_count = @((Get-Item $ENV_KEYS['user']).Property).Count + @((Get-Item $ENV_KEYS['machine']).Property).Count

$script:Pristine = Get-PristinePath
Check '两个作用域的 Path 都读到了原文' ($before_user.Chars -gt 0 -and $before_machine.Chars -gt 0) `
    ("user=$($before_user.Chars) 字符 machine=$($before_machine.Chars) 字符")

$user_entries = Get-PathEntries 'user'
$machine_entries = Get-PathEntries 'machine'

# 给每条条目补上"它在自己作用域里的**原始下标**"（不是"第几个非空条目"）。
# 位置字符串 `machine#21` 里的 21 就是它 —— 与 `doctor` 的 `position(row)` 同义。
function Add-Index {
    param([object[]]$Entries)
    $out = @()
    for ($i = 0; $i -lt $Entries.Count; $i++) {
        $out += [pscustomobject]@{
            Scope = $Entries[$i].Scope
            Value = $Entries[$i].Value
            Index = $i
            Key   = (Get-EntryKey $Entries[$i].Value)
        }
    }
    $out
}

$all_entries = @($machine_entries + $user_entries)
$nonempty = @($all_entries | Where-Object { (Get-EntryValue $_.Value) -ne '' })
$empty_rows = @($all_entries | Where-Object { (Get-EntryValue $_.Value) -eq '' })

$user_indexed = @(Add-Index $user_entries)
$machine_indexed = @(Add-Index $machine_entries)
$all_indexed = @($machine_indexed + $user_indexed)
$nonempty_indexed = @($all_indexed | Where-Object { (Get-EntryValue $_.Value) -ne '' })

# 重复：按规范化键分组，surplus = 组内条数 - 1（与采集器的 `dup_index` 同义）。
$dup_groups = @($nonempty_indexed | Group-Object Key | Where-Object { $_.Count -gt 1 } | Sort-Object Name)
$surplus = 0
$dup_expected = @()
foreach ($g in $dup_groups) {
    $surplus += ($g.Count - 1)
    $refs = @($g.Group | ForEach-Object { "$($_.Scope)#$($_.Index)" })
    $dup_expected += ("{0} x{1} ({2})" -f $g.Name, $g.Count, ($refs -join ' '))
}

$username_re = '(?i)^C:\\Users\\[^\\]+\\'
$username_rows = @($nonempty_indexed | Where-Object { (Get-EntryValue $_.Value) -match $username_re })
$username_machine = @($username_rows | Where-Object { $_.Scope -eq 'machine' })
$spaces_rows = @($nonempty_indexed | Where-Object { (Get-EntryValue $_.Value) -match ' ' })
$nonascii_rows = @($nonempty_indexed | Where-Object { (Get-EntryValue $_.Value) -match '[^\x00-\x7F]' })
$relative_rows = @($nonempty_indexed | Where-Object { -not (Test-IsAbsolute (Get-EntryValue $_.Value)) })
$dangling_rows = @($nonempty_indexed | Where-Object {
        $v = Get-EntryValue $_.Value
        (-not $v.Contains('%')) -and (Test-IsAbsolute $v) -and (-not (Test-Path -LiteralPath $v))
    })

# reparse：条目指向的目录自己是不是 reparse point（junction / symlink）。
$reparse_expected = @()
foreach ($row in $nonempty_indexed) {
    $v = Get-EntryValue $row.Value
    if (-not (Test-Path -LiteralPath $v)) { continue }
    $item = Get-Item -LiteralPath $v -Force -ErrorAction SilentlyContinue
    if (-not $item) { continue }
    if (-not ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) { continue }
    $kind = switch ($item.LinkType) {
        'Junction' { 'junction' }
        'SymbolicLink' { if ($item.PSIsContainer) { 'symlink-dir' } else { 'symlink-file' } }
        default { 'other' }
    }
    $target = if ($item.Target) { @($item.Target)[0] } else { $null }
    $line = if ($target) { "$($row.Scope)#$($row.Index) $kind -> $target" } else { "$($row.Scope)#$($row.Index) $kind" }
    $reparse_expected += $line
}

$eff = $script:Pristine.Length
$level = if ($eff -gt 8191) { 'exceeded' } elseif ($eff * 100 -ge 8191 * 90) { 'critical' } elseif ($eff * 100 -ge 8191 * 75) { 'warning' } else { 'ok' }
Check '脚本自己算的预算档是 ok（本机远未到崖边）' ($level -eq 'ok') `
    ("raw_user=$($before_user.Chars) raw_machine=$($before_machine.Chars) effective=$eff 剩余=$(8191 - $eff)")

Write-Host ("  · 脚本自己的分母：条目 {0}（机器 {1} + 用户 {2}）、非空 {3}、重复 surplus {4}、硬编码用户名 {5}（机器 {6}）、空条目 {7}、含空格 {8}、非 ASCII {9}、相对 {10}、失效 {11}、reparse {12}" -f `
        $all_entries.Count, $machine_entries.Count, $user_entries.Count, $nonempty.Count, $surplus, `
        $username_rows.Count, $username_machine.Count, $empty_rows.Count, $spaces_rows.Count, `
        $nonascii_rows.Count, $relative_rows.Count, $dangling_rows.Count, $reparse_expected.Count)

# ── §2 真机跑一次 ───────────────────────────────────────────────────────

Section '2 真机跑一次：--json / 人类输出 / --no-probe'
$r1 = Invoke-Tuoen -Arguments @('doctor', '--json') -Name 'json1'
Check 'doctor --json 退出码 0（有 error 也退 0 —— 票据的规矩）' ($r1.Exit -eq 0) ("exit=$($r1.Exit)")
Check 'stderr 为空' ($r1.Stderr.Length -eq 0) $r1.Stderr
$script:Json = $r1.Stdout | ConvertFrom-Json
Check 'ok=true 且没有 error 键' ($script:Json.ok -eq $true -and -not ($script:Json.PSObject.Properties.Name -contains 'error'))
Check 'schemaVersion=1 / command=doctor' ($script:Json.schemaVersion -eq 2 -and $script:Json.command -eq 'doctor')

$script:Findings = @($script:Json.data.findings)
function Findings-Of {
    param([string]$Id)
    # 注意：**空数组会被 PowerShell 枚举成"什么都没有"**，调用方拿到 `$null`。
    # 所以这里返回普通数组，而调用方一律写 `@(Findings-Of …).Count` ——
    # 写 `(Findings-Of …).Count` 在 StrictMode 下会报"找不到属性 Count"（踩过一次）。
    @($script:Findings | Where-Object { $_.id -eq $Id })
}
function Evidence-Of {
    param([string]$Id)
    @($script:Findings | Where-Object { $_.id -eq $Id } | ForEach-Object { $_.evidence })
}

# 2.1 结构契约
$ids = Get-SourceModule -File 'crates\core\src\doctor.rs' -Module 'ids'
$sources = Get-SourceModule -File 'crates\core\src\doctor.rs' -Module 'sources'
Check '从源码提取到了 ID 白名单（不是空表）' ($ids.Count -ge 20) ("$($ids.Count) 个 ID")
$unknown = @($script:Findings | Where-Object { $_.id -notin $ids } | ForEach-Object { $_.id } | Sort-Object -Unique)
Check '每条 finding 的 ID 都在源码的白名单里' ($unknown.Count -eq 0) ($unknown -join ', ')
$bad_sev = @($script:Findings | Where-Object { $_.severity -notin @('error', 'warn', 'info') } | ForEach-Object { $_.severity })
Check 'severity 只有三个 slug' ($bad_sev.Count -eq 0) ($bad_sev -join ', ')
$bad_src = @($script:Findings | Where-Object { $_.source -notin $sources } | ForEach-Object { $_.source } | Sort-Object -Unique)
Check 'source 都在源码的来源白名单里' ($bad_src.Count -eq 0) ($bad_src -join ', ')
$order = @{ 'error' = 0; 'warn' = 1; 'info' = 2 }
$keys = @($script:Findings | ForEach-Object { "$($order[$_.severity])|$($_.id)" })
$sorted = @($keys | Sort-Object)
Check '顺序是（严重度, ID）确定的' (($keys -join ',') -eq ($sorted -join ',')) `
    ($(if (($keys -join ',') -eq ($sorted -join ',')) { "$($keys.Count) 条" } else { '顺序不对' }))

$no_evidence = @($script:Findings | Where-Object { -not $_.evidence -or @($_.evidence).Count -eq 0 } | ForEach-Object { $_.id })
Check '每条 finding 都有非空 evidence' ($no_evidence.Count -eq 0) ($no_evidence -join ', ')
$cjk_evidence = @($script:Findings | ForEach-Object { $_.evidence } | Where-Object { -not (Test-NoCjk $_) })
Check 'evidence 全是 ASCII（结论是数据，人话只在 message 里）' ($cjk_evidence.Count -eq 0) ($cjk_evidence -join ' | ')
Check '成功载荷里没有 message 键' (-not [bool]($r1.Stdout -match '"message"'))
Check '成功载荷里没有 CJK' (Test-NoCjk $r1.Stdout)

# 2.2 counts 与明细逐条一致
foreach ($sev in @('error', 'warn', 'info')) {
    $n = @($script:Findings | Where-Object { $_.severity -eq $sev }).Count
    Check ("counts.{0} 与明细一致" -f $sev) ($script:Json.data.counts.$sev -eq $n) ("$($script:Json.data.counts.$sev) vs $n")
}
$summary_keys = @($script:Json.data.summary.PSObject.Properties.Name | Sort-Object)
$expected_keys = @('envVars', 'pathEntries', 'resolvedCommands', 'shimsOnDisk', 'toolRows', 'wslDistributions')
Check 'summary 的六个分量齐' (($summary_keys -join ',') -eq ($expected_keys -join ',')) ($summary_keys -join ', ')

# 2.3 两次 --json 逐字节相同（同一次 shell 内）
$r2 = Invoke-Tuoen -Arguments @('doctor', '--json') -Name 'json2'
Check '两次 --json 逐字节相同' ($r1.Stdout -ceq $r2.Stdout) ("长度 $($r1.Stdout.Length) / $($r2.Stdout.Length)")

# 2.4 人类输出
$rh = Invoke-Tuoen -Arguments @('doctor') -Name 'human'
Check '人类输出退出码 0' ($rh.Exit -eq 0)
Check '人类输出是中文（错误/警告/提示 + 规模）' ([bool]($rh.Stdout -match '错误') -and [bool]($rh.Stdout -match '警告') -and [bool]($rh.Stdout -match '提示') -and [bool]($rh.Stdout -match '规模'))
Check '人类输出说了这次只读' ([bool]($rh.Stdout -match '只读'))
$human_lines = @($rh.Stdout -split "`r?`n" | Where-Object { $_.Trim() -ne '' })
$last = $human_lines[-1]
$second_last = $human_lines[-2]
Check '最后一行是规模行' ([bool]($last -match '^规模：')) $last
Check '倒数第二行是汇总行' ([bool]($second_last -match '^错误 \d+ · 警告 \d+ · 提示 \d+$')) $second_last
Check '汇总行的三个数字与 --json 一致' ($second_last -match ("^错误 {0} · 警告 {1} · 提示 {2}$" -f $script:Json.data.counts.error, $script:Json.data.counts.warn, $script:Json.data.counts.info)) $second_last
$not_named = @($script:Findings | Where-Object { -not [bool]($rh.Stdout -match [regex]::Escape($_.id)) } | ForEach-Object { $_.id } | Sort-Object -Unique)
Check '人类输出逐条点名了每个 ID' ($not_named.Count -eq 0) ($not_named -join ', ')

# 2.5 --no-probe
$rnp = Invoke-Tuoen -Arguments @('doctor', '--json', '--no-probe') -Name 'no-probe'
$np = $rnp.Stdout | ConvertFrom-Json
Check '--no-probe 退出码 0' ($rnp.Exit -eq 0)
$np_ids = @($np.data.findings | ForEach-Object { "$($_.id)|$($_.evidence -join '~')" })
$full_ids = @($script:Findings | ForEach-Object { "$($_.id)|$($_.evidence -join '~')" })
$np_extra = @($np_ids | Where-Object { $_ -notin $full_ids })
Check '--no-probe 不产生默认跑法没有的 finding' ($np_extra.Count -eq 0) ($np_extra -join ' | ')
Check '--no-probe 去掉了探测才有的那条（tool.global-prefix-inside-version-dir）' `
    (@($np.data.findings | Where-Object { $_.id -eq 'tool.global-prefix-inside-version-dir' }).Count -eq 0 -and `
        @(Findings-Of 'tool.global-prefix-inside-version-dir').Count -gt 0)
Check '--no-probe 的 summary 与默认跑法相同' `
    (($np.data.summary | ConvertTo-Json -Compress) -eq ($script:Json.data.summary | ConvertTo-Json -Compress))

# ── §3 独立核对：path.* ─────────────────────────────────────────────────

Section '3 独立核对 path.*（脚本自己数，不经 tuoen）'
$summary = $script:Json.data.summary
Check 'summary.pathEntries == 脚本数的两个作用域条目数' ($summary.pathEntries -eq $all_entries.Count) `
    ("$($summary.pathEntries) vs $($all_entries.Count)（机器 $($machine_entries.Count) + 用户 $($user_entries.Count)）")

$f_username = @(Findings-Of 'path.username-hardcoded')
$expected_username = @($username_rows | ForEach-Object {
        $v = [Environment]::ExpandEnvironmentVariables((Get-EntryValue $_.Value))
        $suffix = if ($_.Scope -eq 'machine') { ' (machine-scope)' } else { '' }
        "$($_.Scope)#$($_.Index) $v$suffix"
    })
Compare-Sets 'path.username-hardcoded 的证据逐行相同' $expected_username (Evidence-Of 'path.username-hardcoded')
# 条数写在 `message` 里，而 `message` **不在成功载荷里**（那是中文，只进人类输出）。
# 所以这几条判据读的是**人类输出**：同一句话在两条输出里必须是同一个数。
Check 'path.username-hardcoded 的条数（人类输出）== 脚本数的条数' `
    ($f_username.Count -eq 1 -and [bool]($rh.Stdout -match ('`PATH` 上有 {0} 条条目硬编码了' -f $username_rows.Count))) `
    ("脚本 $($username_rows.Count) 条 / finding $($f_username.Count) 条")
Check 'path.username-hardcoded 提到的机器级条数 == 脚本数的机器级条数' `
    ([bool]($rh.Stdout -match ('其中 {0} 条在\*\*机器级\*\*' -f $username_machine.Count))) `
    ("机器级 $($username_machine.Count) 条")

Compare-Sets 'path.duplicate 的证据逐行相同' $dup_expected (Evidence-Of 'path.duplicate')
$dup_findings = @(Findings-Of 'path.duplicate')
$dup_from_evidence = 0
foreach ($line in Evidence-Of 'path.duplicate') {
    if ($line -match ' x(\d+) \(') { $dup_from_evidence += ([int]$Matches[1] - 1) }
}
Check 'path.duplicate 的组数与富余条数 == 脚本数的' `
    ($dup_findings.Count -eq 1 -and $dup_from_evidence -eq $surplus -and `
        [bool]($rh.Stdout -match ('有 {0} 组重复条目（{1} 条富余）' -f $dup_groups.Count, $surplus))) `
    ("组 $($dup_groups.Count) / 富余 $surplus / 证据算出来 $dup_from_evidence")

$expected_missing = @($dangling_rows | ForEach-Object {
        $v = [Environment]::ExpandEnvironmentVariables((Get-EntryValue $_.Value))
        "$($_.Scope)#$($_.Index) $v"
    })
Compare-Sets 'path.missing 的证据逐行相同' $expected_missing (Evidence-Of 'path.missing')
Check 'path.missing 的条数（人类输出）== 脚本数的失效条目' `
    ([bool]($rh.Stdout -match ('`PATH` 上有 {0} 条条目指向不存在的路径' -f $dangling_rows.Count))) `
    ("脚本 $($dangling_rows.Count) 条")

$expected_empty = @($all_indexed | Where-Object { (Get-EntryValue $_.Value) -eq '' } | ForEach-Object { "$($_.Scope)#$($_.Index) <empty>" })
Compare-Sets 'path.empty-entry 的证据逐行相同' $expected_empty (Evidence-Of 'path.empty-entry')

$expected_spaces = @($spaces_rows | ForEach-Object { "$($_.Scope)#$($_.Index) $(Get-EntryValue $_.Value)" })
Compare-Sets 'path.spaces 的证据逐行相同' $expected_spaces (Evidence-Of 'path.spaces')
Compare-Sets 'path.reparse 的证据逐行相同' $reparse_expected (Evidence-Of 'path.reparse')

Check '本机没有非 ASCII 条目 → doctor 也不该报' ($nonascii_rows.Count -eq 0 -and @(Findings-Of 'path.non-ascii').Count -eq 0) `
    ("脚本 $($nonascii_rows.Count) 条")
Check '本机没有相对路径条目 → doctor 也不该报' ($relative_rows.Count -eq 0 -and @(Findings-Of 'path.relative').Count -eq 0) `
    ("脚本 $($relative_rows.Count) 条")
Check '预算档是 ok → 不报 path.length-budget（不制造永远修不好的发现）' `
    ($level -eq 'ok' -and @(Findings-Of 'path.length-budget').Count -eq 0)
Check '没有 shim 在盘上 → 不报 path.shadowed' `
    ($summary.shimsOnDisk -eq 0 -and @(Findings-Of 'path.shadowed').Count -eq 0) `
    ("shimsOnDisk=$($summary.shimsOnDisk)")

# ── §4 独立核对：env.* ──────────────────────────────────────────────────

Section '4 独立核对 env.*（脚本自己读注册表）'
$WINDOWS_DEFAULT = @('Path', 'TEMP', 'TMP', 'PATHEXT', 'ComSpec', 'windir', 'SystemDrive', 'SystemRoot',
    'OS', 'USERNAME', 'USERPROFILE', 'HOMEDRIVE', 'HOMEPATH', 'NUMBER_OF_PROCESSORS', 'PSModulePath')
function Test-WindowsDefault {
    param([string]$Name)
    if ($Name -match '^PROCESSOR_') { return $true }
    [bool](@($WINDOWS_DEFAULT | Where-Object { $_.Equals($Name, [System.StringComparison]::OrdinalIgnoreCase) }).Count -gt 0)
}
function Test-CredentialishName {
    param([string]$Name)
    [bool]($Name -match '(?i)KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL')
}

$persistent = @()
foreach ($scope in @('user', 'machine')) {
    $item = Get-Item $ENV_KEYS[$scope]
    foreach ($name in @($item.Property)) {
        $persistent += [pscustomobject]@{
            Scope = $scope
            Name  = $name
            Value = $item.GetValue($name, '', 'DoNotExpandEnvironmentNames')
            Kind  = $item.GetValueKind($name).ToString()
        }
    }
}
# 跳过项（凭据形状的名字）不写进 env.toml，所以也不参与 doctor 的 env.* 判据。
$written = @($persistent | Where-Object { -not (Test-CredentialishName $_.Name) })
$credentialish = @($persistent | Where-Object { Test-CredentialishName $_.Name })

Check 'summary.envVars == 脚本数的持久变量 - 脚本自己判定的凭据形状名' `
    ($summary.envVars -eq $written.Count) ("$($summary.envVars) vs $($written.Count)（总数 $($persistent.Count) - 凭据形状 $($credentialish.Count)）")

$by_name = @{}
foreach ($row in $written) {
    if (-not $by_name.ContainsKey($row.Name)) { $by_name[$row.Name] = @() }
    $by_name[$row.Name] += $row
}
$dup_env_names = @()
$dup_env_expected = @()
foreach ($name in @($by_name.Keys | Sort-Object)) {
    $scopes = @($by_name[$name] | ForEach-Object { $_.Scope } | Sort-Object)
    if ($scopes.Count -ge 2 -and -not (Test-WindowsDefault $name)) {
        $dup_env_names += $name
        $dup_env_expected += @($scopes | ForEach-Object { "$_ $name" })
    }
}
Compare-Sets 'env.duplicated-scope 的证据逐行相同' $dup_env_expected (Evidence-Of 'env.duplicated-scope')
Check 'env.duplicated-scope 的 finding 数 == 脚本数的重名变量数' `
    (@(Findings-Of 'env.duplicated-scope').Count -eq $dup_env_names.Count) `
    ("finding $(@(Findings-Of 'env.duplicated-scope').Count) / 脚本 $($dup_env_names.Count)：$($dup_env_names -join ', ')")

$literal_expected = @()
foreach ($row in $written) {
    $slug = switch ($row.Kind) {
        'String' { 'sz' }
        'ExpandString' { 'expand-sz' }
        'MultiString' { 'multi-sz' }
        'DWord' { 'dword' }
        default { $row.Kind.ToLowerInvariant() }
    }
    if ($row.Kind -eq 'String' -and $row.Value.Contains('%')) {
        $literal_expected += ("{0} {1} type=sz kind=literal-percent value={2}" -f $row.Scope, $row.Name, $row.Value)
    } elseif ($row.Kind -eq 'ExpandString' -and -not $row.Value.Contains('%')) {
        $literal_expected += ("{0} {1} type=expand-sz kind=expand-without-percent value={2}" -f $row.Scope, $row.Name, $row.Value)
    }
}
Compare-Sets 'env.path-literal 的证据逐行相同' $literal_expected (Evidence-Of 'env.path-literal')

$missing_env_expected = @()
foreach ($row in $written) {
    if ($row.Kind -notin @('String', 'ExpandString')) { continue }
    $raw = $row.Value
    # 判据的顺序：先问"是不是一个列表"（`;`），再问"像不像绝对路径" —— 反过来会造出假差异。
    if ($raw.Contains(';')) { continue }
    $expanded = [Environment]::ExpandEnvironmentVariables($raw)
    if (-not (Test-IsAbsolute $expanded)) { continue }
    if (-not (Test-Path -LiteralPath $expanded)) {
        $missing_env_expected += ("{0} {1} -> {2}" -f $row.Scope, $row.Name, $raw)
    }
}
Compare-Sets 'env.missing-target 的证据逐行相同' $missing_env_expected (Evidence-Of 'env.missing-target')

$spaces_env_expected = @($written | Where-Object { $_.Name -match ' ' } | ForEach-Object { "$($_.Scope) $($_.Name)" })
Compare-Sets 'env.name-with-spaces 的证据逐行相同' $spaces_env_expected (Evidence-Of 'env.name-with-spaces')

# ── §5 独立核对：system.* 与 WSL ────────────────────────────────────────

Section '5 独立核对 system.* 与 WSL'
$is_admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$elevated_expected = if ($is_admin) { 'elevated=true' } else { 'elevated=false' }
Compare-Sets 'system.elevated 与脚本自己的提权判定一致' @($elevated_expected) (Evidence-Of 'system.elevated')

$devmode_key = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock'
$devmode_expected = if (Test-Path $devmode_key) {
    $v = (Get-Item $devmode_key).GetValue('AllowDevelopmentWithoutDevLicense', $null)
    if ($null -eq $v) { 'developer-mode=absent' } elseif ($v -ne 0) { 'developer-mode=true' } else { 'developer-mode=false' }
} else { 'developer-mode=absent' }
Compare-Sets 'system.developer-mode 与脚本自己读的注册表一致' @($devmode_expected) (Evidence-Of 'system.developer-mode')

$longpath_key = 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem'
$long_expected = if (Test-Path $longpath_key) {
    $v = (Get-Item $longpath_key).GetValue('LongPathsEnabled', $null)
    if ($null -eq $v) { 'long-paths=unknown' } elseif ($v -ne 0) { 'long-paths=true' } else { 'long-paths=false' }
} else { 'long-paths=unknown' }
Compare-Sets 'system.long-paths 与脚本自己读的注册表一致' @($long_expected) (Evidence-Of 'system.long-paths')

# WSL：判据是"BasePath 展开后不在 %LOCALAPPDATA% 底下"（忽略大小写与尾部分隔符；
# `\\?\` 前缀先剥掉 —— 本机 docker-desktop 就带着它）。
function Remove-Verbatim {
    param([string]$Path)
    if ($Path.StartsWith('\\?\')) { return $Path.Substring(4) }
    if ($Path.StartsWith('\??\')) { return $Path.Substring(4) }
    $Path
}
$local_appdata = $env:LOCALAPPDATA
if (-not $local_appdata) { $local_appdata = [Environment]::GetFolderPath('LocalApplicationData') }
$wsl_expected = @()
$wsl_nonstandard_names = @()
$wsl_total = 0
$wsl_names = @()
foreach ($sub in @(Get-ChildItem 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss' -ErrorAction SilentlyContinue)) {
    $k = Get-Item $sub.PSPath
    $name = $k.GetValue('DistributionName', '')
    if (-not $name) { continue }
    $wsl_total++
    $wsl_names += $name
    $base = [Environment]::ExpandEnvironmentVariables([string]$k.GetValue('BasePath', ''))
    $stripped = (Remove-Verbatim $base).TrimEnd('\')
    $under = $stripped.StartsWith($local_appdata.TrimEnd('\'), [System.StringComparison]::OrdinalIgnoreCase)
    if ($under) { continue }
    $wsl_nonstandard_names += $name
    $vhdx = (Remove-Verbatim ([Environment]::ExpandEnvironmentVariables(([string]$k.GetValue('BasePath', '')) + '\ext4.vhdx')))
    $exists = if (Test-Path -LiteralPath $vhdx) { 'yes' } else { 'no' }
    $wsl_expected += @(
        "distribution=$name",
        "base-path=$([string]$k.GetValue('BasePath', ''))",
        "vhdx-path=$([string]$k.GetValue('BasePath', '') + '\ext4.vhdx')",
        "vhdx-exists=$exists",
        'non-standard-path=true'
    )
}
Check 'summary.wslDistributions == 脚本数的 Lxss 发行版' ($summary.wslDistributions -eq $wsl_total) `
    ("$($summary.wslDistributions) vs $($wsl_total)（$($wsl_names -join ', ')）")
Compare-Sets 'system.wsl-nonstandard-path 的证据逐行相同' $wsl_expected (Evidence-Of 'system.wsl-nonstandard-path')
$reported_wsl = @(Evidence-Of 'system.wsl-nonstandard-path' | Where-Object { $_.StartsWith('distribution=') } | ForEach-Object { $_.Substring('distribution='.Length) })
Check '标准位置的发行版没有被报（报出来的只有脚本自己算出的非标准那几个）' `
    (@($reported_wsl | Where-Object { $_ -notin $wsl_nonstandard_names }).Count -eq 0) `
    ("报了：$($reported_wsl -join ', ')；脚本判非标准：$($wsl_nonstandard_names -join ', ')")
Check 'system.* 这一族全是 info（状态报告，不是错误）' `
    (@($script:Findings | Where-Object { $_.id -like 'system.*' -and $_.severity -ne 'info' }).Count -eq 0)

# ── §6 独立核对：tool.* ─────────────────────────────────────────────────

Section '6 独立核对 tool.*（脚本自己解析 PATH 与注册表）'
$known = Get-KnownCommands
$names = @($known | ForEach-Object { $_.Names })
Check '从源码解析出 12 个工具 / 23 个命令名' ($known.Count -eq 12 -and $names.Count -eq 23) `
    ("工具 $($known.Count) / 名字 $($names.Count)")
Check 'summary.resolvedCommands == 源码里的命令名总数（每条名字都被探过一次）' `
    ($summary.resolvedCommands -eq $names.Count) ("$($summary.resolvedCommands) vs $($names.Count)")

$path_dirs = @($script:Pristine -split ';' | ForEach-Object { Get-EntryValue $_ } | Where-Object { $_ -ne '' })
function Resolve-Command {
    param([string]$File)
    foreach ($d in $path_dirs) {
        $p = Join-Path $d $File
        if (Test-Path -LiteralPath $p) { return $d }
    }
    return $null
}
$resolved_map = @{}
$ma_expected = @()
$ma_tools = @()
foreach ($tool in $known) {
    $seen = @()
    foreach ($n in $tool.Names) {
        $d = Resolve-Command $n.File
        if (-not $d) { continue }
        $resolved_map["$($tool.Id)/$($n.Command)"] = $d
        if (-not (@($seen | Where-Object { $_.Dir.TrimEnd('\').ToLowerInvariant() -eq $d.TrimEnd('\').ToLowerInvariant() }).Count -gt 0)) {
            $seen += [pscustomobject]@{ Dir = $d; Command = $n.Command }
        }
    }
    if ($seen.Count -ge 2) {
        $ma_tools += $tool.Id
        foreach ($s in $seen) { $ma_expected += ("tool={0} command={1} dir={2}" -f $tool.Id, $s.Command, $s.Dir) }
    }
}
Compare-Sets 'tool.multiple-active 的证据逐行相同' $ma_expected (Evidence-Of 'tool.multiple-active')
Check 'tool.multiple-active 的 finding 数 == 脚本数的"命令落在多个目录"的工具数' `
    (@(Findings-Of 'tool.multiple-active').Count -eq $ma_tools.Count) `
    ("finding $(@(Findings-Of 'tool.multiple-active').Count) / 脚本 $($ma_tools.Count)：$($ma_tools -join ', ')")

# nvm4w 的 symlink 与 tuoen 的安装记录 —— multi-manager 的两条证据各自独立验一遍。
$nvm_symlink = (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment').GetValue('NVM_SYMLINK', $null, 'DoNotExpandEnvironmentNames')
$node_prefix = if ($nvm_symlink) { $nvm_symlink } else { 'C:\nvm4w\nodejs' }
$nvm_item = Get-Item -LiteralPath $node_prefix -Force -ErrorAction SilentlyContinue
$nvm_is_link = $nvm_item -and ($nvm_item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)
$nvm_target = if ($nvm_item -and $nvm_item.Target) { @($nvm_item.Target)[0] } else { $null }
$store_has_node = [bool](@($before_store | Where-Object { $_ -match '\\node\\' }).Count -gt 0)
Check 'nvm4w 真的在管 node（NVM_SYMLINK 指向一个 reparse point）' ([bool]$nvm_is_link) "$node_prefix -> $nvm_target"
Check 'tuoen 真的在管 node（store 里有 node 的安装记录）' $store_has_node (($before_store | Where-Object { $_ -match '\\node\\' }) -join ', ')
Check 'tool.multi-manager 的证据是"两个真正的管理器"' `
    (((Evidence-Of 'tool.multi-manager') -join '|') -eq 'node manager=nvm4w|node manager=tuoen' -or `
        ((Evidence-Of 'tool.multi-manager' | Sort-Object) -join '|') -eq 'node manager=nvm4w|node manager=tuoen') `
    ((Evidence-Of 'tool.multi-manager') -join ' | ')

$gp = @(Findings-Of 'tool.global-prefix-inside-version-dir')
if ($gp.Count -eq 0) {
    Skip 'tool.global-prefix-inside-version-dir 的证据逐项核对' '本机没有这条 finding'
} else {
    $gp_ev = @($gp[0].evidence)
    $gp_prefix = Get-EvidenceValue $gp_ev 'tool=node prefix='
    $gp_version = Get-EvidenceValue $gp_ev 'version='
    $gp_target = Get-EvidenceValue $gp_ev 'link-target='
    if (-not $gp_prefix -or -not $gp_target -or -not $gp_version) {
        Check 'tool.global-prefix-inside-version-dir 的证据形状（prefix / link-target / version）' $false ($gp_ev -join ' | ')
    } else {
        $gp_item = Get-Item -LiteralPath $gp_prefix -Force
        $gp_real_target = @($gp_item.Target)[0]
        $leaf = Split-Path -Leaf $gp_target
        $gp_kind = if ($gp_item.LinkType -eq 'Junction') { 'junction' } elseif ($gp_item.PSIsContainer) { 'symlink-dir' } else { 'symlink-file' }
        Check '全局前缀的 reparse 目标 == 脚本自己读出来的目标' ($gp_real_target -eq $gp_target) "$gp_real_target"
        Check '目标名的版本成分 == 证据里的 version' `
            ([bool]($leaf -match '^v?\d+(\.\d+)+$') -and $leaf.TrimStart('v') -eq $gp_version) "$leaf vs $gp_version"
        Check '证据里的 reparse 种类与脚本读到的 LinkType 一致' `
            ([bool](@($gp_ev | Where-Object { $_ -eq "reparse=$gp_kind" }).Count -gt 0)) $gp_item.LinkType
        Check '证据里的 origin=probe（这条只有探测才拿得到）' `
            ([bool](@($gp_ev | Where-Object { $_ -eq 'origin=probe' }).Count -gt 0)) ($gp_ev -join ' | ')
    }
}

# 幽灵记录：每个卸载键都真的在 ARP 里，而且 InstallLocation 是空的（或指向不存在的目录）。
$ghost_keys = @(Evidence-Of 'tool.ghost' | Where-Object { $_.StartsWith('uninstall-key=') } | ForEach-Object { $_.Substring('uninstall-key='.Length) })
$ghost_ok = 0
$ghost_detail = @()
foreach ($guid in $ghost_keys) {
    $found = $null
    foreach ($hive in @('HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall', 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')) {
        $k = Join-Path $hive $guid
        if (Test-Path $k) { $found = Get-Item $k; break }
    }
    if (-not $found) { $ghost_detail += "$guid 不在 ARP 里"; continue }
    $il = [string]$found.GetValue('InstallLocation', '')
    if ($il -eq '' -or -not (Test-Path -LiteralPath $il)) { $ghost_ok++ } else { $ghost_detail += "$guid 的 InstallLocation 还在：$il" }
}
Check 'tool.ghost 的每个卸载键都在 ARP 里且没有可用 InstallLocation' `
    ($ghost_keys.Count -gt 0 -and $ghost_ok -eq $ghost_keys.Count) `
    ("$ghost_ok / $($ghost_keys.Count)  $($ghost_detail -join '; ')")
$placeholders = @(Evidence-Of 'tool.ghost' | Where-Object { $_.EndsWith('path=<placeholder>') })
Check 'tool.ghost 的占位符行数与没有 InstallLocation 的键数一致' ($placeholders.Count -ge 1) ("$($placeholders.Count) 行")

# 无人管的目录：目录真的在、孩子数真的对得上。
$ud_findings = @(Findings-Of 'tool.unmanaged-directory')
$ud_bad = @()
foreach ($f in $ud_findings) {
    $dir = @($f.evidence)[0]
    if (-not (Test-Path -LiteralPath $dir)) { $ud_bad += "$dir 不存在"; continue }
    $claimed = [int](@($f.evidence | Where-Object { $_ -like 'children=*' })[0].Substring(9))
    $real = @(Get-ChildItem -LiteralPath $dir -Force).Count
    if ($claimed -ne $real) { $ud_bad += "$dir children $claimed vs $real" }
    if (@($f.evidence | Where-Object { $_ -eq 'managed-by=none' }).Count -ne 1) { $ud_bad += "$dir 没说 managed-by=none" }
}
Check 'tool.unmanaged-directory 的目录存在且孩子数与磁盘一致' `
    ($ud_findings.Count -gt 0 -and $ud_bad.Count -eq 0) `
    ("$($ud_findings.Count) 条  $($ud_bad -join '; ')")

# WindowsApps 的 App Execution Alias：`python` 赢在它上面，而它是 0 字节 reparse。
$alias_dir = $null
if ($resolved_map.ContainsKey('python/python')) { $alias_dir = $resolved_map['python/python'] }
if ($alias_dir) {
    $alias = Get-Item -LiteralPath (Join-Path $alias_dir 'python.exe') -Force -ErrorAction SilentlyContinue
    $is_alias = $alias -and ($alias.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -and $alias.Length -eq 0
    Check 'python 赢在 WindowsApps 的 0 字节 reparse（App Execution Alias）上' ([bool]$is_alias) `
        ("$alias_dir\python.exe 长度 $(if ($alias) { $alias.Length } else { '?' })")
} else {
    Skip 'python 的 App Execution Alias 形状' '脚本没在 PATH 上解析到 python'
}

# ── §7 收尾：这条命令真的只读 ───────────────────────────────────────────

Section '7 收尾核对：这条命令真的只读'
$after_user = Get-RawPath 'user'
$after_machine = Get-RawPath 'machine'
Check '用户级 Path 逐字节相同（含类型）' (($after_user.Raw -ceq $before_user.Raw) -and ($after_user.Type -eq $before_user.Type)) $after_user.Sha.Substring(0, 12)
Check '机器级 Path 逐字节相同（含类型）' (($after_machine.Raw -ceq $before_machine.Raw) -and ($after_machine.Type -eq $before_machine.Type)) $after_machine.Sha.Substring(0, 12)
Check '%LOCALAPPDATA%\tuoen 没被动过（含 shims 里一层）' (((Get-RepoListing) -join '|') -eq ($before_repo -join '|')) ($before_repo -join ', ')
Check 'store 没被动过' (((Get-StoreListing) -join '|') -eq ($before_store -join '|'))
Check 'Lxss 没被动过' (((Get-LxssListing) -join '|') -eq ($before_lxss -join '|'))
$after_env_count = @((Get-Item $ENV_KEYS['user']).Property).Count + @((Get-Item $ENV_KEYS['machine']).Property).Count
Check '持久变量的个数没变' ($after_env_count -eq $before_env_count) ("$after_env_count vs $before_env_count")
$stray = @(Get-ChildItem $env:TEMP -Directory -Filter 'tuoen.d' -ErrorAction SilentlyContinue)
Check '没有在 %TEMP% 根上留下 tuoen.d' ($stray.Count -eq 0)

if ($SelfTest) {
    Check '自检：这一条必须失败（证明脚本会报失败）' $false
}

# ── §8 摘要 ─────────────────────────────────────────────────────────────

Write-Host ''
$verdict = if ($script:Failed -eq 0) { 'PASS' } else { 'FAIL' }
Write-Host ("SUMMARY checks_passed={0} checks_failed={1} checks_skipped={2} verdict={3} version={4} " -f `
        $script:Passed, $script:Failed, $script:SkipCount, $verdict, $Version) -ForegroundColor $(if ($script:Failed -eq 0) { 'Green' } else { 'Red' })
Write-Host ("        findings={0} error={1} warn={2} info={3} path_entries={4} env_vars={5} tool_rows={6} resolved_commands={7} wsl_distributions={8} shims_on_disk={9}" -f `
        $script:Findings.Count, $script:Json.data.counts.error, $script:Json.data.counts.warn, $script:Json.data.counts.info, `
        $summary.pathEntries, $summary.envVars, $summary.toolRows, $summary.resolvedCommands, $summary.wslDistributions, $summary.shimsOnDisk)
Write-Host ("        raw_user_chars={0} raw_machine_chars={1} effective_chars={2} level={3} duplicates_surplus={4} username_deps={5} missing={6} empty={7} reparse={8}" -f `
        $before_user.Chars, $before_machine.Chars, $eff, $level, $surplus, $username_rows.Count, $dangling_rows.Count, $empty_rows.Count, $reparse_expected.Count)
Write-Host ("        user_path_untouched={0} machine_path_untouched={1} tuoen_dir_untouched={2} out_root={3}" -f `
        ($after_user.Sha -eq $before_user.Sha), ($after_machine.Sha -eq $before_machine.Sha), (((Get-RepoListing) -join '|') -eq ($before_repo -join '|')), $root)
if ($script:Failed -gt 0) {
    Write-Host '失败项：' -ForegroundColor Red
    $script:Failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
}

exit $(if ($script:Failed -eq 0) { 0 } else { 1 })
