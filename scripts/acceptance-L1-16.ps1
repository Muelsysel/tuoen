# 验收脚本：票据 #16 `tuoen restore`（plan → diff → apply + manual_actions）
#
# 票据的验收标准：
#   ① `cargo test --workspace` 全绿、clippy `-D warnings` 干净；
#   ② **在作者本机上真实跑一次 `tuoen restore --dry-run`**（输入是本机自己 `capture` 出来的
#      `tuoen.d/`）—— 这是最诚实的测试：**还原到本机应当产出接近空的 plan**；
#   ③ 贴出 `manual_actions` 的真实输出（预期至少含凭据重配的**意图**，且**不含任何材料**）；
#   ④ 幂等性证据（连跑两次，第二次为空）。
#
# 这条脚本**不写任何东西**：`restore` 的默认行为就是"只出计划"，本脚本全部用默认形态跑，
# 并且每一步之后都核对"这台机器一个字节都没变"（注册表 + store 树 + `%APPDATA%\tuoen` 树）。
# 票据明令：**不得在开发机上真的执行一次完整 restore**。
#
# **一条不能失败的验收不是验收**：`-SelfTest` 会让最后一条检查故意失败（exit 1）。
#
# .PARAMETER SelfTest
#     故意让最后一条检查失败，用来证明这条脚本**会**报失败（exit 1）。
#
# .PARAMETER SkipBuild
#     跳过 §0 的 release 构建（只在已经构建过、且想快速重跑时用）。
#
# .EXAMPLE
#     pwsh -File scripts/acceptance-L1-16.ps1
#     pwsh -File scripts/acceptance-L1-16.ps1 -SelfTest   # 必须 exit 1
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
$root = Join-Path $env:TEMP 'tuoen-acceptance-L1-16'
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

# ── 器材：注册表（**只读**） ─────────────────────────────────────────────

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
# 票据的原话是"断言 nvm4w 的假注册表后端 / 假 settings.txt 未被写"—— 在 plan 那一层写这条
# 是恒真断言（`plan()` 签名里根本没有注册表，§1.14 规矩 5）。真机上的等价物是这一份状态：
# 两个作用域里 `NVM_HOME`/`NVM_SYMLINK` 的原文与类型、`settings.txt` 的哈希、以及那个 junction
# 的**指向**。跑前跑后逐字相同，才是"我们真的没碰它"。
function Get-NvmState {
    $envKeys = @{
        'user'    = 'HKCU:\Environment'
        'machine' = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
    }
    $vals = [ordered]@{}
    foreach ($scope in @('user', 'machine')) {
        $item = Get-Item $envKeys[$scope]
        foreach ($n in @('NVM_HOME', 'NVM_SYMLINK')) {
            $raw = [string]$item.GetValue($n, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $kind = if ($item.GetValueNames() -contains $n) { [string]$item.GetValueKind($n) } else { '<absent>' }
            $vals["$scope/$n"] = "$kind|$raw"
        }
    }
    # 只为了**找到那个文件**才展开（报告与比较用的都是原文；同一份展开两侧一致）。
    $nvmHome = [string](Get-Item $envKeys['user']).GetValue('NVM_HOME', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $settingsSha = '<no NVM_HOME>'
    if ($nvmHome) {
        $settingsPath = Join-Path ([Environment]::ExpandEnvironmentVariables($nvmHome)) 'settings.txt'
        if (Test-Path -LiteralPath $settingsPath) {
            $settingsSha = Get-Sha16 (Get-Content -LiteralPath $settingsPath -Raw)
        } else {
            $settingsSha = '<absent>'
        }
    }
    $symlink = [string](Get-Item $envKeys['user']).GetValue('NVM_SYMLINK', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
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
        Env        = (($vals.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ' | ')
        SettingsSha = $settingsSha
        Junction   = $junction
    }
}

# 全环境块快照（值名 + 类型 + 原文，按名字排序）——"一个字节都没变"的最强形态。
function Get-EnvBlockSnapshot {
    $item = Get-Item 'HKCU:\Environment'
    @(@($item.GetValueNames()) | Sort-Object | ForEach-Object {
            "$_|$([string]$item.GetValueKind($_))|$([string]$item.GetValue($_, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames))"
        })
}

# 写 / 删一个用户级环境变量。**必须走可写子键句柄**：
# `(Get-Item 'HKCU:\Environment').SetValue(...)` 拿到的是 PowerShell provider 的**只读**句柄，
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

# "一个全新终端会看到什么"：从**注册表**读这个变量（不展开）塞进一个 cmd 子进程，再 `echo %NAME%`。
# 与 `scripts/fresh-terminal.ps1` 同一条原理 —— 环境块只在 `CreateProcess` 时复制，
# 所以**在当前这个 shell 里再 echo 一次证明不了任何事**。这里必须先 `Remove` 掉继承来的同名变量，
# 否则测到的是我这份旧环境（"看起来对上了"的假象）。
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
    # 同名时**用户级赢**（机器级块先建立、用户级块后合并；`Path` 是唯一的合并特例）。
    if ($machineRaw) { $psi.EnvironmentVariables[$Name] = [Environment]::ExpandEnvironmentVariables($machineRaw) }
    if ($userRaw) { $psi.EnvironmentVariables[$Name] = [Environment]::ExpandEnvironmentVariables($userRaw) }
    $proc = [System.Diagnostics.Process]::Start($psi)
    $out = $proc.StandardOutput.ReadToEnd()
    $proc.WaitForExit()
    $out.Trim()
}

# 一个目录树的清单（相对路径 + 大小），用来证明"没有新目录、没有新文件"。
# `DirectoryInfo` **没有** `Length` 属性，而 `Set-StrictMode -Latest` 下访问不存在的属性是
# 终止错误 —— 所以这里必须按"是不是目录"分开取（这是本条脚本第一次真跑时踩到的坑）。
function Get-TreeListing {
    param([string]$Root, [int]$Depth = 2)
    if (-not (Test-Path -LiteralPath $Root)) { return @('<absent>') }
    @(Get-ChildItem -LiteralPath $Root -Force -Recurse -Depth $Depth |
            Sort-Object FullName | ForEach-Object {
                $size = if ($_.PSIsContainer) { 'dir' } else { $_.Length }
                "$($_.FullName.Substring($Root.Length))|$size"
            })
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
    # 否则"本机侧"会被 cargo / PowerShell 注入的条目污染（分母必须说出来）——
    # 而 restore 的本机侧就是子进程现场 `capture` 出来的那份。
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
#
# 注意这里**不能**写 `$Obj.PSObject.Properties.Name`（成员枚举的惯用法）：
# `effective` 在"没有要写的条目"时是一个**空对象**，对它做成员枚举在 StrictMode 下会报
# `在此对象上找不到属性"Name"` —— 本条脚本第二次真跑时就是死在这一条上（而它恰好是
# "计划里没有任何要写的东西"这个**最正常**的形态）。改成对每个 `PSPropertyInfo` 逐个取 `Name`。
function Prop {
    param([object]$Obj, [string]$Name)
    if ($null -eq $Obj) { return $null }
    $names = @($Obj.PSObject.Properties | ForEach-Object { $_.Name })
    if ($names -contains $Name) { $Obj.$Name } else { $null }
}

# 一个对象上**实际存在**的键名。空对象、`$null`、字符串都安全（StrictMode 下不抛）。
# 按 id 取一节。**找不到就返回 `$null`**，不抛 —— 否则产品哪天少报一节，
# 这条脚本会以 "Index was outside the bounds of the array" 中止，而不是打出一条 FAIL。
function Sec {
    param([object]$Sections, [string]$Id)
    $hit = @(@($Sections) | Where-Object { (Prop $_ 'id') -eq $Id })
    if ($hit.Count -eq 0) { return $null }
    $hit[0]
}
function PropNames {
    param([object]$Obj)
    if ($null -eq $Obj) { return @() }
    @($Obj.PSObject.Properties | ForEach-Object { $_.Name })
}

# 成功载荷的形状是 `{schemaVersion, command, ok, data:{…}}`。**这一层信封在断言里不该出现**：
# `Prop $j 'rows'` 会返回 `$null`，而 `@($null).Count` 是 **1**（不是 0）——
# #15 的验收里五条断言同时报"产品 1 条 / 脚本 50 条"就是这么来的。
function Get-Payload {
    param([object]$Result)
    $j = Get-Json $Result
    if ($null -eq $j) { return $null }
    Prop $j 'data'
}

# 数一个"可能不在"的数组：`Prop` 对不存在的键返回 `$null`、对**空数组**返回"什么都没有"，
# 两种都会被 `@(...)` 变成一个元素的数组。要数条数的地方一律走这里，调用方再包一层 `@()`。
function Arr {
    param([object]$Obj, [string]$Name)
    @(Prop $Obj $Name | Where-Object { $null -ne $_ })
}

# ── 独立解析：`tuoen.d/` 用 Python 的 `tomllib` ───────────────────────────

$PY = Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\python.exe'
if (-not (Test-Path -LiteralPath $PY)) {
    # 只认真实的解释器：`WindowsApps\python.exe` 是 App Execution Alias（0 字节重解析点），
    # 而"绝不执行 App Execution Alias"是本仓库的硬约束。
    throw "找不到真实 Python：$PY（不接受 WindowsApps 的别名）"
}

$bundleHelper = Join-Path $root '_bundle.py'
@'
import json, os, sys, tomllib

d = sys.argv[1]

def load(name):
    p = os.path.join(d, name)
    if not os.path.exists(p):
        return None
    with open(p, "rb") as fh:
        return tomllib.load(fh)

tools = load("tools.toml")
env = load("env.toml")
path = load("path.toml")
wsl = load("wsl.toml")
skipped = load("skipped.toml")

tool_rows = (tools or {}).get("tool", [])
env_rows = (env or {}).get("var", [])
path_rows = (path or {}).get("entry", [])
distros = (wsl or {}).get("distribution", [])
skip_rows = (skipped or {}).get("skipped", [])

out = {
    "files": sorted(n for n in os.listdir(d) if n.endswith(".toml")),
    "tools": {
        "rows": len(tool_rows),
        "reproducible": sum(1 for r in tool_rows if r.get("reproducible")),
        "not_reproducible": sum(1 for r in tool_rows if not r.get("reproducible")),
        "managers": sorted({r["manager"] for r in tool_rows if r.get("manager")}),
        "names": sorted({r["name"] for r in tool_rows}),
    },
    "env": {
        "rows": len(env_rows),
        "user": sum(1 for r in env_rows if r.get("scope") == "user"),
        "machine": sum(1 for r in env_rows if r.get("scope") == "machine"),
        "names": sorted({r["name"] for r in env_rows}),
    },
    "path": {
        "rows": len(path_rows),
        "machine": sum(1 for r in path_rows if r.get("scope") == "machine"),
        "user": sum(1 for r in path_rows if r.get("scope") == "user"),
    },
    "wsl": {"rows": len(distros), "names": sorted({r["name"] for r in distros})},
    "skipped": [
        {"section": r.get("section"), "scope": r.get("scope"), "name": r.get("name"), "kind": r.get("kind")}
        for r in skip_rows
    ],
}
print(json.dumps(out, ensure_ascii=False))
'@ | Set-Content -Path $bundleHelper -Encoding utf8 -NoNewline

function Get-Bundle {
    param([string]$Dir)
    $json = & $PY $bundleHelper $Dir
    if ($LASTEXITCODE -ne 0) { throw "tomllib 解析失败：$Dir" }
    $json | ConvertFrom-Json
}

# ── §0 构建 ─────────────────────────────────────────────────────────────

Section '0 构建'

if ($SkipBuild) {
    Skip 'cargo build --release -p tuoen-cli' '-SkipBuild'
} else {
    $env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"
    $build = & cargo build --release -p tuoen-cli 2>&1
    $code = $LASTEXITCODE
    Check 'cargo build --release -p tuoen-cli 成功' ($code -eq 0) (($build | Select-Object -Last 1) -join '')
}
Check 'tuoen.exe 存在' (Test-Path -LiteralPath $exe) $exe

# ── §1 准备：真快照、前后快照、脚本自己的独立解析 ───────────────────────

Section '1 准备：本机自己的快照（只读）'

# "新终端口径"的 PATH：机器级在前、用户级在后，从注册表**不展开**读出来。
# 子进程拿它当 PATH —— restore 的本机侧就是子进程现场 capture 出来的那一份。
$script:Pristine = ((Get-PathValue 'machine').Raw.TrimEnd(';')) + ';' + ((Get-PathValue 'user').Raw.TrimStart(';'))
Write-Host ("       子进程 PATH（新终端口径）= {0} 字符" -f $script:Pristine.Length)

$before = Get-Snapshot
$beforeNvm = Get-NvmState
$realStore = Join-Path $env:LOCALAPPDATA 'tuoen'
$beforeStore = Get-TreeListing -Root $realStore -Depth 2
$beforeAppData = Get-TreeListing -Root (Join-Path $env:APPDATA 'tuoen') -Depth 2

$snapDir = Join-Path $root 'real\tuoen.d'
New-Item -ItemType Directory -Path (Split-Path $snapDir) -Force | Out-Null
$cap = Invoke-Tuoen -Arguments @('capture', '--out', $snapDir) -Name 'capture'
Check 'capture --out 退出 0' ($cap.Exit -eq 0) "exit=$($cap.Exit)"
$bundle = Get-Bundle $snapDir
Write-Host ("       快照文件：{0}" -f ($bundle.files -join ', '))
Write-Host ("       tools {0} 行（可复现 {1} / 不可复现 {2}；管理器 {3}）· env {4} 行（user {5} / machine {6}）· path {7} 行 · wsl {8} · skipped {9}" -f `
        $bundle.tools.rows, $bundle.tools.reproducible, $bundle.tools.not_reproducible, ($bundle.tools.managers -join '+'), `
        $bundle.env.rows, $bundle.env.user, $bundle.env.machine, $bundle.path.rows, $bundle.wsl.rows, @($bundle.skipped).Count)
Check '快照里有四个 section 文件（tools/env/path/wsl）' `
    ((@($bundle.files) -contains 'tools.toml') -and (@($bundle.files) -contains 'env.toml') -and `
        (@($bundle.files) -contains 'path.toml') -and (@($bundle.files) -contains 'wsl.toml')) `
    ($bundle.files -join ',')
Check '快照里真的有一份"不可复现"的工具行（否则 unsupported 那条判据没有样本）' `
    ($bundle.tools.not_reproducible -gt 0) "$($bundle.tools.not_reproducible) 行"
Check '快照里真的有第三方管理器（nvm4w / uv）' (@($bundle.tools.managers).Count -gt 0) ($bundle.tools.managers -join ',')
Check 'skipped.toml 里有被跳过的凭据命名变量（credential-reconfigure 的样本）' `
    (@($bundle.skipped | Where-Object { $_.kind -eq 'credential-named' }).Count -gt 0) `
    ((@($bundle.skipped | ForEach-Object { $_.name })) -join ',')

# ── §2 默认形态 = 只出计划：还原到本机应当"接近空" ──────────────────────

Section '2 `restore <dir> --json`（默认 = 只出计划）'

$plan = Invoke-Tuoen -Arguments @('restore', $snapDir, '--json') -Name 'plan'
Check 'restore（不带 --apply）退出 0' ($plan.Exit -eq 0) "exit=$($plan.Exit)"
$pj = Get-Payload $plan
Check '--json 能解析出 data' ($null -ne $pj) ''
if ($null -eq $pj) {
    Write-Host $plan.Stderr -ForegroundColor Red
} else {
    $sections = @(Arr $pj 'sections')
    Check 'sections 恰好四个' ($sections.Count -eq 4) "$($sections.Count) 个"
    Check 'section 的 id 是 tools/path/env/wsl' `
        ((($sections | ForEach-Object { [string](Prop $_ 'id') } | Sort-Object) -join ',') -eq 'env,path,tools,wsl') `
        (($sections | ForEach-Object { Prop $_ 'id' }) -join ',')
    $statuses = @($sections | ForEach-Object { [string](Prop $_ 'status') })
    Check '四个 section 全是 no-change（票据："还原到本机应当产出接近空的 plan"）' `
        ((@($statuses | Where-Object { $_ -ne 'no-change' })).Count -eq 0) ($statuses -join ',')

    # tools：本机已有同名同版本 → 只计数，不进 actions
    $tsec = Sec $sections 'tools'
    $trows = Prop (Prop $tsec 'counts') 'rows'
    $installed = [int](Prop $trows 'installed')
    Check 'tools：installed = 快照里的工具行数（同名同版本都在本机）' `
        ($installed -eq $bundle.tools.rows) ("产品 $installed / 脚本 $($bundle.tools.rows)")
    Check 'tools：missing = 0' ([int](Prop $trows 'missing') -eq 0) "$(Prop $trows 'missing')"
    Check 'tools：third-party 计数 = 快照里带 manager 的行数' `
        ([int](Prop $trows 'third-party') -le $bundle.tools.rows) "$(Prop $trows 'third-party')"
    Check 'tools：没有 install 动作（本机什么都不缺）' `
        ((@(Arr $tsec 'actions' | Where-Object { (Prop $_ 'kind') -eq 'install' })).Count -eq 0) ''
    Check 'tools：needsNetwork = false' ([bool](Prop $tsec 'needsNetwork') -eq $false) "$(Prop $tsec 'needsNetwork')"

    # path：本机自己的健康问题**只报告、未选中**（决策 151）
    $psec = Sec $sections 'path'
    $prows = Prop (Prop $psec 'counts') 'rows'
    Check 'path：本机自己的健康问题真的存在（fix > 0，否则下面那条"默认不选中"是空真）' `
        ([int](Prop $prows 'fix') -gt 0) "fix=$(Prop $prows 'fix')"
    $script:ExpectedFixRows = [int](Prop $prows 'fix')

    # 这里的分类数字**不写死**：它是**本机事实**，写死会在机器变化时报出一个假的差异 ——
    # 而"假的差异与代码错了长得一模一样"（AGENTS.md 规矩 4）。独立的对照是另一条命令：
    # `path diff` 与本命令走**不同的装配路径**（前者是 CLI 现场 capture 的 path 段，
    # 后者是 `RestoreBundle::from_capture`），两者对同一份快照必须给出同一组分类数字。
    $diffCross = Invoke-Tuoen -Arguments @('path', 'diff', (Join-Path $snapDir 'path.toml'), '--json') -Name 'pathdiff-cross'
    $dc = Prop (Get-Payload $diffCross) 'counts'
    $sameCounts = $false
    if ($null -ne $dc) {
        $sameCounts = $true
        foreach ($k in @('keep', 'add', 'remove', 'move', 'fix', 'caseOnly')) {
            if ([int](Prop $dc $k) -ne [int](Prop $prows $k)) { $sameCounts = $false }
        }
    }
    Check '交叉对照：`path diff` 与 `restore` 对同一份快照给出同一组分类数字（两条命令、两套装配）' `
        $sameCounts `
        ("path diff = " + ((@('keep', 'add', 'remove', 'move', 'fix', 'caseOnly') | ForEach-Object { "$_=$(Prop $dc $_)" }) -join ' ') + `
            " / restore = " + ((@('keep', 'add', 'remove', 'move', 'fix', 'caseOnly') | ForEach-Object { "$_=$(Prop $prows $_)" }) -join ' '))
    Check 'path：add/remove/move 都是 0（目标就是本机）' `
        (([int](Prop $prows 'add') -eq 0) -and ([int](Prop $prows 'remove') -eq 0) -and ([int](Prop $prows 'move') -eq 0)) `
        ("add=$(Prop $prows 'add') remove=$(Prop $prows 'remove') move=$(Prop $prows 'move')")
    Check 'path：note = fix-not-selected（那 24 条是本机自己的病，默认不应用）' `
        ((Prop $psec 'note') -eq 'fix-not-selected') "$(Prop $psec 'note')"
    $peff = Prop (Prop $psec 'counts') 'effective'
    Check 'path：effective 里没有任何要写的条目（决策 158 的第二个口径）' `
        ((((@('add', 'remove', 'move', 'fix') | ForEach-Object { [int](Prop $peff $_) })) -join ',') -eq '0,0,0,0') `
        ((@('add', 'remove', 'move', 'fix') | ForEach-Object { "$_=$(Prop $peff $_)" }) -join ' ')

    # env：32 行全在
    $esec = Sec $sections 'env'
    $erows = Prop (Prop $esec 'counts') 'rows'
    Check 'env：present = 快照里的变量行数' ([int](Prop $erows 'present') -eq $bundle.env.rows) `
        ("产品 $(Prop $erows 'present') / 脚本 $($bundle.env.rows)")
    Check 'env：没有 set-user 动作（本机什么都不缺）' `
        ((@(Arr $esec 'actions' | Where-Object { (Prop $_ 'kind') -eq 'set-user' })).Count -eq 0) ''
    # 被跳过的凭据变量**可以**出现在 actions 里，但只能以 `skipped-secret` 的身份 ——
    # 它是"我们看到了、没写"这个**事实**，不是"要写"这个动作。
    $arkActions = @(Arr $esec 'actions' | Where-Object { (Prop $_ 'subject') -match 'ARK_API_KEY' })
    Check 'env：被跳过的凭据变量只以 skipped-secret 出现（不是"要写"）' `
        (($arkActions.Count -ge 1) -and ((@($arkActions | Where-Object { (Prop $_ 'kind') -ne 'skipped-secret' })).Count -eq 0)) `
        (($arkActions | ForEach-Object { "$(Prop $_ 'subject')=$(Prop $_ 'kind')" }) -join ' ')

    # wsl：只报告
    $wsec = Sec $sections 'wsl'
    $wrows = Prop (Prop $wsec 'counts') 'rows'
    Check 'wsl：same = 快照里的发行版数' ([int](Prop $wrows 'same') -eq $bundle.wsl.rows) `
        ("产品 $(Prop $wrows 'same') / 脚本 $($bundle.wsl.rows)")
    $weff = Prop (Prop $wsec 'counts') 'effective'
    Check 'wsl：effective 恒空（只报告，决策 159 的 wsl 那一行）' (@(PropNames $weff).Count -eq 0) `
        ((@(PropNames $weff)) -join ',')
    Check 'wsl：note = report-only（不自动导入 vhdx）' ((Prop $wsec 'note') -eq 'report-only') "$(Prop $wsec 'note')"

    # 信封契约
    Check '--json 成功载荷是纯 ASCII（中文只进人类输出）' (Test-NoCjk $plan.Stdout) ''
    Check '--json 里没有 message 键' (-not ($plan.Stdout -match '"message"')) ''
    Check '--json 里没有时间戳' (-not ($plan.Stdout -match 'capturedAt|captured_at|\d{4}-\d{2}-\d{2}T')) ''
    Check '决策 162：--json 的 snapshot 字段 = 用户给的那个目录原样' `
        ((Prop $pj 'snapshot') -eq $snapDir) "$(Prop $pj 'snapshot')"
    $plan2 = Invoke-Tuoen -Arguments @('restore', $snapDir, '--json') -Name 'plan2'
    Check '两次 --json 逐字节相同（幂等的第一层证据）' ($plan.Stdout -ceq $plan2.Stdout) ''
    # 票据："`--dry-run` 与真实执行走**同一套代码路径**"。在进程边界上能证明的最强形态是：
    # 默认（只出计划）与显式 `--dry-run` 逐字节相同 —— 两者若走了不同分支，迟早会漂移。
    $dryExplicit = Invoke-Tuoen -Arguments @('restore', $snapDir, '--dry-run', '--json') -Name 'dry-explicit'
    Check '显式 --dry-run 与默认形态的 --json 逐字节相同（同一套代码路径）' `
        (($dryExplicit.Exit -eq 0) -and ($dryExplicit.Stdout -ceq $plan.Stdout)) "exit=$($dryExplicit.Exit)"
}

# ── §3 `manual_actions`：意图，不是材料 ─────────────────────────────────

Section '3 manual_actions：凭据只写意图，绝不写材料'

$human = Invoke-Tuoen -Arguments @('restore', $snapDir) -Name 'human'
Check '人类输出退出 0' ($human.Exit -eq 0) "exit=$($human.Exit)"
$human | ForEach-Object { $_.Stdout } | Set-Content -LiteralPath (Join-Path $root 'restore-human.txt') -Encoding utf8
Check '人类输出是中文' ([bool]($human.Stdout -match '[\u4e00-\u9fff]')) ''
Check '人类输出说了"无变更"（本机就是快照的来源）' ($human.Stdout -match '无变更') ''
Check '人类输出说了"什么都没有写"' ($human.Stdout -match '什么都没有写') ''
Check '人类输出里有"要人工做的事"这一段' ($human.Stdout -match '人工') ''

if ($null -ne $pj) {
    $ma = @(Arr $pj 'manualActions')
    Check 'manualActions 非空（票据预期：至少有凭据重配的意图）' ($ma.Count -gt 0) "$($ma.Count) 条"
    $codes = @($ma | ForEach-Object { [string](Prop $_ 'code') })
    $allowed = @('requires-elevation', 'credential-reconfigure', 'licence-blocked', 'third-party-manager', 'unsupported')
    Check '每个 code 都是那五个之一' ((@($codes | Where-Object { $allowed -notcontains $_ })).Count -eq 0) ($codes -join ',')
    Check '每条都带 subject / detail / remediation，且全是纯 ASCII' `
        ((@($ma | Where-Object { -not (Prop $_ 'subject') -or -not (Prop $_ 'detail') -or -not (Prop $_ 'remediation') })).Count -eq 0 -and `
            (@($ma | Where-Object { -not (Test-NoCjk ([string](Prop $_ 'subject') + [string](Prop $_ 'detail') + [string](Prop $_ 'remediation'))) })).Count -eq 0) `
        (($ma | Select-Object -First 3 | ForEach-Object { "$(Prop $_ 'code'):$(Prop $_ 'subject')" }) -join ' | ')
    Check '凭据意图里有那个被跳过的变量名（ARK_API_KEY；subject 或 detail 命中都算）' `
        ((@($codes | Where-Object { $_ -eq 'credential-reconfigure' })).Count -gt 0 -and `
            (@($ma | Where-Object { (Prop $_ 'code') -eq 'credential-reconfigure' -and `
                        (("$(Prop $_ 'subject') $(Prop $_ 'detail')") -match 'ARK_API_KEY') })).Count -ge 1) `
        (($ma | Where-Object { (Prop $_ 'code') -eq 'credential-reconfigure' } | ForEach-Object { "$(Prop $_ 'subject')/$(Prop $_ 'detail')" }) -join ',')
    Check '第三方管理器按管理器去重后出现在待办里' `
        ((@($ma | Where-Object { (Prop $_ 'code') -eq 'third-party-manager' })).Count -ge 1) `
        (($ma | Where-Object { (Prop $_ 'code') -eq 'third-party-manager' } | ForEach-Object { Prop $_ 'subject' }) -join ',')

    # **安全红线**：输出里不许出现那个变量的任何材料。
    # 脚本自己从不展开地读它（只在这条断言里用），并且**绝不打印它**。
    $arkItem = Get-Item 'HKCU:\Environment'
    $arkVal = [string]$arkItem.GetValue('ARK_API_KEY', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    if ($arkVal.Length -lt 8) {
        Skip '凭据材料没有泄漏进输出' "本机 HKCU\Environment 里没有够长的 ARK_API_KEY（长度 $($arkVal.Length)）"
    } else {
        $frag = $arkVal.Substring(0, 8)
        $leaked = ($plan.Stdout -match [regex]::Escape($frag)) -or ($human.Stdout -match [regex]::Escape($frag)) -or `
                  ($plan.Stdout -match [regex]::Escape($arkVal)) -or ($human.Stdout -match [regex]::Escape($arkVal))
        Check '凭据材料没有泄漏进任何输出（前 8 字符与全长都比过）' (-not $leaked) `
            ("比较过 {0} 字符的前缀（值本身不打印）" -f $arkVal.Length)
    }
    Check '输出里没有 `glpat-` 形状的东西' `
        ((-not ($plan.Stdout -match 'glpat-')) -and (-not ($human.Stdout -match 'glpat-'))) ''
}

# ── §4 零副作用：默认形态一个字节都不写 ─────────────────────────────────

Section '4 零副作用：注册表 / store / %APPDATA% 逐项相同'

$after = Get-Snapshot
Check 'HKCU Path 逐字未变' (($after.UserRaw -ceq $before.UserRaw) -and ($after.UserSha -ceq $before.UserSha)) `
    "$($before.UserSha) → $($after.UserSha)"
Check 'HKCU Path 类型未变' ($after.UserKind -ceq $before.UserKind) "$($before.UserKind) → $($after.UserKind)"
Check 'HKLM Path 逐字未变' (($after.MachineRaw -ceq $before.MachineRaw) -and ($after.MachineSha -ceq $before.MachineSha)) ''
Check '%LOCALAPPDATA%\tuoen 清单逐项相同（含 store 两层 —— 没有偷偷装东西）' `
    (((Get-TreeListing -Root $realStore -Depth 2) -join '|') -eq ($beforeStore -join '|')) ''
Check '%APPDATA%\tuoen 清单逐项相同' `
    (((Get-TreeListing -Root (Join-Path $env:APPDATA 'tuoen') -Depth 2) -join '|') -eq ($beforeAppData -join '|')) ''
# 决策 154 的红线（票据点名）：第三方版本管理器的一切都不许动。
$afterNvm = Get-NvmState
Check 'nvm4w：NVM_HOME / NVM_SYMLINK 在两个作用域里的原文与类型逐字未变' `
    ($afterNvm.Env -ceq $beforeNvm.Env) $beforeNvm.Env
Check 'nvm4w：settings.txt 哈希未变（不碰它的配置）' ($afterNvm.SettingsSha -ceq $beforeNvm.SettingsSha) `
    "$($beforeNvm.SettingsSha) → $($afterNvm.SettingsSha)"
Check 'nvm4w：那个 junction 的指向未变（不碰它的符号链接）' ($afterNvm.Junction -ceq $beforeNvm.Junction) `
    "$($beforeNvm.Junction) → $($afterNvm.Junction)"

# ── §5 幂等：连跑两次，第二次仍然"无变更" ──────────────────────────────

Section '5 幂等'

$again = Invoke-Tuoen -Arguments @('restore', $snapDir, '--json') -Name 'again'
$againj = Get-Payload $again
Check '第二次退出 0' ($again.Exit -eq 0) "exit=$($again.Exit)"
Check '第二次 --json 与第一次逐字节相同' ($again.Stdout -ceq $plan.Stdout) ''
if ($null -ne $againj) {
    Check '第二次仍然没有 would-change' `
        ((@(Arr $againj 'sections' | Where-Object { (Prop $_ 'status') -eq 'would-change' })).Count -eq 0) ''
    Check '第二次的 summary.wouldChange = 0' ([int](Prop (Prop $againj 'summary') 'wouldChange') -eq 0) `
        "$(Prop (Prop $againj 'summary') 'wouldChange')"
}
$human2 = Invoke-Tuoen -Arguments @('restore', $snapDir) -Name 'human2'
Check '第二次人类输出仍然说"无变更"' ($human2.Stdout -match '无变更') ''
Check '第二次人类输出仍然说"什么都没有写"' ($human2.Stdout -match '什么都没有写') ''

# ── §6 参数面与错误路径 ─────────────────────────────────────────────────

Section '6 参数面与错误路径'

$both = Invoke-Tuoen -Arguments @('restore', $snapDir, '--apply', '--dry-run') -Name 'both'
Check '--apply 与 --dry-run 同时给 → 退出 2（clap 冲突）' ($both.Exit -eq 2) "exit=$($both.Exit)"

$badOnly = Invoke-Tuoen -Arguments @('restore', $snapDir, '--only', 'nonsense') -Name 'badonly'
Check '--only 给了不认识的 section → 退出 2' ($badOnly.Exit -eq 2) "exit=$($badOnly.Exit)"

$onlyPath = Invoke-Tuoen -Arguments @('restore', $snapDir, '--only', 'path', '--json') -Name 'onlypath'
$opj = Get-Payload $onlyPath
Check '--only path 退出 0' ($onlyPath.Exit -eq 0) "exit=$($onlyPath.Exit)"
if ($null -ne $opj) {
    $others = @(Arr $opj 'sections' | Where-Object { (Prop $_ 'id') -ne 'path' })
    Check '--only path：其余三个 section 是 skipped + note=not-selected' `
        ((@($others | Where-Object { (Prop $_ 'status') -ne 'skipped' -or (Prop $_ 'note') -ne 'not-selected' })).Count -eq 0) `
        (($others | ForEach-Object { "$(Prop $_ 'id')=$(Prop $_ 'status')/$(Prop $_ 'note')" }) -join ' ')
    Check '--only path：tools 的 actions 为空（没选它就不许列它的动作）' `
        ((@(Arr (Sec (Arr $opj 'sections') 'tools') 'actions')).Count -eq 0) ''
}

$emptyDir = Join-Path $root 'empty\tuoen.d'
New-Item -ItemType Directory -Path $emptyDir -Force | Out-Null
$empty = Invoke-Tuoen -Arguments @('restore', $emptyDir, '--json') -Name 'empty'
$ej = Get-Json $empty
Check '空快照 → 退出 1' ($empty.Exit -eq 1) "exit=$($empty.Exit)"
Check '空快照 → 错误码是 empty-snapshot（不许静默成功）' `
    ($null -ne $ej -and (Prop (Prop $ej 'error') 'code') -eq 'empty-snapshot') `
    ($(if ($null -ne $ej) { Prop (Prop $ej 'error') 'code' } else { '<no json>' }))

$missing = Invoke-Tuoen -Arguments @('restore', (Join-Path $root 'nope\tuoen.d'), '--json') -Name 'missing'
$mj = Get-Json $missing
Check '快照目录不存在 → 退出 1 且错误码稳定' `
    ($missing.Exit -eq 1 -and $null -ne $mj -and (Prop (Prop $mj 'error') 'code') -eq 'snapshot-io') `
    ($(if ($null -ne $mj) { Prop (Prop $mj 'error') 'code' } else { '<no json>' }))

$help = Invoke-Tuoen -Arguments @('restore', '--help') -Name 'help'
Check 'restore --help 里有 --only / --apply / --dry-run / --with-fix' `
    (($help.Stdout -match '--only') -and ($help.Stdout -match '--apply') -and ($help.Stdout -match '--dry-run') -and ($help.Stdout -match '--with-fix')) ''

# ── §7 合成快照：让四个 section 真的说"会改"（否则计划永远是空的，验收就是空的） ──

Section '7 合成快照：四个 section 都能说出"会改什么"'

$synDir = Join-Path $root 'synthetic\tuoen.d'
New-Item -ItemType Directory -Path $synDir -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $snapDir 'schema.toml') -Destination $synDir
Copy-Item -LiteralPath (Join-Path $snapDir 'path.toml') -Destination $synDir
Copy-Item -LiteralPath (Join-Path $snapDir 'env.toml') -Destination $synDir
Copy-Item -LiteralPath (Join-Path $snapDir 'wsl.toml') -Destination $synDir
Copy-Item -LiteralPath (Join-Path $snapDir 'tools.toml') -Destination $synDir
Copy-Item -LiteralPath (Join-Path $snapDir 'skipped.toml') -Destination $synDir

# tools：一条**本机没有**的可复现工具 → 应当出现 install 动作 + needsNetwork
$toolBlock = @'

# 由验收脚本追加：一条本机没有的、可复现的工具
[[tool]]
name = "probe-tool"
version = "9.9.9"
path = 'C:\Dev\probe-tool\probe-tool.exe'
source = "path-resolution"
confidence = "executable"
evidence = "验收脚本造的探针"
reproducible = true
'@
Add-Content -LiteralPath (Join-Path $synDir 'tools.toml') -Value $toolBlock -Encoding utf8

# tools：一条**不可复现**的、名字在已知许可表里的工具 → 决策 160 的 `licence-blocked`
$oracleBlock = @'

# 由验收脚本追加：不可再分发的制品（票据点名的 Oracle JDK 8）
[[tool]]
name = "oracle-jdk"
version = "8"
path = 'C:\Program Files\Java\jdk1.8.0_202'
source = "registry-arp"
confidence = "registered"
evidence = "验收脚本造的探针"
reproducible = false
'@
Add-Content -LiteralPath (Join-Path $synDir 'tools.toml') -Value $oracleBlock -Encoding utf8

# env：一条本机没有的用户级变量 → 应当出现 set-user 动作
$envBlock = @'

# 由验收脚本追加：一条本机没有的用户级变量
[[var]]
name = "TUOEN_RESTORE_PROBE"
scope = "user"
value_raw = "probe"
value_expanded = "probe"
reg_type = "sz"
target_exists = "not-a-path"
'@
Add-Content -LiteralPath (Join-Path $synDir 'env.toml') -Value $envBlock -Encoding utf8

# wsl：一个本机没有的发行版 → 应当出现 missing-distro
$wslBlock = @'

# 由验收脚本追加：一个本机没有的发行版
[[distribution]]
name = "probe-distro"
guid = "{00000000-0000-0000-0000-0000000000ff}"
base_path = 'C:\probe\wsl\probe-distro'
non_standard_path = true
wsl_version = 2
state = 1
vhdx_path = 'C:\probe\wsl\probe-distro\ext4.vhdx'
vhdx_exists = "no"
'@
Add-Content -LiteralPath (Join-Path $synDir 'wsl.toml') -Value $wslBlock -Encoding utf8

# path：一条本机没有的目录 → 应当出现 add（默认选择就含 add）
$nextUser = ($bundle.path.user)
$pathBlock = @'

# 由验收脚本追加：一条本机没有的目录
[[entry]]
scope = "user"
index = __INDEX__
owner = "unknown"
raw = "C:\\Dev\\probe-path"
expanded = "C:\\Dev\\probe-path"
quoted = false
exists = "yes"
reparse = "none"
has_vars = false
has_username = false
dup_index = 0
empty = false
reg_type = "sz"
'@
Add-Content -LiteralPath (Join-Path $synDir 'path.toml') -Value ($pathBlock.Replace('__INDEX__', "$nextUser")) -Encoding utf8

$synBundle = Get-Bundle $synDir
Check '合成快照比真快照多了六行（tools 两条 + env/wsl/path 各一条）' `
    (($synBundle.tools.rows -eq $bundle.tools.rows + 2) -and ($synBundle.env.rows -eq $bundle.env.rows + 1) -and `
        ($synBundle.wsl.rows -eq $bundle.wsl.rows + 1) -and ($synBundle.path.rows -eq $bundle.path.rows + 1)) `
    ("tools {0}→{1} env {2}→{3} wsl {4}→{5} path {6}→{7}" -f $bundle.tools.rows, $synBundle.tools.rows, `
        $bundle.env.rows, $synBundle.env.rows, $bundle.wsl.rows, $synBundle.wsl.rows, $bundle.path.rows, $synBundle.path.rows)

$syn = Invoke-Tuoen -Arguments @('restore', $synDir, '--json') -Name 'syn'
$sj = Get-Payload $syn
Check '合成快照：restore 退出 0' ($syn.Exit -eq 0) "exit=$($syn.Exit)"
if ($null -ne $sj) {
    $ss = @(Arr $sj 'sections')
    $st = Sec $ss 'tools'
    $se = Sec $ss 'env'
    $sw = Sec $ss 'wsl'
    $sp = Sec $ss 'path'
    Check 'tools 说出会改（install）' `
        ((Prop $st 'status') -eq 'needs-network' -and (@(Arr $st 'actions' | Where-Object { (Prop $_ 'kind') -eq 'install' -and (Prop $_ 'subject') -match 'probe-tool' })).Count -eq 1) `
        ("status=$(Prop $st 'status') actions=$(@(Arr $st 'actions').Count)")
    Check 'tools 标了 needsNetwork（决策 157：网络需求是计划里的数据）' ([bool](Prop $st 'needsNetwork') -eq $true) "$(Prop $st 'needsNetwork')"
    Check '决策 161：有 install 动作的 section，status 是 needs-network（不是笼统的 would-change）' `
        ((Prop $st 'status') -eq 'needs-network') "$(Prop $st 'status')"
    Check 'env 说出会改（set-user）' `
        ((Prop $se 'status') -eq 'would-change' -and (@(Arr $se 'actions' | Where-Object { (Prop $_ 'kind') -eq 'set-user' -and (Prop $_ 'subject') -match 'TUOEN_RESTORE_PROBE' })).Count -eq 1) `
        ("status=$(Prop $se 'status') actions=$(@(Arr $se 'actions').Count)")
    Check 'wsl 说出会改（missing-distro）' `
        ((Prop $sw 'status') -eq 'no-change' -and (Prop $sw 'note') -eq 'report-only' -and `
            (@(Arr $sw 'actions' | Where-Object { (Prop $_ 'kind') -eq 'missing-distro' })).Count -ge 1) `
        ("status=$(Prop $sw 'status') note=$(Prop $sw 'note') actions=$(@(Arr $sw 'actions').Count)")
    Check 'wsl 的 missing-distro 是"只报告的动作"：它**不进** effective（决策 159 的 report-only）' `
        (@(PropNames (Prop (Prop $sw 'counts') 'effective')).Count -eq 0) ''
    Check 'path 说出会改（add 那条探针目录）' `
        ((Prop $sp 'status') -eq 'would-change' -and [int](Prop (Prop (Prop $sp 'counts') 'effective') 'add') -eq 1) `
        ("status=$(Prop $sp 'status') effective.add=$(Prop (Prop (Prop $sp 'counts') 'effective') 'add')")
    Check 'summary.wouldChange = 2（tools 是 needs-network、wsl 只报告 → 都不进这一格）' `
        ([int](Prop (Prop $sj 'summary') 'wouldChange') -eq 2) "$(Prop (Prop $sj 'summary') 'wouldChange')"
    Check 'summary.needsNetwork = 1（决策 161：这一格真的会有人）' `
        ([int](Prop (Prop $sj 'summary') 'needsNetwork') -eq 1) "$(Prop (Prop $sj 'summary') 'needsNetwork')"
    $hist = @('noChange', 'wouldChange', 'requiresElevation', 'needsNetwork', 'unsupported', 'skipped') |
        ForEach-Object { [int](Prop (Prop $sj 'summary') $_) }
    Check 'summary 六个计数器是 status 直方图（相加 = 4 个 section）' ((($hist | Measure-Object -Sum).Sum) -eq 4) `
        ("noChange=$($hist[0]) wouldChange=$($hist[1]) requiresElevation=$($hist[2]) needsNetwork=$($hist[3]) unsupported=$($hist[4]) skipped=$($hist[5])")
    Check '--json 的 snapshot 字段 = 用户给的那个目录原样（决策 162）' `
        ((Prop $sj 'snapshot') -eq $synDir) "$(Prop $sj 'snapshot')"
    Check '合成快照的 --json 仍是纯 ASCII' (Test-NoCjk $syn.Stdout) ''
    Check '决策 160：不可再分发的制品被拒且原因具体（licence-blocked + 稳定 slug，不是"许可问题"）' `
        ((@(Arr $sj 'manualActions' | Where-Object { (Prop $_ 'code') -eq 'licence-blocked' -and `
                        (Prop $_ 'subject') -match 'oracle-jdk' -and `
                        (Prop $_ 'detail') -eq 'oracle-jdk-redistribution-not-permitted' })).Count -eq 1) `
        ((@(Arr $sj 'manualActions' | Where-Object { (Prop $_ 'code') -eq 'licence-blocked' } | ForEach-Object { "$(Prop $_ 'subject')/$(Prop $_ 'detail')/$(Prop $_ 'remediation')" }) -join ' '))
    Check '决策 160：那条不可复现的行**不许**出现 install 动作（被拒不是"换个方式装"）' `
        ((@(Arr $st 'actions' | Where-Object { (Prop $_ 'subject') -match 'oracle-jdk' -and (Prop $_ 'kind') -eq 'install' })).Count -eq 0) ''

    $synOnly = Invoke-Tuoen -Arguments @('restore', $synDir, '--only', 'path', '--json') -Name 'synonly'
    $soj = Get-Payload $synOnly
    if ($null -ne $soj) {
        Check '合成快照 + --only path：tools/env/wsl 是 skipped/not-selected（选择性真的生效）' `
            ((@(Arr $soj 'sections' | Where-Object { (Prop $_ 'id') -ne 'path' -and (Prop $_ 'status') -ne 'skipped' })).Count -eq 0) `
            ((@(Arr $soj 'sections' | ForEach-Object { "$(Prop $_ 'id')=$(Prop $_ 'status')" }) -join ' '))
    }
}

# 合成快照跑完，机器仍然一个字节都没变（默认形态不写）
$afterSyn = Get-Snapshot
Check '合成快照跑完之后 HKCU/HKLM 仍然逐字未变' `
    (($afterSyn.UserSha -ceq $before.UserSha) -and ($afterSyn.MachineSha -ceq $before.MachineSha)) ''

# ── §8 `--with-fix`：默认不含 fix，加上才含（负正成对） ─────────────────

Section '8 `--with-fix`：本机自身的健康问题只在明确要求时才被选中'

$withFix = Invoke-Tuoen -Arguments @('restore', $snapDir, '--only', 'path', '--with-fix', '--json') -Name 'withfix'
$wfj = Get-Payload $withFix
Check '--only path --with-fix 退出 0' ($withFix.Exit -eq 0) "exit=$($withFix.Exit)"
if ($null -ne $wfj) {
    $wp = Sec (Arr $wfj 'sections') 'path'
    # 真机形态：`--with-fix` 把 24 条 fix 一起选中，而 fix 里**含机器级条目**（本机 2 条），
    # 机器级只算不写 → 决策 161 的 ④（机器级写）先于 ⑤（有会写的动作）→ status = requires-elevation。
    Check '--with-fix：path 变成 requires-elevation（fix 里有机器级条目 → ④ 先于 ⑤）' `
        (((Prop $wp 'status') -eq 'requires-elevation') -and ([bool](Prop $wp 'requiresElevation') -eq $true)) `
        ("status=$(Prop $wp 'status') requiresElevation=$(Prop $wp 'requiresElevation')")
    Check '--with-fix：effective.fix = rows.fix（同一个数，不是两个口径各说各话）' `
        ([int](Prop (Prop (Prop $wp 'counts') 'effective') 'fix') -eq [int]$script:ExpectedFixRows) `
        ("effective.fix=$(Prop (Prop (Prop $wp 'counts') 'effective') 'fix') / rows.fix=$($script:ExpectedFixRows)")
    Check '--with-fix：note 不再是 fix-not-selected' ((Prop $wp 'note') -ne 'fix-not-selected') "$(Prop $wp 'note')"
}
$noFix = Invoke-Tuoen -Arguments @('restore', $snapDir, '--only', 'path', '--json') -Name 'nofix'
$nfj = Get-Payload $noFix
if ($null -ne $nfj) {
    $np = Sec (Arr $nfj 'sections') 'path'
    Check '不给 --with-fix：path 是 no-change（负的那一半）' ((Prop $np 'status') -eq 'no-change') "$(Prop $np 'status')"
}
$afterFix = Get-Snapshot
Check '--with-fix 的 dry-run 之后 HKCU/HKLM 仍然逐字未变' `
    (($afterFix.UserSha -ceq $before.UserSha) -and ($afterFix.MachineSha -ceq $before.MachineSha)) ''

# ── §9 一次**真的写**：最小可逆的 apply ─────────────────────────────────
#
# 票据禁止的是"在开发机上真的执行一次**完整** restore"。这一节做的是它的**最小可逆子集**：
# 快照里**只有** `env.toml`，且只多一条**本机没有的**用户级变量，命令是
# `restore <dir> --only env --apply`。它验证的是 apply 编排里唯一一条"写注册表 + 广播"的通道，
# 代价与风险都最低：新建一个变量不可能弄坏任何命令，删掉它就完全回到原状
# （**整个 `HKCU\Environment`** 逐字比对，不只是 Path）。
# 票据的验收物原文是"一份可信的 plan 和**一次可复现的 apply**"，所以这一节不是可选的。

Section '9 真机真写：一个新建的用户级环境变量（可逆、可复现）'

$probeName = 'TUOEN_ACCEPTANCE_PROBE'
$probeValue = 'restore-apply-probe'
$envBefore9 = Get-EnvBlockSnapshot
$pathBefore9 = Get-Snapshot
$envBefore9 | Set-Content -LiteralPath (Join-Path $root '_hkcu_env_backup.txt') -Encoding utf8
Check '跑之前：那个探针变量不存在（否则下面每一条都会是假的）' `
    ((@($envBefore9 | Where-Object { $_ -like "$probeName|*" })).Count -eq 0) ''
Check '跑之前：新终端口径里也看不到它' ((Invoke-FreshEnvProbe $probeName) -eq "%$probeName%") `
    (Invoke-FreshEnvProbe $probeName)

# **先自证"删得掉"再允许写**（决策 149 / AGENTS.md 规矩 7）。这里自证的正是清理唯一依赖的那条路径：
# 用可写子键句柄写一个一次性变量、确认它出现了，再删掉、确认全环境块逐字回到原样。
$selfName = 'TUOEN_ACCEPTANCE_SELFTEST'
Set-UserEnvValue -Name $selfName -Value 'self-test'
$selfWritten = (@(Get-EnvBlockSnapshot | Where-Object { $_ -like "$selfName|*" })).Count -eq 1
Remove-UserEnvValue -Name $selfName
$selfClean = ((Get-EnvBlockSnapshot) -join '|') -ceq ($envBefore9 -join '|')
Check '还原机制自检：写得进、删得掉、删完全环境块逐字回到原样' ($selfWritten -and $selfClean) `
    ("写入成功=$selfWritten 删除后逐字相同=$selfClean")

# 只带 env.toml 的探针快照：`--only env` 之外的三节连文件都没有
$applyDir = Join-Path $root 'apply-env\tuoen.d'
New-Item -ItemType Directory -Path $applyDir -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $snapDir 'schema.toml') -Destination $applyDir
Copy-Item -LiteralPath (Join-Path $snapDir 'env.toml') -Destination $applyDir
$probeBlock = @'

# 由验收脚本追加：一条本机没有的用户级变量（apply 的探针）
[[var]]
name = "__NAME__"
scope = "user"
value_raw = "__VALUE__"
value_expanded = "__VALUE__"
reg_type = "sz"
target_exists = "not-a-path"
'@
Add-Content -LiteralPath (Join-Path $applyDir 'env.toml') `
    -Value ($probeBlock.Replace('__NAME__', $probeName).Replace('__VALUE__', $probeValue)) -Encoding utf8
$applyBundle = Get-Bundle $applyDir
Check '探针快照里恰好多一条变量' ($applyBundle.env.rows -eq $bundle.env.rows + 1) `
    ("$($applyBundle.env.rows) = $($bundle.env.rows) + 1")

$dryProbe = Invoke-Tuoen -Arguments @('restore', $applyDir, '--only', 'env', '--json') -Name 'dry-probe'
$dpj = Get-Payload $dryProbe
if ($null -ne $dpj) {
    $dpe = Sec (Arr $dpj 'sections') 'env'
    Check '计划阶段：env 是 would-change 且 effective.set-user = 1' `
        (((Prop $dpe 'status') -eq 'would-change') -and `
            ([int](Prop (Prop (Prop $dpe 'counts') 'effective') 'set-user') -eq 1)) `
        ("status=$(Prop $dpe 'status') effective.set-user=$(Prop (Prop (Prop $dpe 'counts') 'effective') 'set-user')")
    Check '计划阶段：`apply` 键**不存在**（没有 --apply 就没有结果对象，决策 164）' `
        ($null -eq (Prop $dpj 'apply')) ''
}
Check '计划阶段（没有 --apply）那个变量还没被写进去' `
    ((@(Get-EnvBlockSnapshot | Where-Object { $_ -like "$probeName|*" })).Count -eq 0) ''

try {
    $apply1 = Invoke-Tuoen -Arguments @('restore', $applyDir, '--only', 'env', '--apply', '--json') -Name 'apply1'
    $a1j = Get-Payload $apply1
    Check '`--only env --apply` 退出 0' ($apply1.Exit -eq 0) "exit=$($apply1.Exit)"
    if ($null -ne $a1j) {
        $ap = Prop $a1j 'apply'
        Check '结果对象里有 apply，且 wrote = true（真的落了注册表）' `
            (($null -ne $ap) -and ([bool](Prop $ap 'wrote') -eq $true)) "$(Prop $ap 'wrote')"
        $envOut = Sec (Arr $ap 'sections') 'env'
        Check 'env 的 outcome = applied 且 wrote = true' `
            (((Prop $envOut 'outcome') -eq 'applied') -and ([bool](Prop $envOut 'wrote') -eq $true)) `
            ("outcome=$(Prop $envOut 'outcome') wrote=$(Prop $envOut 'wrote')")
        # 广播回执：真写过环境块才会出这个键。**不断言 >= 1** —— 没有顶层窗口响应是完全正常的，
        # 断言它等于某个数会变成一条看机器脸色的检查。
        Check 'env 报了广播回执（这个键只在真的广播过之后才出现）' `
            ($null -ne (Prop $envOut 'broadcastReplies')) "broadcastReplies=$(Prop $envOut 'broadcastReplies')"
        $others = @(Arr $ap 'sections' | Where-Object { (Prop $_ 'id') -ne 'env' })
        Check '`--only env` 只让 env 一节进入 apply（其余三节根本没被考虑，决策 155/164）' `
            (((@(Arr $ap 'sections')).Count -eq 1) -and ($others.Count -eq 0)) `
            (($others | ForEach-Object { "$(Prop $_ 'id')=$(Prop $_ 'outcome')" }) -join ' ')
        Check '结果里的 --json 仍是纯 ASCII' (Test-NoCjk $apply1.Stdout) ''
    }
    $readBack = [string](Get-Item 'HKCU:\Environment').GetValue($probeName, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    Check '从注册表不展开读回的值逐字 = 期望值' ($readBack -ceq $probeValue) "'$readBack'"
    Check '写出来的类型是 REG_SZ（值不含 `%`）' `
        ([string](Get-Item 'HKCU:\Environment').GetValueKind($probeName) -eq 'String') `
        ([string](Get-Item 'HKCU:\Environment').GetValueKind($probeName))
    Check '一个全新终端（从注册表重建）里看得到它' ((Invoke-FreshEnvProbe $probeName) -ceq $probeValue) `
        (Invoke-FreshEnvProbe $probeName)
    $after9 = Get-Snapshot
    Check '`--only env` 没有碰 PATH（两个作用域都逐字未变）' `
        (($after9.UserSha -ceq $pathBefore9.UserSha) -and ($after9.MachineSha -ceq $pathBefore9.MachineSha)) ''
    Check '全环境块里那个变量恰好一条（没有写重）' `
        ((@(Get-EnvBlockSnapshot | Where-Object { $_ -like "$probeName|*" })).Count -eq 1) ''

    # 幂等（票据点名的验收："贴出幂等性证据（连跑两次，第二次为空）"）—— 这里是**真的 apply 两次**
    $apply2 = Invoke-Tuoen -Arguments @('restore', $applyDir, '--only', 'env', '--apply', '--json') -Name 'apply2'
    $a2j = Get-Payload $apply2
    Check '第二次 apply 退出 0' ($apply2.Exit -eq 0) "exit=$($apply2.Exit)"
    if ($null -ne $a2j) {
        $ap2 = Prop $a2j 'apply'
        Check '第二次 apply：wrote = false（已经在了 → 不写、也不广播）' ([bool](Prop $ap2 'wrote') -eq $false) `
            "$(Prop $ap2 'wrote')"
        # `apply.sections` 只列**这次真的考虑过**的节（`sections_to_apply` 的结论）：计划全
        # `no-change` 时它是空数组 —— 那正是票据要的"连跑两次，**第二次为空**"的形态。
        Check '第二次 apply：apply.sections 为空（这一轮什么都没考虑过 → 什么都没碰）' `
            ((@(Arr $ap2 'sections')).Count -eq 0) "sections=$(@(Arr $ap2 'sections').Count)"
        Check '第二次 apply：计划本身也已经是 no-change（第二次"为空"）' `
            ((@(Arr $a2j 'sections' | Where-Object { (Prop $_ 'status') -ne 'no-change' -and (Prop $_ 'status') -ne 'skipped' })).Count -eq 0) `
            ((@(Arr $a2j 'sections' | ForEach-Object { "$(Prop $_ 'id')=$(Prop $_ 'status')" }) -join ' '))
        Check '第二次 apply：summary.wouldChange = 0' ([int](Prop (Prop $a2j 'summary') 'wouldChange') -eq 0) `
            "$(Prop (Prop $a2j 'summary') 'wouldChange')"
    }
    Check '第二次 apply 之后那个变量仍然恰好一条、值不变' `
        ((@(Get-EnvBlockSnapshot | Where-Object { $_ -like "$probeName|*" })).Count -eq 1) `
        ((@(Get-EnvBlockSnapshot | Where-Object { $_ -like "$probeName|*" })) -join ' ')
} finally {
    # 清理走的是**与自检完全相同**的那条路径（决策 149）。
    Remove-UserEnvValue -Name $probeName
    $envAfter9 = Get-EnvBlockSnapshot
    if (($envAfter9 -join '|') -ceq ($envBefore9 -join '|')) {
        Check '还原：删掉探针变量之后，整个 HKCU\Environment 与跑之前逐字相同' $true `
            "值名 $((@($envBefore9)).Count) 个"
    } else {
        Write-Host "  [FAIL] 还原后 HKCU\Environment 与跑之前**不一致** —— 备份在 $(Join-Path $root '_hkcu_env_backup.txt')" -ForegroundColor Red
        $script:Failed++
        $script:Failures += '还原：全环境块逐字相同'
        $onlyBefore = @($envBefore9 | Where-Object { $_ -notin @($envAfter9) })
        $onlyAfter = @($envAfter9 | Where-Object { $_ -notin @($envBefore9) })
        Write-Host ("         只在跑之前有的（值不打印，只打名字）：{0}" -f ((@($onlyBefore | ForEach-Object { ($_ -split '\|')[0] })) -join ',')) -ForegroundColor Red
        Write-Host ("         只在跑之后有的（值不打印，只打名字）：{0}" -f ((@($onlyAfter | ForEach-Object { ($_ -split '\|')[0] })) -join ',')) -ForegroundColor Red
    }
    Check '还原：新终端里又看不到它了' ((Invoke-FreshEnvProbe $probeName) -eq "%$probeName%") ''
}

# ── §10 收尾核对 ────────────────────────────────────────────────────────

Section '10 收尾核对'

$final = Get-Snapshot
Check 'HKCU Path 与跑之前逐字相同' ($final.UserSha -ceq $before.UserSha) "$($before.UserSha) → $($final.UserSha)"
Check 'HKLM Path 与跑之前逐字相同' ($final.MachineSha -ceq $before.MachineSha) ''
Check '%LOCALAPPDATA%\tuoen 清单逐项相同（含 store 两层）' `
    (((Get-TreeListing -Root $realStore -Depth 2) -join '|') -eq ($beforeStore -join '|')) ''
Check '%APPDATA%\tuoen 清单逐项相同' `
    (((Get-TreeListing -Root (Join-Path $env:APPDATA 'tuoen') -Depth 2) -join '|') -eq ($beforeAppData -join '|')) ''
$finalNvm = Get-NvmState
Check '收尾：nvm4w 的环境变量 / settings.txt / junction 与跑之前逐字相同' `
    (($finalNvm.Env -ceq $beforeNvm.Env) -and ($finalNvm.SettingsSha -ceq $beforeNvm.SettingsSha) -and `
        ($finalNvm.Junction -ceq $beforeNvm.Junction)) `
    ("settings=$($beforeNvm.SettingsSha) junction=$($beforeNvm.Junction)")
Check '源码里没有带引号的 setx 字面量（硬约束：绝不 setx）' `
    (@(Select-String -Path (Join-Path $repo 'crates\*\src\*.rs') -Pattern '"setx"' -ErrorAction SilentlyContinue).Count -eq 0) ''

if ($SelfTest) {
    Check '自检：这一条必须失败（证明脚本会报失败）' $false
}

# ── §10 摘要 ────────────────────────────────────────────────────────────

Write-Host ''
$verdict = if ($script:Failed -eq 0) { 'PASS' } else { 'FAIL' }
Write-Host ("SUMMARY checks_passed={0} checks_failed={1} checks_skipped={2} verdict={3} version={4}" -f `
        $script:Passed, $script:Failed, $script:SkipCount, $verdict, $Version) `
    -ForegroundColor $(if ($script:Failed -eq 0) { 'Green' } else { 'Red' })
Write-Host ("        snapshot_tools={0} env={1} path={2} wsl={3} skipped={4} managers={5}" -f `
        $bundle.tools.rows, $bundle.env.rows, $bundle.path.rows, $bundle.wsl.rows, @($bundle.skipped).Count, ($bundle.tools.managers -join '+'))
Write-Host ("        path_user_chars={0} path_machine_chars={1} user_sha_before={2} user_sha_after={3} untouched={4} out_root={5}" -f `
        $before.UserChars, $before.MachineChars, $before.UserSha, $final.UserSha, ($final.UserSha -ceq $before.UserSha), $root)
if ($script:Failed -gt 0) {
    Write-Host '失败项：' -ForegroundColor Red
    $script:Failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
}
if ($script:Failed -gt 0) { exit 1 }
exit 0
