<#
.SYNOPSIS
    验收脚本：票据 #18 —— L1 真机验收（capture → doctor → path diff → restore → 幂等）。

.DESCRIPTION
    票据 #18 写的是**证据**，不是功能：在作者本机上跑完整流程，把每一步的真实输出落盘，
    并在每一步之后核对"这台机器一个字节都没变"。

    它只写两种东西：
      ① `%TEMP%\tuoen-acceptance-L1-18\` 下的工件；
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
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repo 'target\release\tuoen.exe'
$root = Join-Path $env:TEMP 'tuoen-acceptance-L1-18'
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

$helper = Join-Path $root '_l118.py'
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
path = load("path.toml")
tools = load("tools.toml")
env = load("env.toml")
wsl = load("wsl.toml")

entries = (path or {}).get("entry", [])
effective = (path or {}).get("effective", [])

out = {
    "files": sorted(n for n in os.listdir(d) if n.endswith(".toml")),
    "sections": list((sch or {}).get("sections", [])),
    "schema_version": (sch or {}).get("schema_version"),
    "tuoen_version": (sch or {}).get("tuoen_version"),
    "budget": (path or {}).get("budget"),
    "entries": [
        {
            "scope": e.get("scope"),
            "index": e.get("index"),
            "raw": e.get("raw"),
            "empty": e.get("empty"),
            "exists": e.get("exists"),
            "reparse": e.get("reparse"),
            "owner": e.get("owner"),
            "dup_index": e.get("dup_index"),
            "has_username": e.get("has_username"),
            "reg_type": e.get("reg_type"),
        }
        for e in entries
    ],
    "effective": [
        {"value": e.get("value"), "owners": e.get("owners")} if isinstance(e, dict) else {"value": e}
        for e in effective
    ],
    "tools": {"rows": len((tools or {}).get("tool", []))},
    "env": {"rows": len((env or {}).get("var", [])),
            "names": sorted(v.get("name") for v in (env or {}).get("var", []))},
    "wsl": {"rows": len((wsl or {}).get("distribution", []))},
}
print(json.dumps(out, ensure_ascii=False))
'@ | Set-Content -Path $helper -Encoding utf8 -NoNewline

function Get-Bundle {
    param([string]$Dir)
    $empty = [pscustomobject]@{
        files    = @()
        sections = @()
        budget   = $null
        entries  = @()
        effective = @()
        tools    = [pscustomobject]@{ rows = -1 }
        env      = [pscustomobject]@{ rows = -1; names = @() }
        wsl      = [pscustomobject]@{ rows = -1 }
    }
    if (-not (Test-Path -LiteralPath $Dir)) { return $empty }
    $json = & $PY $helper $Dir
    if ($LASTEXITCODE -ne 0) { return $empty }
    $parsed = $null
    try { $parsed = $json | ConvertFrom-Json } catch { return $empty }
    if ($null -eq $parsed) { return $empty }
    $parsed
}

# ── 0. 构建 ─────────────────────────────────────────────────────────────

Section '0. 构建'

$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"

if ($SkipBuild) {
    Skip 'release 构建' '-SkipBuild'
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

$u = Get-PathValue 'user'
$m = Get-PathValue 'machine'
$script:Pristine = ($m.Raw + ';' + $u.Raw)
$pristineEntries = @($script:Pristine -split ';' | Where-Object { $_.Trim() })
Write-Host ("  PATH 口径：机器级 {0} + 用户级 {1} = {2} 字符；非空条目 {3} 条" -f `
        $m.Raw.Length, $u.Raw.Length, $script:Pristine.Length, $pristineEntries.Count)
Check '两个作用域的 Path 都是 REG_SZ（本机事实，决定了只能验"不展开"那条分支）' `
    (($u.Kind -eq 'String') -and ($m.Kind -eq 'String')) "user=$($u.Kind) machine=$($m.Kind)"

$before = Get-Snapshot
$beforeEnv = Get-EnvBlockSnapshot
$beforeNvm = Get-NvmState
$beforeStore = Get-TreeListing "$env:LOCALAPPDATA\tuoen\store" 3
$beforeShims = Get-TreeListing "$env:LOCALAPPDATA\tuoen\shims" 2
$beforeCache = Get-TreeListing "$env:APPDATA\tuoen\cache" 2

Write-Host ("  HKCU\Environment：{0} 个值；Path {1} 字符 sha16 {2}" -f $beforeEnv.Count, $before.UserChars, $before.UserSha)
Write-Host ("  HKLM Path：{0} 字符 sha16 {1}" -f $before.MachineChars, $before.MachineSha)
Write-Host ("  nvm4w：{0}" -f $beforeNvm.Env)
Write-Host ("         junction → {0}" -f $beforeNvm.Junction)
Write-Host ("         settings.txt {0} B sha256 {1}" -f $beforeNvm.SettingsSize, $beforeNvm.SettingsSha.Substring(0, [Math]::Min(16, $beforeNvm.SettingsSha.Length)))

$backupFile = Join-Path $root 'hkcu-environment-backup.json'
$backupRows = Save-EnvBackup $backupFile
Check '整个 HKCU\Environment 已备份到文件（含类型与原文）' `
    ((Test-Path -LiteralPath $backupFile) -and ($backupRows.Count -eq $beforeEnv.Count)) `
    "$backupFile（$($backupRows.Count) 个值）"
Write-Host "  恢复命令（万一脚本中途死掉）：" -ForegroundColor Yellow
Write-Host "    `$rows = Get-Content '$backupFile' -Raw | ConvertFrom-Json; `$k = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', `$true); foreach (`$r in `$rows) { `$k.SetValue(`$r.Name, `$r.Value, [Microsoft.Win32.RegistryValueKind](`$r.Kind)) }; `$k.Dispose()" -ForegroundColor Yellow

# 规矩 7：**先自证还原能用，再允许写入跑**。把当前值原样写回一次，断言逐字未变。
$proved = Restore-EnvBackup $backupFile
$afterProve = Get-EnvBlockSnapshot
Check '自证还原：把备份原样写回一次后，整个 HKCU\Environment 逐字未变' `
    ((($afterProve -join '|') -eq ($beforeEnv -join '|')) -and ($proved -eq $backupRows.Count)) `
    "$proved 个值写回"

# ── 2. `capture`（真机，计时） ──────────────────────────────────────────

Section '2. `tuoen capture`（真机，默认全量）'

$snapDir = Join-Path $root 'tuoen.d'
$cap = Invoke-Tuoen @('capture', '--out', $snapDir, '--json') -Label 'capture（默认全量，--json）'
Check 'capture 退出 0' ($cap.Exit -eq 0) "exit=$($cap.Exit)  $($cap.Ms) ms"
Check '--json 成功载荷无 CJK（中文只进人类输出）' (Test-NoCjk $cap.Stdout)
$capPayload = Get-Payload $cap
Check '成功载荷有 data 信封，且四个旧 section 的数字都在' `
    (($null -ne (Prop $capPayload 'tools')) -and ($null -ne (Prop $capPayload 'path')) -and `
        ($null -ne (Prop $capPayload 'env')) -and ($null -ne (Prop $capPayload 'wsl')))
$capFiles = @(Arr $capPayload 'files')
Check '落盘八个文件（六个 section + schema + skipped）' ($capFiles.Count -eq 8) ($capFiles -join ',')

$b = Get-Bundle $snapDir
Check '脚本自己用 tomllib 能解析出全部八个文件' (@($b.files).Count -eq 8) ($b.files -join ',')
Check 'schema.sections 是六个 section 的字典序' `
    ((@($b.sections) -join ',') -eq 'configs,env,globals,path,tools,wsl') ($b.sections -join ',')

# 票据点名要贴的：`[budget]` 段与重复条目计数。
$budget = $b.budget
Write-Host '  ── path.toml 的 [budget] 段（逐字）──'
if ($null -ne $budget) {
    foreach ($k in @('raw_user_chars', 'raw_machine_chars', 'effective_chars', 'cliff', 'remaining', 'level')) {
        Write-Host ("    {0} = {1}" -f $k, (Prop $budget $k))
    }
}
Check '[budget] 段在（票据点名要贴）' ($null -ne $budget)
if ($null -ne $budget) {
    # **期望值从注册表自己算**：不展开的原文长度 + 一个分隔符。
    Check 'effective_chars = 机器级 + 用户级 + 1（脚本自己从注册表算）' `
        ((Prop $budget 'effective_chars') -eq ($m.Raw.Length + 1 + $u.Raw.Length)) `
        "产品=$(Prop $budget 'effective_chars') 脚本=$($m.Raw.Length + 1 + $u.Raw.Length)"
    Check 'raw_user_chars / raw_machine_chars 与注册表原文长度逐字相同' `
        (((Prop $budget 'raw_user_chars') -eq $u.Raw.Length) -and ((Prop $budget 'raw_machine_chars') -eq $m.Raw.Length))
    Check 'cliff = 8191（cmd.exe 的悬崖，决策 15）' ((Prop $budget 'cliff') -eq 8191)
    Check 'remaining = cliff − effective_chars（自洽）' `
        ((Prop $budget 'remaining') -eq (8191 - (Prop $budget 'effective_chars'))) `
        "remaining=$(Prop $budget 'remaining')"
    Check 'level = ok（本机还剩 6000+ 字符）' ((Prop $budget 'level') -eq 'ok') "level=$(Prop $budget 'level')"
}

$entries = @($b.entries)
Check 'path.toml 的行数 = 非空条目数 + 空条目数（脚本自己数）' `
    ($entries.Count -ge $pristineEntries.Count) "rows=$($entries.Count) 非空条目=$($pristineEntries.Count)"
$dupEntries = @($entries | Where-Object { (Prop $_ 'dup_index') -gt 0 })
$dupValues = @($dupEntries | ForEach-Object { [string](Prop $_ 'raw') } | Sort-Object -Unique)
Write-Host ("  ── 重复条目：{0} 行（涉及 {1} 个不同的值）──" -f $dupEntries.Count, $dupValues.Count)
Check '有重复条目（本机确实有，票据点名要数）' ($dupEntries.Count -gt 0) "dup=$($dupEntries.Count)"
# `dup_index` 的语义是**同一（归一化后的）值的出现序号**（决策 96）。归一化 = 去首尾空白、
# 去掉结尾的分隔符、大小写不敏感 —— 所以 `C:\Program Files\dotnet` 与 `…\dotnet\` 是**同一个值**。
# （第一版我按 `raw` 原文分组，于是"带尾斜杠的那一条 dup_index=1"看起来像错 —— **是我错了**。）
$dupBad = @()
$groups = @($entries | Group-Object { ([string](Prop $_ 'raw')).Trim().TrimEnd('\', '/').ToLowerInvariant() })
foreach ($g in $groups) {
    $idx = @($g.Group | ForEach-Object { [int](Prop $_ 'dup_index') } | Sort-Object)
    $want = @(0..($idx.Count - 1))
    if (($idx -join ',') -ne ($want -join ',')) { $dupBad += "$($g.Name) → $($idx -join ',')" }
}
Check '每个归一化后的值的 dup_index 恰好是 0..n−1（出现序号语义，决策 96）' `
    ($dupBad.Count -eq 0) ($dupBad -join '; ')
$emptyEntries = @($entries | Where-Object { (Prop $_ 'empty') -eq $true })
Check '空条目被标成 empty = true（决策 47：空段落不是一个缺失的目录）' ($emptyEntries.Count -ge 1) "empty=$($emptyEntries.Count)"
Check 'tools / env / wsl 的行数与 #16 的真机口径一致（新 section 没有改动旧输出）' `
    ((@($b.tools.rows)[0] -eq 27) -and (@($b.env.rows)[0] -eq 32) -and (@($b.wsl.rows)[0] -eq 2)) `
    "tools=$($b.tools.rows) env=$($b.env.rows) wsl=$($b.wsl.rows)"

# ── 3. `doctor`（真机，计时） ───────────────────────────────────────────

Section '3. `tuoen doctor`（真机）'

$docHuman = Invoke-Tuoen @('doctor') -Label 'doctor（人类输出）'
Check 'doctor 退出 0（发现了问题也是 0：严重度是数据，不是失败）' ($docHuman.Exit -eq 0) "exit=$($docHuman.Exit)  $($docHuman.Ms) ms"
$docHumanPath = Join-Path $root 'doctor-human.txt'
Set-Content -LiteralPath $docHumanPath -Value $docHuman.Stdout -Encoding utf8
Check '人类输出非空并已落盘' ($docHuman.Stdout.Trim().Length -gt 0) "$docHumanPath"

$docJson = Invoke-Tuoen @('doctor', '--json') -Label 'doctor（--json）'
$docPayload = Get-Payload $docJson
Check 'doctor --json 退出 0 且成功载荷无 CJK' (($docJson.Exit -eq 0) -and (Test-NoCjk $docJson.Stdout))
$findings = @(Arr $docPayload 'findings')
$counts = Prop $docPayload 'counts'
Check 'findings 非空（本机是"坏机器"，票据要的就是逐条对照）' ($findings.Count -gt 0) "count=$($findings.Count)"
Check 'counts 的三个数与 findings 的实际分布一致（自洽）' `
    ((@($findings | Where-Object { (Prop $_ 'severity') -eq 'error' }).Count -eq (Prop $counts 'error')) -and `
        (@($findings | Where-Object { (Prop $_ 'severity') -eq 'warn' }).Count -eq (Prop $counts 'warn')) -and `
        (@($findings | Where-Object { (Prop $_ 'severity') -eq 'info' }).Count -eq (Prop $counts 'info'))) `
    "error=$(Prop $counts 'error') warn=$(Prop $counts 'warn') info=$(Prop $counts 'info')"
$ids = @($findings | ForEach-Object { [string](Prop $_ 'id') } | Sort-Object -Unique)
Write-Host ("  19 类发现（逐条与取证报告对照）：{0}" -f ($ids -join ' · '))
$expectIds = @(
    'env.duplicated-scope', 'env.missing-target', 'env.name-with-spaces', 'env.path-literal',
    'path.duplicate', 'path.empty-entry', 'path.missing', 'path.reparse', 'path.spaces',
    'path.username-hardcoded', 'system.developer-mode', 'system.elevated', 'system.long-paths',
    'system.wsl-nonstandard-path', 'tool.ghost', 'tool.global-prefix-inside-version-dir',
    'tool.multi-manager', 'tool.multiple-active', 'tool.unmanaged-directory'
)
$missingIds = @($expectIds | Where-Object { $ids -notcontains $_ })
$extraIds = @($ids | Where-Object { $expectIds -notcontains $_ })
Check '发现的 id 集合与取证报告一致（缺/多都算差异）' `
    (($missingIds.Count -eq 0) -and ($extraIds.Count -eq 0)) `
    "缺=$(($missingIds -join ',')) 多=$(($extraIds -join ','))"
$nonAscii = @($findings | Where-Object {
        $ev = @(Arr $_ 'evidence')
        (@($ev | Where-Object { [string]$_ -notmatch '^[\x20-\x7e]*$' }).Count -gt 0)
    })
Check '每一条的 evidence 都是纯 ASCII（决策 137：evidence 是数据，中文只进 message）' `
    ($nonAscii.Count -eq 0) "非 ASCII $($nonAscii.Count) 条"
Check 'message 不进 --json（它天生是中文）' `
    (@($findings | Where-Object { (PropNames $_) -contains 'message' }).Count -eq 0)
# 本机三个已知事实必须在里面（取证报告里就有）。
$mustHave = @('tool.global-prefix-inside-version-dir', 'path.missing', 'system.developer-mode')
Check '本机已知的三类问题都在（版本目录里的全局前缀 / 失效条目 / 未开开发者模式）' `
    ((@($mustHave | Where-Object { $ids -contains $_ }).Count) -eq $mustHave.Count) `
    (($mustHave | Where-Object { $ids -notcontains $_ }) -join ',')

# ── 4. `path diff`（真机，计时） ────────────────────────────────────────

Section '4. `tuoen path diff`（真机，目标 = 本机自己刚 capture 的快照）'

$snapPathToml = Join-Path $snapDir 'path.toml'
$diffHuman = Invoke-Tuoen @('path', 'diff', $snapPathToml) -Label 'path diff（人类输出）'
Check 'path diff 退出 0（只读，永远 0）' ($diffHuman.Exit -eq 0) "exit=$($diffHuman.Exit)  $($diffHuman.Ms) ms"
$diffHumanPath = Join-Path $root 'path-diff-human.txt'
Set-Content -LiteralPath $diffHumanPath -Value $diffHuman.Stdout -Encoding utf8

$diffJson = Invoke-Tuoen @('path', 'diff', $snapPathToml, '--json') -Label 'path diff（--json）'
$diffPayload = Get-Payload $diffJson
$dc = Prop $diffPayload 'counts'
$diffRows = @(Arr $diffPayload 'rows')
Write-Host ("  ── 六类计数：{0}" -f (($dc | ConvertTo-Json -Compress)))
Check 'path diff 的行数 = 49（本机口径）' ($diffRows.Count -eq 49) "rows=$($diffRows.Count)"
Check '同一台机器上 capture 再 diff：add / remove / move / case-only **必须全是 0**' `
    (((Prop $dc 'add') -eq 0) -and ((Prop $dc 'remove') -eq 0) -and ((Prop $dc 'move') -eq 0) -and ((Prop $dc 'caseOnly') -eq 0)) `
    "add=$(Prop $dc 'add') remove=$(Prop $dc 'remove') move=$(Prop $dc 'move') caseOnly=$(Prop $dc 'caseOnly')"
Check 'keep + fix = 总行数（每一行都被归了类）' `
    (((Prop $dc 'keep') + (Prop $dc 'fix')) -eq $diffRows.Count) `
    "keep=$(Prop $dc 'keep') fix=$(Prop $dc 'fix')"
Check 'fix = 24（本机自己的健康问题：重复 / 失效 / 空条目 / 用户名）' ((Prop $dc 'fix') -eq 24) "fix=$(Prop $dc 'fix')"
Check 'snapshot 字段是**用户敲的那个路径原样**' `
    ((Prop $diffPayload 'snapshot') -eq $snapPathToml) "snapshot=$(Prop $diffPayload 'snapshot')"
$userFromProfile = Split-Path -Leaf $env:USERPROFILE
Check 'currentUsername 与脚本自己从 USERPROFILE 取的一致' `
    ((Prop $diffPayload 'currentUsername') -eq $userFromProfile) `
    "产品=$(Prop $diffPayload 'currentUsername') 脚本=$userFromProfile"
$typos = @(Arr $diffPayload 'typos')
Write-Host ("  拼写疑似 {0} 条（**只报告、永不纠错**，决策 24）" -f $typos.Count)

# ── 5. `restore --dry-run` 与 `restore`（同一套计划代码） ────────────────

Section '5. `restore --dry-run` 与 `restore`（默认形态）'

$rDry = Invoke-Tuoen @('restore', $snapDir, '--dry-run', '--json') -Label 'restore --dry-run'
$rPlain = Invoke-Tuoen @('restore', $snapDir, '--json') -Label 'restore（默认 = 只出计划）'
Check '两个都退出 0' (($rDry.Exit -eq 0) -and ($rPlain.Exit -eq 0)) "dry=$($rDry.Exit) plain=$($rPlain.Exit)"
Check '`--dry-run` 与默认形态的载荷**逐字节相同**（决策 150：同一套计划代码）' `
    ($rDry.Stdout.Trim() -eq $rPlain.Stdout.Trim())
$plan = Get-Payload $rPlain
$planSummary = Prop $plan 'summary'
Check '计划四个 section 全 no-change（还原到本机 = 接近空的计划）' `
    ((Prop $planSummary 'noChange') -eq 4) "noChange=$(Prop $planSummary 'noChange')"
Check 'wouldChange = 0 且 needsNetwork = 0' `
    (((Prop $planSummary 'wouldChange') -eq 0) -and ((Prop $planSummary 'needsNetwork') -eq 0))
$planActions = @(Arr $plan 'manualActions')
Check 'manualActions 三条（nvm4w / uv / 凭据重配）' ($planActions.Count -eq 3) "count=$($planActions.Count)"
$codes = @($planActions | ForEach-Object { [string](Prop $_ 'code') } | Sort-Object)
Check 'manualActions 的 code 是 third-party-manager ×2 + credential-reconfigure ×1' `
    (($codes -join ',') -eq 'credential-reconfigure,third-party-manager,third-party-manager') ($codes -join ',')
$credAction = Find $planActions 'code' 'credential-reconfigure'
Check '凭据那条只含**意图**（subject 是变量名，不是值）' `
    (($null -ne $credAction) -and ([string](Prop $credAction 'subject') -eq 'env:ARK_API_KEY')) `
    "subject=$(Prop $credAction 'subject')"
$arkRaw = [string](Get-Item 'HKCU:\Environment').GetValue('ARK_API_KEY', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
Check '脚本自己确认 ARK_API_KEY 在本机真的存在（否则上面那条是空真）' ($arkRaw.Length -gt 0) "长度 $($arkRaw.Length)（值不打印）"
Check '计划载荷里**没有**那个变量的值（材料从不进快照，决策 147）' `
    (-not $rPlain.Stdout.Contains($arkRaw))

# ── 6. 空计划上的 `--apply`：一个字节都不许写 ───────────────────────────

Section '6. `restore --apply`（计划是空的，所以"真的做"也必须什么都不做）'

$rApplyEmpty = Invoke-Tuoen @('restore', $snapDir, '--apply', '--json') -Label 'restore --apply（空计划）'
Check '空计划上的 --apply 退出 0' ($rApplyEmpty.Exit -eq 0) "exit=$($rApplyEmpty.Exit)  $($rApplyEmpty.Ms) ms"
$applyObj = Prop (Get-Payload $rApplyEmpty) 'apply'
Check 'apply.wrote = false（计划与现状一致 ⇒ 不写、也不广播）' ((Prop $applyObj 'wrote') -eq $false)
Check 'apply.sections 是空数组（没有一节被真的动过）' (@(Arr $applyObj 'sections').Count -eq 0)

# ── 7. 非空计划：计划 vs **真的执行** ───────────────────────────────────

Section '7. 非空计划：故意加一个不存在的用户级变量，看"计划 vs 真的执行"'

$copyDir = Join-Path $root 'probe-snapshot'
Copy-Item -LiteralPath $snapDir -Destination $copyDir -Recurse
$stamp = (Get-Date).ToString('yyyyMMddHHmmss')
$probeValue = "l118-$stamp"
$probeName = 'TUOEN_L1_ACCEPT_PROBE'
Add-Content -LiteralPath (Join-Path $copyDir 'env.toml') -NoNewline -Value (
    "`n[[var]]`nname = `"$probeName`"`nscope = `"user`"`nvalue_raw = `"$probeValue`"`n" +
    "value_expanded = `"$probeValue`"`nreg_type = `"sz`"`ntarget_exists = `"not-a-path`"`n")
$probeBundle = Get-Bundle $copyDir
Check '脚本自己加的 [[var]] 块能被 tomllib 解析（env 行数 +1）' `
    (@($probeBundle.env.rows)[0] -eq (@($b.env.rows)[0] + 1)) `
    "加之前=$(@($b.env.rows)[0]) 加之后=$(@($probeBundle.env.rows)[0])"

$probePlan = Invoke-Tuoen @('restore', $copyDir, '--only', 'env', '--json') -Label 'restore --only env（非空计划）'
$pp = Get-Payload $probePlan
$ppSections = @(Arr $pp 'sections')
$ppEnv = Find $ppSections 'id' 'env'
Check '非空计划里 env 那节 status = would-change' `
    (($null -ne $ppEnv) -and ((Prop $ppEnv 'status') -eq 'would-change')) "status=$(Prop $ppEnv 'status')"
$ppActions = @(Arr $ppEnv 'actions')
Write-Host ("  计划里的动作 {0} 条：" -f $ppActions.Count)
$ppActions | ForEach-Object { Write-Host ("    " + ($_ | ConvertTo-Json -Compress -Depth 4)) }
# `actions` 里**同时**有"要写的"与"只报告的"（决策 161：report-only 的动作不算写入）。
$ppWrites = @($ppActions | Where-Object { (Prop $_ 'kind') -eq 'set-user' })
$ppSkips = @($ppActions | Where-Object { (Prop $_ 'kind') -eq 'skipped-secret' })
Check '计划里恰好 1 条**要写的**动作（set-user），指向那个探针变量' `
    (($ppWrites.Count -eq 1) -and ([string](Prop $ppWrites[0] 'subject') -eq $probeName)) `
    "writes=$($ppWrites.Count) subject=$(Prop $ppWrites[0] 'subject')"
Check '同一节里那条**只报告**的动作也在（skipped-secret：凭据不进快照）' `
    (($ppSkips.Count -eq 1) -and ([string](Prop $ppSkips[0] 'subject') -eq 'ARK_API_KEY')) `
    "skips=$($ppSkips.Count) subject=$(Prop $ppSkips[0] 'subject')"

# 规矩 7 的第二半：**无论中途怎么死，探针变量都必须被删掉** —— 所以这一段包在 try/finally 里。
try {
$probeApply = Invoke-Tuoen @('restore', $copyDir, '--only', 'env', '--apply', '--json') -Label 'restore --only env --apply（真的写）'
Check '真的执行退出 0' ($probeApply.Exit -eq 0) "exit=$($probeApply.Exit)  $($probeApply.Ms) ms"
$pa = Prop (Get-Payload $probeApply) 'apply'
$paSections = @(Arr $pa 'sections')
Write-Host ("  apply：" + ($pa | ConvertTo-Json -Compress -Depth 5))
Check 'apply.wrote = true' ((Prop $pa 'wrote') -eq $true)
Check 'apply.sections 里恰好一节（env），且 wrote = true' `
    (($paSections.Count -eq 1) -and ((Prop $paSections[0] 'id') -eq 'env') -and ((Prop $paSections[0] 'wrote') -eq $true)) `
    "sections=$($paSections.Count) id=$(Prop $paSections[0] 'id') wrote=$(Prop $paSections[0] 'wrote')"
Check '计划里 1 条**要写的**动作 / 执行了 1 节 —— 逐条对上（票据要的"差异为空"）' `
    (($ppWrites.Count -eq 1) -and ($paSections.Count -eq 1) -and ((Prop $paSections[0] 'outcome') -eq 'applied')) `
    "writes=$($ppWrites.Count) sections=$($paSections.Count) outcome=$(Prop $paSections[0] 'outcome')"

$writtenRaw = [string](Get-Item 'HKCU:\Environment').GetValue($probeName, '<absent>', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
Check '注册表里真的出现了那个变量，值与快照逐字相同（不展开读）' ($writtenRaw -eq $probeValue) "读回=$writtenRaw"
$writtenKind = if ((Get-Item 'HKCU:\Environment').GetValueNames() -contains $probeName) { [string](Get-Item 'HKCU:\Environment').GetValueKind($probeName) } else { '<absent>' }
Check '值的类型是 REG_SZ（值里没有 %，所以不该用 REG_EXPAND_SZ）' ($writtenKind -eq 'String') "kind=$writtenKind"
$fresh = Invoke-FreshEnvProbe $probeName
Check '一个**从注册表重建环境**的新进程能看到它（"新终端里生效"的唯一合法验法）' `
    ($fresh -eq $probeValue) "新进程看到=$fresh"

$probeAgain = Invoke-Tuoen @('restore', $copyDir, '--only', 'env', '--json') -Label 'restore --only env（第二次 = 幂等）'
$pa2 = Get-Payload $probeAgain
$pa2Env = Find @(Arr $pa2 'sections') 'id' 'env'
Check '第二次计划变成 no-change（幂等：再跑一次是空的）' `
    (($null -ne $pa2Env) -and ((Prop $pa2Env 'status') -eq 'no-change')) "status=$(Prop $pa2Env 'status')"

} finally {
    # 清理：删掉探针变量 —— **一定会跑**，哪怕上面某一条真的抛了。
    Remove-UserEnvValue $probeName
}
$afterCleanup = Get-EnvBlockSnapshot
Check '删掉探针变量后，整个 HKCU\Environment 与跑之前**逐字相同**（含值名、类型、原文）' `
    (($afterCleanup -join '|') -eq ($beforeEnv -join '|')) `
    "值数 $($afterCleanup.Count) vs $($beforeEnv.Count)"

# ── 8. 幂等：两次 capture 逐字节相同 ───────────────────────────────────

Section '8. 幂等：两次 `capture` 除时间戳外逐字节相同'

$snap2 = Join-Path $root 'tuoen.d-2'
$null = Invoke-Tuoen @('capture', '--out', $snap2, '--json') -Label 'capture（第二次，验幂等）'
$idemBad = @()
foreach ($f in @('path.toml', 'tools.toml', 'env.toml', 'wsl.toml', 'globals.toml', 'configs.toml', 'skipped.toml', 'schema.toml')) {
    $t1 = Get-WithoutTimestamp (Join-Path $snapDir $f)
    $t2 = Get-WithoutTimestamp (Join-Path $snap2 $f)
    if ($t1 -ne $t2) { $idemBad += $f }
}
Check '八个文件两次捕获除 captured_at 外逐字节相同' ($idemBad.Count -eq 0) ($idemBad -join ', ')

# ── 9. 未使用 setx ─────────────────────────────────────────────────────

Section '9. 未使用 `setx`（票据点名的证据）'

$setxProduct = @(Select-String -Path (Join-Path $repo 'crates\*\src\*.rs'), (Join-Path $repo 'crates\*\src\*\*.rs') `
        -Pattern '["'']setx' -ErrorAction SilentlyContinue)
Check '产品源码里没有带引号的 setx 字面量（grep 无命中）' ($setxProduct.Count -eq 0) "$($setxProduct.Count) 处"
$setxScript = @(Select-String -Path (Join-Path $repo 'scripts\*.ps1') -Pattern '[s]etx' -ErrorAction SilentlyContinue)
$setxSuspect = @($setxScript | Where-Object { (($_.Line -replace '#.*$', '') -match '[s]etx\s+[%/A-Za-z]') })
Check '脚本里没有一条会执行 setx 的语句（这个命令只许出现在守卫自己的文字里）' `
    ($setxSuspect.Count -eq 0) (($setxSuspect | ForEach-Object { "$(Split-Path -Leaf $_.Path):$($_.LineNumber)" }) -join ', ')
$setxProbe = Join-Path $root 'setx-probe.ps1'
# 这一行**故意**拼出来写（`'& set' + 'x …'`）：脚本自己也在 §9 的扫描范围里，
# 写成整串会让判据命中它自己要证明的那件事（#17 的教训：守卫会自己命中自己）。
Set-Content -LiteralPath $setxProbe -Value ('& set' + 'x TUOEN_L118_PROBE 1') -Encoding utf8
$setxProbeHit = @(Select-String -Path $setxProbe -Pattern '[s]etx' | Where-Object { (($_.Line -replace '#.*$', '') -match '[s]etx\s+[%/A-Za-z]') })
Check '反向验证：塞一条真的 setx 调用进去，判据必须命中（1 条）' ($setxProbeHit.Count -eq 1) "命中 $($setxProbeHit.Count)"

# ── 10. 未触碰 nvm4w / store / 缓存 ────────────────────────────────────

Section '10. 未触碰 nvm4w 与 tuoen 自己的目录（跑前跑后逐项相同）'

$afterNvm = Get-NvmState
Check 'NVM_HOME / NVM_SYMLINK 在两个作用域里的原文与类型逐字未变' `
    ($afterNvm.Env -eq $beforeNvm.Env) "after=$($afterNvm.Env)"
Check '`C:\nvm4w\nodejs` 这个 junction 的**指向**未变' ($afterNvm.Junction -eq $beforeNvm.Junction) "after=$($afterNvm.Junction)"
Check 'nvm4w 的 settings.txt 哈希未变' ($afterNvm.SettingsSha -eq $beforeNvm.SettingsSha) `
    "before=$($beforeNvm.SettingsSha.Substring(0, [Math]::Min(16, $beforeNvm.SettingsSha.Length))) after=$($afterNvm.SettingsSha.Substring(0, [Math]::Min(16, $afterNvm.SettingsSha.Length)))"
$afterStore = Get-TreeListing "$env:LOCALAPPDATA\tuoen\store" 3
Check '`store/` 目录树逐项相同（没装、没删、没换 current）' (($afterStore -join '|') -eq ($beforeStore -join '|'))
$afterCache = Get-TreeListing "$env:APPDATA\tuoen\cache" 2
Check '`%APPDATA%\tuoen\cache` 目录树逐项相同' (($afterCache -join '|') -eq ($beforeCache -join '|'))
$afterPath = Get-Snapshot
Check '两个作用域的 Path 原文与类型逐字未变（这一票一次都没写过 PATH）' `
    (($afterPath.UserRaw -eq $before.UserRaw) -and ($afterPath.UserKind -eq $before.UserKind) -and `
        ($afterPath.MachineRaw -eq $before.MachineRaw) -and ($afterPath.MachineKind -eq $before.MachineKind)) `
    "user sha $(Get-Sha16 $afterPath.UserRaw) vs $($before.UserSha)"

# ── 11. 实测数据（票据点名要的） ────────────────────────────────────────

Section '11. 实测数据'

Write-Host '  ── 耗时（真实毫秒，含进程启动）──'
foreach ($t in $script:Timings) { Write-Host ("    {0}" -f ($t -replace '\|', '  →  ') + ' ms') }
Write-Host ("    PATH 长度：机器级 {0} + 用户级 {1} = {2} 字符（跑前跑后相同）" -f `
        $before.MachineChars, $before.UserChars, ($before.MachineChars + 1 + $before.UserChars))

# shim 启动开销：store 里已经有 L0 装的 node 24.19.0，所以不需要下载。
$shimPathOut = Invoke-Tuoen @('shim', 'path') -Label 'shim path'
$shimDir = @($shimPathOut.Stdout -split "`r?`n" | Where-Object { $_.Trim() })[-1].Trim()
Check 'shim path 打印出一个目录' (Test-Path -LiteralPath $shimDir) "shimDir=$shimDir"
$shimAdd = Invoke-Tuoen @('shim', 'add', 'node') -Label 'shim add node'
Check 'shim add node 退出 0' ($shimAdd.Exit -eq 0) "exit=$($shimAdd.Exit)"
$shimExe = Join-Path $shimDir 'node.exe'
Check 'shim 目录里真的出现 node.exe' (Test-Path -LiteralPath $shimExe) "$shimExe"
$realNode = Join-Path $env:LOCALAPPDATA 'tuoen\store\node\current\node.exe'
Check 'store 里有生效的 node.exe（L0 时期装的，这一票没有下载任何东西）' (Test-Path -LiteralPath $realNode) "$realNode"
if ((Test-Path -LiteralPath $shimExe) -and (Test-Path -LiteralPath $realNode)) {
    $shimRuns = @(1..5 | ForEach-Object { Invoke-Program $shimExe @('--version') })
    $realRuns = @(1..5 | ForEach-Object { Invoke-Program $realNode @('--version') })
    $shimMs = @($shimRuns | ForEach-Object { $_.Ms } | Sort-Object)
    $realMs = @($realRuns | ForEach-Object { $_.Ms } | Sort-Object)
    Write-Host ("    shim  node.exe --version ：{0}（中位 {1} ms）" -f (($shimMs -join ' / ') + ' ms'), $shimMs[2])
    Write-Host ("    真身  node.exe --version ：{0}（中位 {1} ms）" -f (($realMs -join ' / ') + ' ms'), $realMs[2])
    Check 'shim 与真身的输出逐字相同（v24.19.0）' `
        ((($shimRuns | ForEach-Object { $_.Stdout.Trim() } | Sort-Object -Unique) -join ',') -eq `
            (($realRuns | ForEach-Object { $_.Stdout.Trim() } | Sort-Object -Unique) -join ',')) `
        "shim=$($shimRuns[0].Stdout.Trim()) 真身=$($realRuns[0].Stdout.Trim())"
    Check 'shim 的额外开销是"一次进程启动"量级（中位差 < 40 ms）' `
        (($shimMs[2] - $realMs[2]) -lt 40) "差 $($shimMs[2] - $realMs[2]) ms"
    Note ("shim 第一次跑用了 $($shimRuns[0].Ms) ms（刚写出来的 .exe 会被 Defender 扫一遍），后续 $($shimMs[1])–$($shimMs[4]) ms")
    $script:Timings.Add(("shim node.exe --version（中位）|{0}" -f $shimMs[2]))
    $script:Timings.Add(("store node.exe --version（中位）|{0}" -f $realMs[2]))
}
# `shim remove` 收的是**命令名**（不是工具 id）：`shim add node` 生成四条（node/npm/npx/corepack），
# 所以删的时候要把四条都给出来 —— 名字从 shim 目录里**自己数**，不猜。
$shimNames = @(Get-ChildItem -LiteralPath $shimDir -Filter '*.exe' -File -ErrorAction SilentlyContinue |
        ForEach-Object { [System.IO.Path]::GetFileNameWithoutExtension($_.Name) })
Write-Host ("  shim add node 生成了 {0} 条：{1}" -f $shimNames.Count, ($shimNames -join ', '))
Check 'shim add node 生成四条（node / npm / npx / corepack，实测过的命令表）' `
    ((($shimNames | Sort-Object) -join ',') -eq 'corepack,node,npm,npx') ($shimNames -join ',')
$shimRemove = Invoke-Tuoen (@('shim', 'remove') + $shimNames) -Label 'shim remove（四条命令名）'
$shimGone = @(Get-ChildItem -LiteralPath $shimDir -Filter '*.exe' -File -ErrorAction SilentlyContinue).Count -eq 0
Check 'shim remove 四条命令名：退出 0 且文件真的全没了' (($shimRemove.Exit -eq 0) -and $shimGone) `
    "exit=$($shimRemove.Exit) 文件还在=$(-not $shimGone)"
# #21 报的是"对本来就不在的 shim 报失败"。这一票**只记录**它的真实行为（不改代码、不判对错）。
$shimGhost = Invoke-Tuoen @('shim', 'remove', 'tuoen-definitely-not-a-shim') -Label 'shim remove（不存在的名字）'
Note ("#21 复现记录：`shim remove tuoen-definitely-not-a-shim` 退出码 = $($shimGhost.Exit)（#21 的期望是幂等成功 0）")
$afterShims = Get-TreeListing "$env:LOCALAPPDATA\tuoen\shims" 2
Check 'shim 目录回到跑之前的样子（这一票没有留下 shim）' (($afterShims -join '|') -eq ($beforeShims -join '|'))

$listOut = Invoke-Tuoen @('list') -Label 'tuoen list'
Check 'tuoen list 退出 0 且列出 node 24.19.0' `
    (($listOut.Exit -eq 0) -and ($listOut.Stdout -match 'node') -and ($listOut.Stdout -match '24\.19\.0'))

# ── 12. 收尾 ───────────────────────────────────────────────────────────

Section '12. 收尾'

$finalEnv = Get-EnvBlockSnapshot
Check '最后再核一次：整个 HKCU\Environment 与跑之前逐字相同' `
    (($finalEnv -join '|') -eq ($beforeEnv -join '|')) "值数 $($finalEnv.Count) vs $($beforeEnv.Count)"
$finalPath = Get-Snapshot
Check '最后再核一次：两个作用域的 Path 逐字未变' `
    (($finalPath.UserRaw -eq $before.UserRaw) -and ($finalPath.MachineRaw -eq $before.MachineRaw))

if ($SelfTest) {
    Check '自检：这一条必须失败（-SelfTest 用）' $false '故意失败'
}

$summaryPath = Join-Path $root 'summary.txt'
$summaryLines = @()
$summaryLines += "checks_passed=$($script:Passed) checks_failed=$($script:Failed) checks_skipped=$($script:SkipCount)"
$summaryLines += ''
$summaryLines += '── 耗时 ──'
$summaryLines += @($script:Timings | ForEach-Object { $_ -replace '\|', '  →  ' })
$summaryLines += ''
$summaryLines += '── 备注 ──'
$summaryLines += @($script:Notes)
Set-Content -LiteralPath $summaryPath -Value $summaryLines -Encoding utf8

$verdict = if ($script:Failed -eq 0) { 'PASS' } else { 'FAIL' }
Write-Host ''
Write-Host "SUMMARY checks_passed=$($script:Passed) checks_failed=$($script:Failed) checks_skipped=$($script:SkipCount) verdict=$verdict" `
    -ForegroundColor $(if ($verdict -eq 'PASS') { 'Green' } else { 'Red' })
if ($script:Failed -gt 0) {
    Write-Host '失败项：' -ForegroundColor Red
    foreach ($f in $script:Failures) { Write-Host "  - $f" -ForegroundColor Red }
}
Write-Host "工件目录：$root"

exit $(if ($script:Failed -gt 0) { 1 } else { 0 })
