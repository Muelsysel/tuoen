<#
.SYNOPSIS
    验收脚本：票据 #27 —— L2 真机验收（全局包：根、两个来源、重定向、真装、幂等）。

.DESCRIPTION
    票据 #18 写的是**证据**，不是功能：在作者本机上跑完整流程，把每一步的真实输出落盘，
    并在每一步之后核对"这台机器一个字节都没变"。

    它只写两种东西：
      ① `%TEMP%\tuoen-acceptance-L2-27\` 下的工件；
      ② §7 里**故意**写一个用户级环境变量 `TUOEN_L1_ACCEPT_PROBE` —— 空计划上的
         "计划 vs 真的执行"对比是恒真的，所以必须让计划非空。写之前先备份整个
         `HKCU\Environment`、**自证还原能用**（AGENTS.md 规矩 7），跑完删掉并逐字核对回原样。

.PARAMETER SelfTest
    故意让最后一条检查失败（exit 1）—— 一条不能失败的验收不是验收。

.PARAMETER SkipBuild
    跳过 §0 的 release 构建。

.EXAMPLE
    pwsh -File scripts/acceptance-L1-18.ps1
    pwsh -File scripts/acceptance-L1-18.ps1 -SelfTest   # 必须 exit 1
#>
[CmdletBinding()]
param(
    [switch]$SelfTest,
    [switch]$SkipBuild,
    [switch]$WithNetwork
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repo 'target\release\tuoen.exe'
$root = Join-Path $env:TEMP 'tuoen-acceptance-L2-27'
Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $root | Out-Null

$script:Passed = 0
$script:Failed = 0
$script:SkipCount = 0
$script:Failures = @()
$script:Timings = New-Object System.Collections.Generic.List[string]
$script:Notes = New-Object System.Collections.Generic.List[string]

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

function Note {
    param([string]$Text)
    $script:Notes.Add($Text)
    Write-Host "  [note] $Text" -ForegroundColor DarkGray
}

# ── 器材：注册表（原文 + 类型，**从不展开**） ───────────────────────────

$ENV_KEYS = @{
    'user'    = 'HKCU:\Environment'
    'machine' = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
}

# 原文 + 类型。**必须 `DoNotExpandEnvironmentNames`**：展开是不可逆的信息损失（AGENTS.md 规矩 2）。
function Get-PathValue {
    param([string]$Scope)
    $item = Get-Item $ENV_KEYS[$Scope]
    [pscustomobject]@{
        Raw  = [string]$item.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        Kind = [string]$item.GetValueKind('Path')
    }
}

function Get-Sha16 {
    param([string]$Text)
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
    $hash = [System.Security.Cryptography.SHA256]::Create().ComputeHash($bytes)
    (([System.BitConverter]::ToString($hash)) -replace '-', '').ToLowerInvariant().Substring(0, 16)
}

function Get-FileSha {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return '<absent>' }
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-Snapshot {
    $u = Get-PathValue 'user'
    $m = Get-PathValue 'machine'
    [pscustomobject]@{
        UserRaw      = $u.Raw
        UserKind     = $u.Kind
        UserChars    = $u.Raw.Length
        UserSha      = Get-Sha16 $u.Raw
        MachineRaw   = $m.Raw
        MachineKind  = $m.Kind
        MachineChars = $m.Raw.Length
        MachineSha   = Get-Sha16 $m.Raw
    }
}

# 决策 154 的红线：**绝不碰第三方版本管理器的符号链接 / 环境变量 / settings.txt**。
# 真机上的等价物是这一份状态：两个作用域里 `NVM_HOME`/`NVM_SYMLINK` 的原文与类型、
# `settings.txt` 的哈希、以及那个 junction 的**指向**。跑前跑后逐字相同才算证据。
function Get-NvmState {
    $vals = [ordered]@{}
    foreach ($scope in @('user', 'machine')) {
        $item = Get-Item $ENV_KEYS[$scope]
        foreach ($n in @('NVM_HOME', 'NVM_SYMLINK')) {
            $raw = [string]$item.GetValue($n, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $kind = if ($item.GetValueNames() -contains $n) { [string]$item.GetValueKind($n) } else { '<absent>' }
            $vals["$scope/$n"] = "$kind|$raw"
        }
    }
    # 只为了**找到那个文件**才展开（报告与比较用的都是原文）。
    $nvmHome = [string](Get-Item $ENV_KEYS['user']).GetValue('NVM_HOME', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $settingsSha = '<no NVM_HOME>'
    $settingsBytes = -1
    $settingsPath = ''
    if ($nvmHome) {
        $settingsPath = Join-Path ([Environment]::ExpandEnvironmentVariables($nvmHome)) 'settings.txt'
        if (Test-Path -LiteralPath $settingsPath) {
            $settingsSha = Get-FileSha $settingsPath
            $settingsBytes = (Get-Item -LiteralPath $settingsPath).Length
        } else {
            $settingsSha = '<absent>'
        }
    }
    $symlink = [string](Get-Item $ENV_KEYS['user']).GetValue('NVM_SYMLINK', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $junction = '<no NVM_SYMLINK>'
    if ($symlink) {
        $p = [Environment]::ExpandEnvironmentVariables($symlink)
        if (Test-Path -LiteralPath $p) {
            $junction = [string](Get-Item -LiteralPath $p -Force).LinkTarget
            if (-not $junction) { $junction = '<not-a-link>' }
        } else {
            $junction = '<absent>'
        }
    }
    [pscustomobject]@{
        Env          = (($vals.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ' | ')
        SettingsSha  = $settingsSha
        SettingsPath = $settingsPath
        SettingsSize = $settingsBytes
        Junction     = $junction
    }
}

# 全环境块快照（值名 + 类型 + 原文，按名字排序）——"一个字节都没变"的最强形态。
function Get-EnvBlockSnapshot {
    $item = Get-Item 'HKCU:\Environment'
    @(@($item.GetValueNames()) | Sort-Object | ForEach-Object {
            "$_|$([string]$item.GetValueKind($_))|$([string]$item.GetValue($_, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames))"
        })
}

# 写 / 删一个用户级环境变量。**必须走可写子键句柄**：PowerShell provider 的句柄是只读的，
# 那正是 #15 真机验收里"写进去了、还原不了"那次事故的根因（决策 149 / AGENTS.md 规矩 7）。
function Set-UserEnvValue {
    param([string]$Name, [string]$Value)
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if ($null -eq $key) { throw '打不开 HKCU\Environment 的可写子键' }
    try { $key.SetValue($Name, $Value, [Microsoft.Win32.RegistryValueKind]::String) } finally { $key.Dispose() }
}

function Remove-UserEnvValue {
    param([string]$Name)
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if ($null -eq $key) { throw '打不开 HKCU\Environment 的可写子键' }
    try { $key.DeleteValue($Name, $false) } finally { $key.Dispose() }
}

# 整个 `HKCU\Environment` 的备份（JSON：类型 + 名字 + 原文）与**还原**。
# 规矩 7：改状态的脚本必须"先自证还原能用，再允许写入跑"。
function Save-EnvBackup {
    param([string]$Path)
    $item = Get-Item 'HKCU:\Environment'
    $rows = @(@($item.GetValueNames()) | Sort-Object | ForEach-Object {
            [pscustomobject]@{
                Kind  = [string]$item.GetValueKind($_)
                Name  = $_
                Value = [string]$item.GetValue($_, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            }
        })
    $rows | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $Path -Encoding utf8
    $rows
}

function Restore-EnvBackup {
    param([string]$Path)
    $rows = @(Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json)
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    if ($null -eq $key) { throw '打不开 HKCU\Environment 的可写子键' }
    try {
        foreach ($r in $rows) {
            $key.SetValue([string]$r.Name, [string]$r.Value, [Microsoft.Win32.RegistryValueKind]([string]$r.Kind))
        }
    } finally { $key.Dispose() }
    $rows.Count
}

# "一个全新终端会看到什么"：从**注册表**读这个变量（不展开）塞进一个 cmd 子进程，再 `echo %NAME%`。
# 环境块只在 `CreateProcess` 时复制，所以**在当前这个 shell 里再 echo 一次证明不了任何事**；
# 必须先 `Remove` 掉继承来的同名变量，否则测到的是本脚本这份旧环境。
function Invoke-FreshEnvProbe {
    param([string]$Name)
    $userRaw = [string](Get-Item 'HKCU:\Environment').GetValue($Name, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $machineRaw = [string](Get-Item $ENV_KEYS['machine']).GetValue($Name, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $env:ComSpec
    $psi.Arguments = '/c echo %' + $Name + '%'
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables.Remove($Name)
    if ($machineRaw) { $psi.EnvironmentVariables[$Name] = [Environment]::ExpandEnvironmentVariables($machineRaw) }
    if ($userRaw) { $psi.EnvironmentVariables[$Name] = [Environment]::ExpandEnvironmentVariables($userRaw) }
    $proc = [System.Diagnostics.Process]::Start($psi)
    $out = $proc.StandardOutput.ReadToEnd()
    $proc.WaitForExit()
    $out.Trim()
}

# 一个目录树的清单（相对路径 + 大小）。`DirectoryInfo` **没有** `Length` 属性，
# 而 StrictMode 下访问不存在的属性是终止错误 —— 必须按"是不是目录"分开取。
function Get-TreeListing {
    param([string]$Root, [int]$Depth = 2)
    if (-not (Test-Path -LiteralPath $Root)) { return @('<absent>') }
    @(Get-ChildItem -LiteralPath $Root -Force -Recurse -Depth $Depth |
            Sort-Object FullName | ForEach-Object {
                $size = if ($_.PSIsContainer) { 'dir' } else { $_.Length }
                "$($_.FullName.Substring($Root.Length))|$size"
            })
}

# ── 器材：跑进程（带计时） ──────────────────────────────────────────────

function Invoke-Tuoen {
    param([string[]]$Arguments, [string]$Label = '')
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $exe
    foreach ($a in $Arguments) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    # 子进程的 PATH = **新终端口径**（从注册表重建），而不是本脚本继承来的那份：
    # 否则"本机侧"会被 cargo / PowerShell 注入的条目污染（两个分母必须说出来）。
    $psi.EnvironmentVariables['PATH'] = $script:Pristine
    $psi.EnvironmentVariables['Path'] = $script:Pristine
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stdout = $proc.StandardOutput.ReadToEnd()
    $stderr = $proc.StandardError.ReadToEnd()
    $proc.WaitForExit()
    $sw.Stop()
    if ($Label) { $script:Timings.Add(("{0}|{1}" -f $Label, $sw.ElapsedMilliseconds)) }
    [pscustomobject]@{
        Exit   = $proc.ExitCode
        Stdout = $stdout
        Stderr = $stderr
        Ms     = $sw.ElapsedMilliseconds
    }
}

function Invoke-Program {
    param([string]$File, [string[]]$Arguments = @())
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $File
    foreach ($a in $Arguments) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stdout = $proc.StandardOutput.ReadToEnd()
    $stderr = $proc.StandardError.ReadToEnd()
    $proc.WaitForExit()
    $sw.Stop()
    [pscustomobject]@{ Exit = $proc.ExitCode; Stdout = $stdout; Stderr = $stderr; Ms = $sw.ElapsedMilliseconds }
}

# ── 器材：JSON 断言助手 ─────────────────────────────────────────────────

function Get-Json {
    param([object]$Result)
    if (-not $Result.Stdout.Trim()) { return $null }
    try { return ($Result.Stdout | ConvertFrom-Json) } catch { return $null }
}

# 逐成员取名字：**`$obj.PSObject.Properties.Name` 在空对象上会炸**（#16 的教训）。
function Prop {
    param([object]$Obj, [string]$Name)
    if ($null -eq $Obj) { return $null }
    $names = @($Obj.PSObject.Properties | ForEach-Object { $_.Name })
    if ($names -contains $Name) { $Obj.$Name } else { $null }
}

function PropNames {
    param([object]$Obj)
    if ($null -eq $Obj) { return @() }
    @($Obj.PSObject.Properties | ForEach-Object { $_.Name })
}

# 数一个"可能不在"的数组：`@($null).Count` 是 **1**，所以一律先滤 `$null`。
function Arr {
    param([object]$Obj, [string]$Name)
    @(Prop $Obj $Name | Where-Object { $null -ne $_ })
}

function Find {
    param([object]$Rows, [string]$Field, [string]$Value)
    $hit = @(@($Rows) | Where-Object { (Prop $_ $Field) -eq $Value })
    if ($hit.Count -eq 0) { return $null }
    $hit[0]
}

function Get-Payload {
    param([object]$Result)
    $j = Get-Json $Result
    if ($null -eq $j) { return $null }
    Prop $j 'data'
}

function Test-NoCjk {
    param([string]$Text)
    -not ($Text -match '[\u4e00-\u9fff\u3000-\u303f\uff00-\uffef]')
}

function Get-WithoutTimestamp {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return '<absent>' }
    ((Get-Content -LiteralPath $Path -Raw) -split "`n" |
            Where-Object { $_ -notmatch '^\s*captured_at\s*=' }) -join "`n"
}

# ── 独立解析：`tuoen.d/` 用 Python 的 `tomllib` ───────────────────────────

$PY = Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\python.exe'
if (-not (Test-Path -LiteralPath $PY)) { throw "找不到真实 Python：$PY（不接受 WindowsApps 的别名）" }

$helper = Join-Path $root '_l227.py'
@'
import json, os, sys, tomllib

d = sys.argv[1]

def load(name):
    p = os.path.join(d, name)
    if not os.path.exists(p):
        return None
    with open(p, "rb") as fh:
        return tomllib.load(fh)

sch = load("schema.toml")
glb = load("globals.toml")
env = load("env.toml")

rows = (glb or {}).get("global", [])

out = {
    "files": sorted(n for n in os.listdir(d) if n.endswith(".toml")),
    "sections": list((sch or {}).get("sections", [])),
    "schema_version": (sch or {}).get("schema_version"),
    "globals": {
        "rows": len(rows),
        "keys": sorted({k for r in rows for k in r}),
        "package_keys": sorted({k for r in rows for q in r.get("packages", []) for k in q}),
        "sources": sorted({str(r.get("source")) for r in rows}),
        "tools": sorted({str(r.get("tool")) for r in rows}),
        # 工具行自己的两格：prefix 与"这个前缀是不是落在版本目录里"（决策 173 的四支判据）。
        "tool_rows": [
            {
                "tool": r.get("tool"),
                "source": r.get("source"),
                "prefix": r.get("prefix"),
                "inside": r.get("prefix_inside_version_dir"),
                "packages": len(r.get("packages", [])),
            }
            for r in rows
        ],
        "entries": [
            {
                "tool": r.get("tool"),
                "tool_version": r.get("tool_version"),
                "source": r.get("source"),
                "name": q.get("name"),
                "version": q.get("version"),
                "bin_names": q.get("bin_names"),
            }
            for r in rows
            for q in r.get("packages", [])
        ],
    },
    "env": {"rows": len((env or {}).get("var", []))},
}
print(json.dumps(out, ensure_ascii=False))
'@ | Set-Content -Path $helper -Encoding utf8 -NoNewline


function Get-Globals {
    param([string]$Dir)
    $empty = [pscustomobject]@{
        files    = @()
        sections = @()
        globals  = [pscustomobject]@{ rows = -1; keys = @(); sources = @(); tools = @(); entries = @() }
        env      = [pscustomobject]@{ rows = -1 }
    }
    if (-not (Test-Path -LiteralPath $Dir)) { return $empty }
    $json = & $PY $helper $Dir
    if ($LASTEXITCODE -ne 0) { return $empty }
    $parsed = $null
    try { $parsed = $json | ConvertFrom-Json } catch { return $empty }
    if ($null -eq $parsed) { return $empty }
    $parsed
}

# `npm` 是 `.cmd`，`Process.Start` 起不动它（CVE-2024-27980 之后 `.cmd` 不能被 spawn）——
# 脚本自己量机器的时候走 `cmd /d /c`。这与产品怎么构造载荷无关，是我们自己的仪器。
function Invoke-Shell {
    param([string]$Command, [string]$Label = '')
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $out = (& cmd.exe /d /c $Command 2>&1 | Out-String)
    $code = $LASTEXITCODE
    $sw.Stop()
    if ($Label) { $script:Timings.Add(("{0}|{1}" -f $Label, $sw.ElapsedMilliseconds)) }
    [pscustomobject]@{ Exit = $code; Stdout = $out; Ms = $sw.ElapsedMilliseconds }
}

# 从一份 `globals.toml` 里切出**一个包**，重组成一份能直接喂给 `restore` 的最小快照：
# 文件头 + 那个工具的那一行 + 那一个 `[[global.packages]]` 块。
# 只动文本、不解析再重排 —— 这样写回的快照与产品自己写出来的**逐字同形**。
function New-GlobalsSnapshot {
    param([string]$Source, [string]$Tool, [string]$Name, [string]$Dest)
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) { return $false }
    $raw = Get-Content -LiteralPath $Source -Raw
    # 先按 `[[global]]` 切工具块（文件头跟着第一块走）。
    $toolBlocks = @(($raw -split "(?m)^(?=\[\[global\]\])") | Where-Object {
            $_ -match ("(?m)^tool = `"" + [regex]::Escape($Tool) + "`"\s*$") })
    if ($toolBlocks.Count -ne 1) { return $false }
    $blk = $toolBlocks[0]
    # 块内再按 `[[global.packages]]` 切包 —— 每个包块**自带**它那一行表头。
    $pkgBlocks = @(($blk -split "(?m)^(?=\[\[global\.packages\]\])") | Where-Object {
            $_ -match ("(?m)^name = `"" + [regex]::Escape($Name) + "`"\s*$") })
    if ($pkgBlocks.Count -ne 1) { return $false }
    $cut = $blk.IndexOf('[[global.packages]]')
    if ($cut -lt 0) { return $false }
    Set-Content -LiteralPath $Dest -Value ($blk.Substring(0, $cut) + $pkgBlocks[0]) -Encoding utf8 -NoNewline
    $true
}


Section '0. 构建'

$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"

if ($SkipBuild) {
    # `-SkipBuild` 用的是**现在躺在那里**的那个 release 二进制 —— 它可能比源码旧。
    # 实测踩过一次：debug 构建里已经有 `source` 键，而 release 还是旧的、没有 ——
    # 于是 §3 报了一条"产品少了 source 键"的假 FAIL。所以把前提变成一条检查。
    $exeTime = (Get-Item -LiteralPath $exe -ErrorAction SilentlyContinue).LastWriteTime
    if ($null -eq $exeTime) {
        Skip 'release 构建' '-SkipBuild 且 release 二进制还不存在'
    } else {
        $newest = @(
            @(Get-ChildItem -Recurse -Include *.rs -Path (Join-Path $repo 'crates') -ErrorAction SilentlyContinue)
            @(Get-Item -LiteralPath (Join-Path $repo 'Cargo.toml') -ErrorAction SilentlyContinue)
        ) | Sort-Object LastWriteTime -Descending | Select-Object -First 1
        Check 'release 二进制不比最新源码旧（-SkipBuild 的前提）' `
            ($exeTime -ge $newest.LastWriteTime) `
            "exe=$($exeTime.ToString('HH:mm:ss')) 最新源码=$($newest.LastWriteTime.ToString('HH:mm:ss')) ($($newest.Name))"
    }
} else {
    Push-Location $repo
    $build = & cargo build --release -p tuoen-cli 2>&1
    $buildExit = $LASTEXITCODE
    Pop-Location
    Check 'cargo build --release -p tuoen-cli' ($buildExit -eq 0) "exit=$buildExit"
    if ($buildExit -ne 0) { $build | Select-Object -Last 15 | ForEach-Object { Write-Host "    $_" } }
}
if (-not (Test-Path -LiteralPath $exe)) { throw "找不到 $exe" }

# ── 1. 前置事实 + 备份 + 自证还原能用 ───────────────────────────────────

Section '1. 前置事实（跑之前的那份真相）'

$m = Get-PathValue 'machine'
$u = Get-PathValue 'user'
$script:Pristine = ($m.Raw + ';' + $u.Raw)
Check '机器级 PATH 可读' ($m.Raw.Length -gt 0) "chars=$($m.Raw.Length) kind=$($m.Kind)"
Check '用户级 PATH 可读' ($u.Raw.Length -gt 0) "chars=$($u.Raw.Length) kind=$($u.Kind)"

$script:Before = Get-Snapshot
$script:NvmBefore = Get-NvmState
$script:EnvBefore = @(Get-EnvBlockSnapshot)
Note "跑前 HKCU\Environment 共 $($script:EnvBefore.Count) 个值；用户级 Path $($script:Before.UserChars) 字符 sha16 $($script:Before.UserSha)"

# 决策 27 的红线基线：这三个变量**现在不在注册表里**（重定向只进子进程环境）。
foreach ($n in @('NPM_CONFIG_PREFIX', 'PYTHONUSERBASE', 'PIP_USER')) {
    $inUser = @($script:EnvBefore | Where-Object { $_ -like "$n|*" }).Count
    $inMachine = @(@((Get-Item $ENV_KEYS['machine']).GetValueNames()) | Where-Object { $_ -eq $n }).Count
    Check "跑前注册表里没有 $n" (($inUser -eq 0) -and ($inMachine -eq 0)) "user=$inUser machine=$inMachine"
}

# 光看注册表不够：这三个变量只要在**当前进程环境**里存在，机器侧的根就跟着变 ——
# 实测：设上 NPM_CONFIG_PREFIX 之后，npm 那 7 行整个消失（`npm config get prefix` 会读它，
# 于是机器侧的前缀指向那个目录，根不存在 ⇒ 一行都不出），只剩 pip 3 行。
# 那样 §2/§3 的期望值会全部失真，所以跑之前必须钉住"它们不在这个进程里"。
foreach ($n in @('NPM_CONFIG_PREFIX', 'PYTHONUSERBASE', 'PIP_USER')) {
    # 注意：`Get-Item Env:\X` 在变量不存在时返回 $null，`.Value` 在 StrictMode 下会让**整个脚本消失**（不是报 FAIL）。
    $v = [string][System.Environment]::GetEnvironmentVariable($n)
    Check "跑之前当前进程环境里没有 $n" ([string]::IsNullOrEmpty($v)) "value=$v"
}

# 机器侧的真相（决策 154：nvm4w 的地盘我们一个字都不改）。
$script:MachinePrefix = (& cmd.exe /d /c 'npm config get prefix' 2>&1 | Out-String).Trim()
Check '机器侧 npm prefix 可读' ($script:MachinePrefix.Length -gt 0) "prefix=$script:MachinePrefix"
$script:NodeVersion = (& node --version 2>&1 | Out-String).Trim()
Check 'node 版本可读' ($script:NodeVersion -match '^v\d') "node=$script:NodeVersion"
$script:TuoenGlobals = Join-Path $env:LOCALAPPDATA 'tuoen\globals'
$script:NpmTree = Join-Path (Join-Path $script:TuoenGlobals 'npm') $script:NodeVersion
Note "tuoen 的 npm 根预期在 $script:NpmTree（从 node -v 自己算的，不是抄产品的）"

# 备份 + **自证还原能用**（AGENTS.md 规矩 7）：把当前值原样写回一次，逐字核对。
$script:Backup = Join-Path $root 'hkcu-environment.json'
$backupRows = @(Save-EnvBackup $script:Backup)
Check 'HKCU\Environment 备份成功' ($backupRows.Count -gt 0) "values=$($backupRows.Count)"
$proofBefore = @(Get-EnvBlockSnapshot)
$null = Restore-EnvBackup $script:Backup
$proofAfter = @(Get-EnvBlockSnapshot)
Check '自证还原：原样写回之后逐字未变' ((($proofBefore -join "`n") -eq ($proofAfter -join "`n")) -and ($proofBefore.Count -gt 0)) "values=$($proofBefore.Count)"

# ── 2. `tuoen globals list`（只读） ─────────────────────────────────────

Section '2. `tuoen globals list`（只读，两个来源）'

$gl = Invoke-Tuoen @('globals', 'list', '--json') 'globals list'
Check 'globals list --json 退出 0' ($gl.Exit -eq 0) "exit=$($gl.Exit)"
$gj = Get-Payload $gl
Check '--json 有 data 信封' ($null -ne $gj) "keys=$(PropNames $gj)"
Check '--json 里没有 CJK（稳定不本地化）' (Test-NoCjk $gl.Stdout) ''

$roots = @(Arr $gj 'roots')
Check 'roots 里两个工具都在（空的根也要出）' ($roots.Count -eq 2) "count=$($roots.Count)"
foreach ($t in @('npm', 'pip')) {
    $r = Find $roots 'tool' $t
    Check "roots 里有 $t 的根" ($null -ne $r) "tool=$t"
    if ($r) {
        Check "roots[$t].root 非空" ([bool](Prop $r 'root')) "root=$(Prop $r 'root')"
        Check "roots[$t].source = tuoen" ((Prop $r 'source') -eq 'tuoen') "source=$(Prop $r 'source')"
    }
}
$npmRootRow = Find $roots 'tool' 'npm'
if ($npmRootRow) {
    Check 'npm 的根 = %LOCALAPPDATA%\tuoen\globals\npm\<node -v>（我自己算的）' `
        ((Prop $npmRootRow 'root') -eq $script:NpmTree) "product=$(Prop $npmRootRow 'root') mine=$script:NpmTree"
}

# 机器侧的数字：**脚本自己数一遍**，两条独立来源对上了才算证据（决策 94 的第四次）。
$npmRaw = Invoke-Shell 'npm ls -g --json --depth=0 --offline' 'npm ls -g'
$myNpm = @()
try { $myNpm = @((($npmRaw.Stdout | ConvertFrom-Json).dependencies.PSObject.Properties)) } catch { $myNpm = @() }
Check '我自己数出机器侧 npm 包' ($myNpm.Count -gt 0) "count=$($myNpm.Count)"
$pipRaw = Invoke-Program 'pip' @('list', '--format=json', '--disable-pip-version-check')
$myPip = @()
try { $myPip = @($pipRaw.Stdout | ConvertFrom-Json) } catch { $myPip = @() }
Check '我自己数出机器侧 pip 包' ($myPip.Count -gt 0) "count=$($myPip.Count)"

$pk = @(Arr $gj 'packages')
$mNpm = @($pk | Where-Object { (Prop $_ 'tool') -eq 'npm' -and (Prop $_ 'source') -eq 'machine' })
$mPip = @($pk | Where-Object { (Prop $_ 'tool') -eq 'pip' -and (Prop $_ 'source') -eq 'machine' })
$tNpm = @($pk | Where-Object { (Prop $_ 'tool') -eq 'npm' -and (Prop $_ 'source') -eq 'tuoen' })
$tPip = @($pk | Where-Object { (Prop $_ 'tool') -eq 'pip' -and (Prop $_ 'source') -eq 'tuoen' })
Check "机器侧 npm 行数 = 我自己数的 $($myNpm.Count)" ($mNpm.Count -eq $myNpm.Count) "product=$($mNpm.Count) mine=$($myNpm.Count)"
Check "机器侧 pip 行数 = 我自己数的 $($myPip.Count)" ($mPip.Count -eq $myPip.Count) "product=$($mPip.Count) mine=$($myPip.Count)"
Check '每一行都有 source' (@($pk | Where-Object { -not (Prop $_ 'source') }).Count -eq 0) "rows=$($pk.Count)"
# 票据冻结的 `packages` 行是 tool / name / version / source（`binNames` 拿不到时**整键消失**）——
# `toolVersion` 不在里面：它是"哪一份运行时"的答案，而根的行里已经由路径说清了。
Check '每一行都有 tool/name/version/source 四个键' (@($pk | Where-Object { -not (Prop $_ 'tool') -or -not (Prop $_ 'name') -or -not (Prop $_ 'version') -or -not (Prop $_ 'source') }).Count -eq 0) "rows=$($pk.Count)"

# `binNames` 是"这个包提供了哪些命令"：拿到就是非空表，拿不到**整键消失**（`[]` 会被读成"它一个命令都没有"）。
# `pnpm` 的 bin 映射是四个名字（pn / pnpm / pnpx / pnx）—— 我自己独立量过。
$pnpmMachine = Find $mNpm 'name' 'pnpm'
Check '机器侧 pnpm 的 binNames 是四个命令' (@(Arr $pnpmMachine 'binNames').Count -eq 4) "bins=$(@(Arr $pnpmMachine 'binNames') -join ',')"
Note "tuoen 侧现在：npm $($tNpm.Count) 个、pip $($tPip.Count) 个（这一节只读，装包在 §5）"

$glHuman = Invoke-Tuoen @('globals', 'list') 'globals list 人类输出'
Check 'globals list（人类输出）退出 0' ($glHuman.Exit -eq 0) "exit=$($glHuman.Exit)"
Set-Content -LiteralPath (Join-Path $root 'globals-list-human.txt') -Value $glHuman.Stdout -Encoding utf8
Check '人类输出说得出 tuoen 的根在哪' ($glHuman.Stdout -match 'globals') ''
Check '人类输出是中文' ($glHuman.Stdout -match '[\u4e00-\u9fff]') ''

# ── 3. `capture --only globals`（只读） ─────────────────────────────────

Section '3. `tuoen capture --only globals`（真机，只读）'

$capDir = Join-Path $root 'capture-1'
$cap = Invoke-Tuoen @('capture', '--only', 'globals', '--out', $capDir) 'capture globals'
Check 'capture --only globals 退出 0' ($cap.Exit -eq 0) "exit=$($cap.Exit)"
$g1 = Get-Globals $capDir
$gFiles = @(Arr $g1 'files')
Check 'globals.toml 被写出来了' ($gFiles -contains 'globals.toml') "files=$($gFiles -join ',')"
$gKeys = @(Prop (Prop $g1 'globals') 'keys')
Check 'globals.toml 的行里有 source 键（加性、永远出键）' ($gKeys -contains 'source') "keys=$($gKeys -join ',')"
$gEntries = @(Prop (Prop $g1 'globals') 'entries')
$gSources = @(Prop (Prop $g1 'globals') 'sources')
Check 'globals.toml 只有 machine 来源（tuoen 的根还是空的）' (($gSources -join ',') -eq 'machine') "sources=$($gSources -join ',')"
$gMachine = @($gEntries | Where-Object { $_.source -eq 'machine' })
$gNpm = @($gMachine | Where-Object { $_.tool -eq 'npm' })
Check "globals.toml 的 npm 行数 = globals list 的 $($mNpm.Count)" ($gNpm.Count -eq $mNpm.Count) "capture=$($gNpm.Count) list=$($mNpm.Count)"
Check "globals.toml 的 pip 行数 = globals list 的 $($mPip.Count)" (@($gMachine | Where-Object { $_.tool -eq 'pip' }).Count -eq $mPip.Count) ''
Note "capture 出来的行键：$($gKeys -join ',')"

# 决策 173 的四支判据**不是恒真** —— 真机上这两条正好相反：
#   npm 的 prefix `C:\nvm4w\nodejs` 是指向 `…\nvm\v24.19.0` 的符号链接 ⇒ 解析后落在版本目录里 ⇒ true
#   pip 的 prefix 以 `Python312` 结尾 —— 含字母，不是版本段 ⇒ false
$gToolRows = @(Prop (Prop $g1 'globals') 'tool_rows')
$gRowNpm = @($gToolRows | Where-Object { $_.tool -eq 'npm' })
$gRowPip = @($gToolRows | Where-Object { $_.tool -eq 'pip' })
Check 'globals.toml 里 npm 那行 prefix_inside_version_dir = true' `
    ($gRowNpm.Count -eq 1 -and $gRowNpm[0].inside -eq $true) `
    "rows=$($gRowNpm.Count) inside=$(if ($gRowNpm.Count -eq 1) { $gRowNpm[0].inside } else { 'n/a' })"
Check 'globals.toml 里 pip 那行 prefix_inside_version_dir = false' `
    ($gRowPip.Count -eq 1 -and $gRowPip[0].inside -eq $false) `
    "rows=$($gRowPip.Count) inside=$(if ($gRowPip.Count -eq 1) { $gRowPip[0].inside } else { 'n/a' })"
Check 'npm 那行的 prefix = 机器自己的 npm prefix' `
    ($gRowNpm.Count -eq 1 -and $gRowNpm[0].prefix -eq $script:MachinePrefix) `
    "capture=$(if ($gRowNpm.Count -eq 1) { $gRowNpm[0].prefix } else { 'n/a' }) machine=$script:MachinePrefix"
# pip 的 prefix 用**另一条实现**独立推一遍：取 `pip --version` 里 ` from ` 之后那条路径，
# 切掉 `Lib\site-packages` 及其之后（产品的规则也一样，但这里是脚本自己算的）。
$pvLine = (& pip --version 2>&1 | Out-String).Trim()
$pvMatch = [regex]::Match($pvLine, ' from (.+?)\\Lib\\site-packages')
$expectPipPrefix = if ($pvMatch.Success) { $pvMatch.Groups[1].Value } else { '' }
Check '从 pip --version 能独立反推出 pip 的 prefix' ($expectPipPrefix.Length -gt 0) "line=$pvLine"
Check 'pip 那行的 prefix = 反推出来的那个目录' `
    ($gRowPip.Count -eq 1 -and $gRowPip[0].prefix -eq $expectPipPrefix) `
    "capture=$(if ($gRowPip.Count -eq 1) { $gRowPip[0].prefix } else { 'n/a' }) expect=$expectPipPrefix"

# ── 4. `shell` 的重定向（#26：只进子进程环境） ──────────────────────────

Section '4. `tuoen shell` 的重定向（绝不写注册表）'

# **`shell` 是项目级命令** —— 真机实测：不在项目里跑，它说
# "这个目录里没有 `tuoen.toml`（找的是 …）"、退出码 1。所以先造一个临时项目。
# `APPDATA` 指到临时家目录（信任/配置状态不落进真家目录），而 `LOCALAPPDATA` **保持真的** ——
# store 里的 node 24.19.0 要能找到（这是 L1-14 验收脚本用过的同一套切法）。
$proj = Join-Path $root 'proj-shell'
New-Item -ItemType Directory -Path $proj -Force | Out-Null
$fakeAppData = Join-Path $root 'appdata'
New-Item -ItemType Directory -Path $fakeAppData -Force | Out-Null
[System.IO.File]::WriteAllText((Join-Path $proj 'tuoen.toml'),
    "[project]`nname = `"acceptance-l2-27`"`n`n[tools]`nnode = `"24`"`n",
    (New-Object System.Text.UTF8Encoding $false))

function Invoke-InProject {
    param([string[]]$Arguments, [string]$Label = '')
    $outFile = Join-Path $root ('shell-' + ($Label -replace '[^\w]', '-') + '.out')
    $errFile = $outFile -replace '\.out$', '.err'
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $exe
    foreach ($a in $Arguments) { $psi.ArgumentList.Add($a) }
    $psi.WorkingDirectory = $proj
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables['PATH'] = $script:Pristine
    $psi.EnvironmentVariables['Path'] = $script:Pristine
    $psi.EnvironmentVariables['APPDATA'] = $fakeAppData
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stdout = $proc.StandardOutput.ReadToEnd()
    $stderr = $proc.StandardError.ReadToEnd()
    $proc.WaitForExit()
    Set-Content -Path $outFile -Value $stdout -NoNewline -Encoding utf8
    Set-Content -Path $errFile -Value $stderr -NoNewline -Encoding utf8
    [pscustomobject]@{ Exit = $proc.ExitCode; Stdout = $stdout; Stderr = $stderr; File = $outFile; ErrFile = $errFile }
}

$sh = Invoke-InProject @('shell', '--exec', 'npm config get prefix') 'npm-prefix'
Check 'shell --exec 退出 0' ($sh.Exit -eq 0) "exit=$($sh.Exit)"
Check '子进程里 npm 的 prefix 指向 tuoen 的根' ($sh.Stdout.Trim() -eq $script:NpmTree) "child=$($sh.Stdout.Trim()) expect=$script:NpmTree"

$shEnv = Invoke-InProject @('shell', '--exec', 'echo NPM_CONFIG_PREFIX=%NPM_CONFIG_PREFIX%') 'echo-env'
Check '子进程里 NPM_CONFIG_PREFIX 真的设上了' `
    ($shEnv.Stdout -match [regex]::Escape("NPM_CONFIG_PREFIX=$script:NpmTree")) "out=$($shEnv.Stdout.Trim())"

# pip 那一半同样要问一句：**重定向必须真的传到子进程**，而不是"我们以为传了"。
$expectPyRoot = Join-Path $script:TuoenGlobals 'pip'
$shPy = Invoke-InProject @('shell', '--exec', 'echo PYTHONUSERBASE=%PYTHONUSERBASE% PIP_USER=%PIP_USER%') 'echo-pip-env'
Check '子进程里 PYTHONUSERBASE 指向 tuoen 的 pip 根' `
    ($shPy.Stdout -match [regex]::Escape("PYTHONUSERBASE=$expectPyRoot")) "out=$($shPy.Stdout.Trim())"
Check '子进程里 PIP_USER = 1' ($shPy.Stdout -match 'PIP_USER=1') "out=$($shPy.Stdout.Trim())"

# 红线：跑完 shell 之后，父进程与注册表一个字节都没变（重定向绝不落盘）。
$afterShell = @(Get-EnvBlockSnapshot)
Check '跑完 shell 之后 HKCU\Environment 逐字未变' ((($script:EnvBefore -join "`n") -eq ($afterShell -join "`n")) -and ($script:EnvBefore.Count -gt 0)) "values=$($afterShell.Count)"
Check '注册表里仍然没有 NPM_CONFIG_PREFIX' (@($afterShell | Where-Object { $_ -like 'NPM_CONFIG_PREFIX|*' }).Count -eq 0) ''
Check '当前这个 shell 自己也没被污染' ([string]::IsNullOrEmpty($env:NPM_CONFIG_PREFIX)) ''

# ── 5. 非空计划：真装一个包（离线，只用缓存） ───────────────────────────

Section '5. 非空计划：把 pnpm 装进 tuoen 的根（离线）'

# **可重复性**：上一轮验收可能已经把这个包装进了 tuoen 的根，那这一轮的计划就会是
# `no-change`（"真的装一次"这条断言永远不成立）。所以这里先把**那一个包目录**删掉。
# 删之前先断言解析出来的路径确实在 tuoen 自己的根下面 —— 绝不删别的东西。
$pkgDir = Join-Path $script:NpmTree 'node_modules\pnpm'
if (Test-Path -LiteralPath $pkgDir) {
    $resolvedPkg = (Resolve-Path -LiteralPath $pkgDir).Path
    $resolvedBase = (Resolve-Path -LiteralPath $script:TuoenGlobals).Path
    if ($resolvedPkg.StartsWith($resolvedBase, [System.StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedPkg -Recurse -Force
        Note "删掉了上一轮留下的 $resolvedPkg（只删这一个包，好让这一轮真的装一次）"
    } else {
        Check '要删的包目录确实在 tuoen 的根下面（绝不动别处）' $false "pkg=$resolvedPkg base=$resolvedBase"
    }
} else {
    Note "tuoen 的根里还没有 pnpm —— 这一轮会是第一次装"
}

# 快照 = §3 那份真机 capture，但 `globals.toml` 只留 **pnpm 那一行** ——
# 这样"真的做了什么"是一个能逐项核对的有限动作，而不是"把机器上七个包都装一遍"。
$snap = Join-Path $root 'snap-1'
New-Item -ItemType Directory -Path $snap -Force | Out-Null
Copy-Item -Path (Join-Path $capDir '*') -Destination $snap -Force
$okPnpm = New-GlobalsSnapshot -Source (Join-Path $capDir 'globals.toml') -Tool 'npm' -Name 'pnpm' -Dest (Join-Path $snap 'globals.toml')
Check '快照里能定位到 pnpm 那一行、并且只留它' $okPnpm ''

$plan1 = Invoke-Tuoen @('restore', $snap, '--only', 'globals', '--json') 'restore plan globals'
Check 'restore --only globals（计划）退出 0' ($plan1.Exit -eq 0) "exit=$($plan1.Exit)"
$p1 = Get-Payload $plan1
$sec1 = Find (Arr $p1 'sections') 'id' 'globals'
Check '计划里有 globals 这一节' ($null -ne $sec1) "ids=$((@(Arr $p1 'sections' | ForEach-Object { Prop $_ 'id' }) -join ','))"
if ($sec1) {
    Check '计划说它会改东西（would-change）' ((Prop $sec1 'status') -eq 'would-change') "status=$(Prop $sec1 'status')"
    Check '缓存能解决 ⇒ needsNetwork = false（离线可做）' ((Prop $sec1 'needsNetwork') -eq $false) "needsNetwork=$(Prop $sec1 'needsNetwork')"
    Note "globals 那一节的键：$((PropNames $sec1) -join ',')"
}
Note "计划载荷的键：$((PropNames $p1) -join ',')"

# 票据 #24 §4 的**必需部分**（不是可选项）：快照里的 prefix 不是我们装进去的地方，
# 所以计划里必须有一条 `globals-prefix-moved` 的意图（只写意图，不写材料）。
$maCodes = @(Arr $p1 'manualActions' | ForEach-Object { Prop $_ 'code' })
Check '计划里有一条 globals-prefix-moved 的意图' `
    ($maCodes -contains 'globals-prefix-moved') "codes=$($maCodes -join ',')"

# 人类输出必须逐字说出来：装进的是 tuoen 的根，**不是**机器自己的那个 prefix。
# 期望值用 §1 自己量出来的 `npm config get prefix`（不是抄产品的话）。
$plan1Human = Invoke-Tuoen @('restore', $snap, '--only', 'globals') 'restore plan globals 人类输出'
Check '人类输出（计划）退出 0' ($plan1Human.Exit -eq 0) "exit=$($plan1Human.Exit)"
Check '人类输出逐字说出"不在机器自己的 prefix"' `
    (($plan1Human.Stdout -match '不在') -and ($plan1Human.Stdout -match [regex]::Escape($script:MachinePrefix))) `
    ("含前缀=$(($plan1Human.Stdout -match [regex]::Escape($script:MachinePrefix))) 含不在=$(($plan1Human.Stdout -match '不在'))")

$apply1 = Invoke-Tuoen @('restore', $snap, '--only', 'globals', '--apply', '--offline', '--json') 'restore apply globals'
Check 'restore --apply --offline 退出 0' ($apply1.Exit -eq 0) "exit=$($apply1.Exit)"
$ap1 = Get-Payload $apply1
$applyObj = Prop $ap1 'apply'
Check '载荷里有 apply' ($null -ne $applyObj) "keys=$(PropNames $ap1)"
Check 'apply.wrote = true' ((Prop $applyObj 'wrote') -eq $true) "wrote=$(Prop $applyObj 'wrote')"
$apSec = Find (Arr $applyObj 'sections') 'id' 'globals'
Check 'apply.sections 里有 globals' ($null -ne $apSec) ''
if ($apSec) {
    Check 'globals 的 outcome = applied' ((Prop $apSec 'outcome') -eq 'applied') "outcome=$(Prop $apSec 'outcome')"
    Check 'globals 的 wrote = true' ((Prop $apSec 'wrote') -eq $true) ''
    Note "apply.sections[globals] 的键：$((PropNames $apSec) -join ',')"
}

# 磁盘上的事实：装的必须是**快照里写的那个精确版本**。
$installed = Join-Path $script:NpmTree 'node_modules\pnpm'
Check "磁盘上真的有 $installed" (Test-Path -LiteralPath $installed) ''
$pkgJson = Join-Path $installed 'package.json'
if (Test-Path -LiteralPath $pkgJson) {
    $instVer = (Get-Content -LiteralPath $pkgJson -Raw | ConvertFrom-Json).version
    Check '装进去的是精确版本 11.21.0' ($instVer -eq '11.21.0') "version=$instVer"
} else {
    Skip '装进去的版本号' 'package.json 不在'
}

# ── 6. 幂等：再 plan 一次必须 no-change ────────────────────────────────

Section '6. 幂等：同样的快照再 plan 一次'

$plan2 = Invoke-Tuoen @('restore', $snap, '--only', 'globals', '--json') 'restore plan globals 2'
$p2 = Get-Payload $plan2
$sec2 = Find (Arr $p2 'sections') 'id' 'globals'
Check '第二次 plan 退出 0' ($plan2.Exit -eq 0) "exit=$($plan2.Exit)"
if ($sec2) {
    Check '第二次是 no-change（幂等判据只比 tuoen 侧）' ((Prop $sec2 'status') -eq 'no-change') "status=$(Prop $sec2 'status')"
}

$capDir2 = Join-Path $root 'capture-2'
$cap2 = Invoke-Tuoen @('capture', '--only', 'globals', '--out', $capDir2) 'capture globals 2'
$g2 = Get-Globals $capDir2
$g2Entries = @(Prop (Prop $g2 'globals') 'entries')
$g2Tuoen = @($g2Entries | Where-Object { $_.source -eq 'tuoen' })
$g2Machine = @($g2Entries | Where-Object { $_.source -eq 'machine' })
Check '装完之后 capture 里有 tuoen 来源的行' ($g2Tuoen.Count -ge 1) "count=$($g2Tuoen.Count)"
$pnpmTuoen = @($g2Tuoen | Where-Object { $_.tool -eq 'npm' -and $_.name -eq 'pnpm' })
Check 'tuoen 来源里有且只有一个 pnpm' ($pnpmTuoen.Count -eq 1) "rows=$($pnpmTuoen.Count)"
if ($pnpmTuoen.Count -eq 1) {
    Check 'tuoen 侧那个 pnpm 是 11.21.0' ($pnpmTuoen[0].version -eq '11.21.0') "version=$($pnpmTuoen[0].version)"
    # 快照里的包行只有 name/version（bin 名是 `--json` 那一侧的事），所以这里只断言版本。
}
Check '机器来源的行数一个字都没变（决策 167）' ($g2Machine.Count -eq $gMachine.Count) "before=$($gMachine.Count) after=$($g2Machine.Count)"

# ── 7. shim（#25）：只发 `.exe` ─────────────────────────────────────────

Section '7. shim：只发 `.exe`（npm 包的 bin 是 .cmd/.ps1，所以它不该有 shim）'

$shimDir = Join-Path $env:LOCALAPPDATA 'tuoen\shims'
$shimList = Invoke-Tuoen @('shim', 'list', '--json') 'shim list'
Check 'shim list 退出 0' ($shimList.Exit -eq 0) "exit=$($shimList.Exit)"
if (Test-Path -LiteralPath $shimDir) {
    $bad = @(Get-ChildItem -LiteralPath $shimDir -File -Force | Where-Object { $_.Extension -in @('.cmd', '.ps1') })
    Check 'shim 目录里没有任何 .cmd / .ps1（铁律：绝不发这两个）' ($bad.Count -eq 0) "bad=$(@($bad | ForEach-Object { $_.Name }) -join ',')"
    $all = @(Get-ChildItem -LiteralPath $shimDir -File -Force)
    Note "shim 目录里有 $($all.Count) 个文件：$(@($all | ForEach-Object { $_.Name }) -join ',')"
} else {
    Note "shim 目录还不存在：$shimDir"
}

# #25 的核心承诺（票据 §1/§4）：**装好的包要敲得出来** —— pnpm 的每一个 bin 名都要有
# `.exe` shim，而且 shim 跑出来的东西必须与"真身"逐字相同。
# 期望的**名字**从装进去的那个包**自己的 `package.json`** 读（不是抄产品的话，也不是写死）。
$installedPkgJson = Join-Path (Join-Path $script:NpmTree 'node_modules\pnpm') 'package.json'
$expectBins = @()
if (Test-Path -LiteralPath $installedPkgJson) {
    $binField = (Get-Content -LiteralPath $installedPkgJson -Raw | ConvertFrom-Json).bin
    $expectBins = if ($binField -is [System.Management.Automation.PSCustomObject]) {
        @($binField.PSObject.Properties | ForEach-Object { $_.Name })
    } else {
        @('pnpm')
    }
}
Check '从装进去的 package.json 读到了 pnpm 的 bin 名（票据实测 4 个）' ($expectBins.Count -eq 4) "bins=$($expectBins -join ',')"
foreach ($b in $expectBins) {
    Check "shim 目录里有 $b.exe（包的 bin → .exe shim）" (Test-Path -LiteralPath (Join-Path $shimDir "$b.exe")) ''
}
$shimPnpm = Join-Path $shimDir 'pnpm.exe'
if (Test-Path -LiteralPath $shimPnpm) {
    $viaShim = (Invoke-Program $shimPnpm @('--version')).Stdout.Trim()
    # "真身" = 机器自己的 pnpm（本机在 `C:\nvm4w\nodejs\pnpm.cmd`，它是个 `.cmd`，
    # 所以走 cmd.exe /c —— 我们自己的 shim 绝不发 `.cmd`，但**调用**别人的 `.cmd` 是另一回事）。
    $realCmd = Join-Path $script:MachinePrefix 'pnpm.cmd'
    $viaReal = if (Test-Path -LiteralPath $realCmd) { (& cmd.exe /d /c "`"$realCmd`" --version" 2>&1 | Out-String).Trim() } else { '' }
    Check 'shim 的 `pnpm --version` 与真身逐字相同' (($viaShim.Length -gt 0) -and ($viaShim -eq $viaReal)) "shim=$viaShim real=$viaReal"
    Check 'shim 报的版本就是快照里那个精确版本 11.21.0' ($viaShim -eq '11.21.0') "shim=$viaShim"
}

if ($WithNetwork) {
    Section '7b. pip 的 `.exe` shim（联网，-WithNetwork）'
    $snap2 = Join-Path $root 'snap-2'
    New-Item -ItemType Directory -Path $snap2 -Force | Out-Null
    Copy-Item -Path (Join-Path $capDir '*') -Destination $snap2 -Force
    $okPy = New-GlobalsSnapshot -Source (Join-Path $capDir 'globals.toml') -Tool 'pip' -Name 'pypinyin' -Dest (Join-Path $snap2 'globals.toml')
    Check '快照里能定位到 pypinyin 那一行、并且只留它' $okPy ''
    if ($okPy) {
        $apply2 = Invoke-Tuoen @('restore', $snap2, '--only', 'globals', '--apply', '--json') 'restore apply pip (network)'
        Check 'pip 的联网安装退出 0' ($apply2.Exit -eq 0) "exit=$($apply2.Exit)"
        $pipExe = Join-Path (Join-Path $script:TuoenGlobals 'pip') 'Scripts\pypinyin.exe'
        Check "pip 的 Scripts 里有 pypinyin.exe" (Test-Path -LiteralPath $pipExe) "path=$pipExe"
        $shimAdd = Invoke-Tuoen @('shim', 'add', 'pypinyin') 'shim add pypinyin'
        Check 'shim add pypinyin 退出 0' ($shimAdd.Exit -eq 0) "exit=$($shimAdd.Exit)"
        $shimExe = Join-Path $shimDir 'pypinyin.exe'
        Check '发出来的是 .exe' (Test-Path -LiteralPath $shimExe) "path=$shimExe"
        if (Test-Path -LiteralPath $shimExe) {
            $run = Invoke-Program $shimExe @('--version')
            Check 'shim 跑得起来' ($run.Exit -eq 0) "exit=$($run.Exit) out=$($run.Stdout.Trim())"
        }
    }
} else {
    Skip 'pip 侧的 .exe shim（需要联网装 pypinyin）' '-WithNetwork 未开'
}

# ── 8. 红线 ─────────────────────────────────────────────────────────────

Section '8. 红线：setx / HKCU\Environment / nvm4w / 机器侧 prefix'

# (a) 产品源码里不许有 setx 调用。判据用**拼出来的**名字，否则这段守卫会命中它自己
#     （#17 的教训：35 处守卫文字互相命中）。产品源码里到处用反引号写它来警告，所以
#     判据必须落在"会执行"的形态上：`Command::new("setx")` / `"setx.exe"`。
$needle = 's' + 'etx'
$srcHits = @(Get-ChildItem -Path (Join-Path $repo 'crates') -Recurse -Include *.rs |
        Select-String -Pattern ('Command::new\(\s*"' + $needle + '"|"' + $needle + '\.exe"'))
Check ('产品源码里没有调用 ' + $needle) ($srcHits.Count -eq 0) "hits=$($srcHits.Count)"
# 判据落在"会执行的语句"上：先把单引号/双引号字符串**挖掉**再找。
# 不然这三条会把它们自己抓住（`Section '…setx…'`、两条检查名里也写着它）——
# 这是 #17 那条"守卫会自己命中自己"的同一个坑，只是这次踩在字符串里而不是注释里。
$scriptHits = @(Get-Content -LiteralPath $PSCommandPath | Where-Object {
        $_ -notmatch '^\s*#' -and
        (($_ -replace "'[^']*'", "''") -replace '"[^"]*"', '""') -match ('(?<![\w`])' + $needle + '\b') })
Check '本脚本里没有会执行 setx 的语句' ($scriptHits.Count -eq 0) "hits=$($scriptHits.Count)"
# 反向验证：这条判据必须能红（否则"没有命中"什么都不说明）。
$selfProbe = @('& ' + $needle + ' FOO bar')
Check '反向验证：判据对一行真的 setx 调用会命中' (@($selfProbe | Where-Object { $_ -match ('(?<![\w`])' + $needle + '\b') }).Count -eq 1) ''

# (b) 整块环境：跑前跑后逐字相同。§5 真的装了包，而它**不该**碰注册表。
$after = @(Get-EnvBlockSnapshot)
Check 'HKCU\Environment 跑前跑后逐字相同' ((($script:EnvBefore -join "`n") -eq ($after -join "`n")) -and ($script:EnvBefore.Count -gt 0)) "before=$($script:EnvBefore.Count) after=$($after.Count)"
$snapAfter = Get-Snapshot
Check '用户级 Path 逐字未变' ($snapAfter.UserRaw -eq $script:Before.UserRaw) "sha16=$(Get-Sha16 $snapAfter.UserRaw)"
Check '机器级 Path 逐字未变' ($snapAfter.MachineRaw -eq $script:Before.MachineRaw) "sha16=$(Get-Sha16 $snapAfter.MachineRaw)"

# (c) nvm4w 的地盘（决策 154）。
$nvmAfter = Get-NvmState
Check 'NVM_HOME / NVM_SYMLINK 两个作用域逐字未变' ($nvmAfter.Env -eq $script:NvmBefore.Env) ''
Check 'nvm4w 的 junction 指向未变' ($nvmAfter.Junction -eq $script:NvmBefore.Junction) "junction=$($nvmAfter.Junction)"
Check 'nvm4w 的 settings.txt 哈希未变' ($nvmAfter.SettingsSha -eq $script:NvmBefore.SettingsSha) "sha=$($nvmAfter.SettingsSha)"
Check '机器自己的 npm prefix 没被碰' ((& cmd.exe /d /c 'npm config get prefix' 2>&1 | Out-String).Trim() -eq $script:MachinePrefix) "prefix=$script:MachinePrefix"

# ── 9. 实测数据 + 收尾 ─────────────────────────────────────────────────

Section '9. 实测数据'

foreach ($t in $script:Timings) {
    $parts = $t -split '\|'
    Write-Host ("    {0,-36} {1,7} ms" -f $parts[0], $parts[1])
}
Set-Content -LiteralPath (Join-Path $root 'timings.txt') -Value $script:Timings -Encoding utf8
Note "工件目录：$root"

Section '10. 收尾'

if ($SelfTest) {
    Check 'SELF-TEST：这一条**必须**失败（一条不能失败的验收不是验收）' $false '故意的'
}

$verdict = if ($script:Failed -eq 0) { 'PASS' } else { 'FAIL' }
Write-Host ''
if ($script:Failures.Count -gt 0) {
    Write-Host 'FAILURES:' -ForegroundColor Red
    foreach ($f in $script:Failures) { Write-Host "  - $f" -ForegroundColor Red }
}
Write-Host ("SUMMARY checks_passed={0} checks_failed={1} checks_skipped={2} verdict={3}" -f `
        $script:Passed, $script:Failed, $script:SkipCount, $verdict) `
    -ForegroundColor $(if ($script:Failed -eq 0) { 'Green' } else { 'Red' })
foreach ($n in $script:Notes) { Write-Host "NOTE $n" }
Write-Host "ARTIFACTS $root"

if ($script:Failed -gt 0) { exit 1 }
exit 0
