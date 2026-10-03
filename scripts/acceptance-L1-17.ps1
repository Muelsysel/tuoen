<#
.SYNOPSIS
    验收脚本：票据 #17 `tuoen capture --only globals` / `--only configs`。

.DESCRIPTION
    票据的验收标准：
      ① `cargo test --workspace` 全绿、clippy `-D warnings` 干净；
      ② **在作者本机上真实跑一次** `tuoen capture --only globals` 与 `--only configs`，
         把两份产出**以及跳过清单**贴进 issue 评论；
      ③ **确认 `.m2/settings.xml` 被跳过且原因具体**（这是本票最重要的证据）；
      ④ 确认 `.m2/repository` 等缓存目录出现在跳过清单里；
      ⑤ 把 `globals.toml` 的 npm 清单与取证报告对照（预期 7 个包）。

    **这条脚本一个字节都不写**（`capture` 是只读命令）：每一步之后都核对"这台机器没变"——
    真实配置文件的 sha256、`HKCU\Environment` 的全文快照、JetBrains 目录清单、
    `%LOCALAPPDATA%\tuoen` 与 `%APPDATA%\tuoen` 的目录树。

    **期望值全部由脚本自己算出来**（`node -v` / `npm config get prefix` / `npm ls -g` /
    `pip --version` / `pip list` / `git config --show-origin` / 自己算的 sha256 / 自己数的缓存目录），
    不与产品自己的输出对照 —— 否则"两边一起错"照样绿。

    **一条不能失败的验收不是验收**：`-SelfTest` 会让最后一条检查故意失败（exit 1）。

.PARAMETER SelfTest
    故意让最后一条检查失败，用来证明这条脚本**会**报失败（exit 1）。

.PARAMETER SkipBuild
    跳过 §0 的 release 构建（只在已经构建过、且想快速重跑时用）。

.EXAMPLE
    pwsh -File scripts/acceptance-L1-17.ps1
    pwsh -File scripts/acceptance-L1-17.ps1 -SelfTest   # 必须 exit 1
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
$root = Join-Path $env:TEMP 'tuoen-acceptance-L1-17'
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

# 原文 + 类型。**必须 `DoNotExpandEnvironmentNames`**：展开是不可逆的信息损失。
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

function Get-PathSnapshot {
    $u = Get-PathValue 'user'
    $m = Get-PathValue 'machine'
    [pscustomobject]@{
        UserRaw   = $u.Raw; UserKind = $u.Kind; UserSha = Get-Sha16 $u.Raw
        MachRaw   = $m.Raw; MachKind = $m.Kind; MachSha = Get-Sha16 $m.Raw
    }
}

# `HKCU\Environment` 的**全文**快照（值名 + 类型 + 不展开的原文），按名字排序。
function Get-EnvBlockSnapshot {
    $item = Get-Item 'HKCU:\Environment'
    @(@($item.GetValueNames()) | Sort-Object | ForEach-Object {
            "$_|$([string]$item.GetValueKind($_))|$([string]$item.GetValue($_, '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames))"
        })
}

# 一个目录树的清单（相对路径 + 大小）。`DirectoryInfo` **没有** `Length`，
# 而 `Set-StrictMode -Latest` 下访问不存在的属性是终止错误。
function Get-TreeListing {
    param([string]$Root, [int]$Depth = 2)
    if (-not (Test-Path -LiteralPath $Root)) { return @('<absent>') }
    @(Get-ChildItem -LiteralPath $Root -Force -Recurse -Depth $Depth -ErrorAction SilentlyContinue |
            Sort-Object FullName | ForEach-Object {
                $size = if ($_.PSIsContainer) { 'dir' } else { $_.Length }
                "$($_.FullName.Substring($Root.Length))|$size"
            })
}

function Invoke-Tuoen {
    param([string[]]$Arguments, [hashtable]$ExtraEnv = @{})
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $exe
    foreach ($a in $Arguments) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    # 子进程的 PATH = **新终端口径**（从注册表重建），而不是本脚本继承来的那份。
    $psi.EnvironmentVariables['PATH'] = $script:Pristine
    $psi.EnvironmentVariables['Path'] = $script:Pristine
    foreach ($k in $ExtraEnv.Keys) { $psi.EnvironmentVariables[$k] = $ExtraEnv[$k] }
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stdout = $proc.StandardOutput.ReadToEnd()
    $stderr = $proc.StandardError.ReadToEnd()
    $proc.WaitForExit()
    [pscustomobject]@{ Exit = $proc.ExitCode; Stdout = $stdout; Stderr = $stderr; Args = ($Arguments -join ' ') }
}

# 用**新终端口径**的 PATH 跑一个外部命令（期望值的独立来源）。
function Invoke-External {
    param([string]$Program, [string[]]$Arguments)
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $Program
    foreach ($a in $Arguments) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables['PATH'] = $script:Pristine
    $psi.EnvironmentVariables['Path'] = $script:Pristine
    try { $proc = [System.Diagnostics.Process]::Start($psi) } catch { return $null }
    $stdout = $proc.StandardOutput.ReadToEnd()
    $stderr = $proc.StandardError.ReadToEnd()
    $proc.WaitForExit()
    [pscustomobject]@{ Exit = $proc.ExitCode; Stdout = $stdout; Stderr = $stderr }
}

function Get-Json {
    param([object]$Result)
    try { $Result.Stdout | ConvertFrom-Json } catch { $null }
}

function Test-NoCjk {
    param([string]$Text)
    -not [bool]($Text -match '[^\x00-\x7F]')
}

# 取一个可能不存在的属性（StrictMode 下访问不存在的属性是终止错误）。
# **不能**写 `$Obj.PSObject.Properties.Name`：空对象上成员枚举会炸。
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

function Get-Payload {
    param([object]$Result)
    $j = Get-Json $Result
    if ($null -eq $j) { return $null }
    Prop $j 'data'
}

# 数一个"可能不在"的数组：`@($null).Count` 是 **1**，所以一律先滤 `$null`。
function Arr {
    param([object]$Obj, [string]$Name)
    @(Prop $Obj $Name | Where-Object { $null -ne $_ })
}

# 按字段取一行。**找不到就返回 `$null`**，让断言去报 FAIL（而不是让脚本中止）。
function Find {
    param([object]$Rows, [string]$Field, [string]$Value)
    $hit = @(@($Rows) | Where-Object { (Prop $_ $Field) -eq $Value })
    if ($hit.Count -eq 0) { return $null }
    $hit[0]
}

function FindLike {
    param([object]$Rows, [string]$Field, [string]$Pattern)
    $hit = @(@($Rows) | Where-Object { [string](Prop $_ $Field) -like $Pattern })
    if ($hit.Count -eq 0) { return $null }
    $hit[0]
}

# 路径归一化：只比较"是不是同一个文件"，不比较分隔符风格（git 报正斜杠）。
function Normalize-Path {
    param([string]$Path)
    if (-not $Path) { return '' }
    ($Path -replace '/', '\').TrimEnd('\').ToLowerInvariant()
}

# ── 独立解析：`tuoen.d/` 用 Python 的 `tomllib` ───────────────────────────

$PY = Join-Path $env:LOCALAPPDATA 'Programs\Python\Python312\python.exe'
if (-not (Test-Path -LiteralPath $PY)) {
    throw "找不到真实 Python：$PY（不接受 WindowsApps 的别名）"
}

$helper = Join-Path $root '_l117.py'
@'
import json, os, sys, tomllib

d = sys.argv[1]

def load(name):
    p = os.path.join(d, name)
    if not os.path.exists(p):
        return None
    with open(p, "rb") as fh:
        return tomllib.load(fh)

cfg = load("configs.toml")
glob = load("globals.toml")
sch = load("schema.toml")
skip = load("skipped.toml")
tools = load("tools.toml")
env = load("env.toml")
path = load("path.toml")
wsl = load("wsl.toml")

config_rows = (cfg or {}).get("config", [])
global_rows = (glob or {}).get("global", [])
skip_rows = (skip or {}).get("skipped", [])

out = {
    "files": sorted(n for n in os.listdir(d) if n.endswith(".toml")),
    "sections": list((sch or {}).get("sections", [])),
    "schema_version": (sch or {}).get("schema_version"),
    # 四个旧 section 的行数：新 section 不许改动它们（不回归）。
    "tools": {"rows": len((tools or {}).get("tool", []))},
    "env": {"rows": len((env or {}).get("var", []))},
    "path": {"rows": len((path or {}).get("entry", []))},
    "wsl": {"rows": len((wsl or {}).get("distribution", []))},
    "config": [
        {
            "path": r.get("path"),
            "kind": r.get("kind"),
            "layer": r.get("layer"),
            "captured": r.get("captured"),
            "bytes": r.get("bytes"),
            "hash": r.get("content_hash"),
            "skip": r.get("skip_reason"),
            "keys": sorted(r.keys()),
        }
        for r in config_rows
    ],
    "git": (cfg or {}).get("git"),
    "globals": [
        {
            "tool": r.get("tool"),
            "tool_version": r.get("tool_version"),
            "prefix": r.get("prefix"),
            "inside": r.get("prefix_inside_version_dir"),
            "error": r.get("enumerate_error"),
            "keys": sorted(r.keys()),
            "packages": sorted(
                [{"name": p.get("name"), "version": p.get("version")} for p in r.get("packages", [])],
                key=lambda p: p["name"],
            ),
        }
        for r in global_rows
    ],
    "skipped": [
        {"section": r.get("section"), "scope": r.get("scope"), "name": r.get("name"),
         "kind": r.get("kind"), "reason": r.get("reason")}
        for r in skip_rows
    ],
}
print(json.dumps(out, ensure_ascii=False))
'@ | Set-Content -Path $helper -Encoding utf8 -NoNewline

function Get-Bundle {
    param([string]$Dir)
    # **产品没落盘时不许把脚本炸掉**：那样最该被看见的回归会变成一段 traceback，
    # 连一条 FAIL 都打不出来（#16 的教训：脚本自己"消失"比报红更坏）。
    # 返回一个"什么都没有"的骨架，让每一条断言各自去报 FAIL。
    $empty = [pscustomobject]@{
        files    = @()
        sections = @()
        config   = @()
        globals  = @()
        skipped  = @()
        git      = $null
        tools    = [pscustomobject]@{ rows = -1 }
        env      = [pscustomobject]@{ rows = -1 }
        path     = [pscustomobject]@{ rows = -1 }
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

# ── 1. 前置：独立事实（期望值的唯一来源） ────────────────────────────────

Section '1. 独立事实（脚本自己从注册表 / 磁盘 / 工具问出来）'

$u = Get-PathValue 'user'
$m = Get-PathValue 'machine'
$script:Pristine = ($m.Raw + ';' + $u.Raw)
Write-Host ("  PATH 口径：机器级 {0} + 用户级 {1} = {2} 字符（注册表原文，不展开）" -f $m.Raw.Length, $u.Raw.Length, $script:Pristine.Length)

$home2 = $env:USERPROFILE
$realConfigs = @(
    @{ Path = "$home2\.gitconfig"; Kind = 'git'; Layer = 'global' },
    @{ Path = "$home2\.npmrc"; Kind = 'npm'; Layer = $null },
    @{ Path = "$home2\.m2\settings.xml"; Kind = 'maven'; Layer = $null },
    @{ Path = "$home2\.docker\config.json"; Kind = 'docker'; Layer = $null },
    @{ Path = "$home2\.ssh\config"; Kind = 'ssh'; Layer = $null },
    @{ Path = "$home2\.wslconfig"; Kind = 'wsl'; Layer = $null },
    @{ Path = "$env:APPDATA\Code\User\settings.json"; Kind = 'vscode'; Layer = $null }
)
$presentConfigs = @($realConfigs | Where-Object { Test-Path -LiteralPath $_.Path -PathType Leaf })
Write-Host ("  真实配置文件存在 {0}/{1} 个" -f $presentConfigs.Count, $realConfigs.Count)
foreach ($c in $presentConfigs) {
    Write-Host ("    {0,8}  {1}  {2}" -f (Get-Item -LiteralPath $c.Path).Length, (Get-FileHash -LiteralPath $c.Path -Algorithm SHA256).Hash.Substring(0, 12).ToLowerInvariant(), $c.Path)
}

# git 的两层：路径与身份都问 git 自己。
$sysGit = Invoke-External 'git' @('config', '--system', '--list', '--show-origin')
$globGit = Invoke-External 'git' @('config', '--global', '--list', '--show-origin')
$sysGitPath = $null
$globGitPath = $null
if ($sysGit -and $sysGit.Exit -eq 0 -and $sysGit.Stdout.Trim()) {
    $sysGitPath = (($sysGit.Stdout -split "`n")[0] -split "`t")[0] -replace '^file:', ''
}
if ($globGit -and $globGit.Exit -eq 0 -and $globGit.Stdout.Trim()) {
    $globGitPath = (($globGit.Stdout -split "`n")[0] -split "`t")[0] -replace '^file:', ''
}
$gitIdentity = @()
foreach ($pair in @(@('system', $sysGit), @('global', $globGit))) {
    if ($null -eq $pair[1]) { continue }
    foreach ($line in ($pair[1].Stdout -split "`n")) {
        # **不能**用 `^\S*\t` 去配来源：`file:C:/Program Files/Git/etc/gitconfig` 里**有空格**，
        # 那样一条身份都配不到（而"没配到"恰好等于本机的真值 `missing` —— 一个不会红的假阴性）。
        # 按**第一个 tab** 切开，来源部分随便它有没有空格。
        $parts = $line -split "`t", 2
        if ($parts.Count -eq 2 -and $parts[1] -match '^(user\.name|user\.email)=(.*)$') {
            $gitIdentity += [pscustomobject]@{ Layer = $pair[0]; Key = $Matches[1]; Value = $Matches[2] }
        }
    }
}
Write-Host ("  git 系统级配置：{0}" -f $sysGitPath)
Write-Host ("  git 全局级配置：{0}" -f $globGitPath)
if ($gitIdentity.Count -eq 0) {
    Write-Host '  git 身份：两层都没有 user.name / user.email（本机事实）'
    $expectedIdentity = 'missing'
} else {
    $expectedIdentity = $gitIdentity[-1].Layer   # 全局覆盖系统
    foreach ($i in $gitIdentity) { Write-Host ("  git 身份：{0} 层 {1} = {2}" -f $i.Layer, $i.Key, $i.Value) }
}

# 真实 PAT：只取前 8 字符与全长，**绝不打印**。
$settings = "$home2\.m2\settings.xml"
$patPrefix = $null
$patLength = 0
if (Test-Path -LiteralPath $settings) {
    $text = Get-Content -LiteralPath $settings -Raw
    $mm = [regex]::Match($text, 'glpat-[A-Za-z0-9_\-]{8,}')
    if ($mm.Success) { $patPrefix = $mm.Value.Substring(0, 8); $patLength = $mm.Value.Length }
}
Check '独立事实：本机 .m2/settings.xml 里真的有一个 glpat- 形状的材料（本票的靶子）' ($null -ne $patPrefix) "长度 $patLength（值不打印）"

# npm / pip 的独立枚举。
$nodeV = Invoke-External 'node' @('-v')
$npmPrefix = Invoke-External 'cmd.exe' @('/C', 'npm.cmd', 'config', 'get', 'prefix')
$npmList = Invoke-External 'cmd.exe' @('/C', 'npm.cmd', 'ls', '-g', '--json', '--depth=0', '--offline')
$pipV = Invoke-External 'pip.exe' @('--version')
$pipList = Invoke-External 'pip.exe' @('list', '--format=json', '--disable-pip-version-check')

$expNode = if ($nodeV -and $nodeV.Exit -eq 0) { $nodeV.Stdout.Trim() } else { $null }
$expPrefix = if ($npmPrefix -and $npmPrefix.Exit -eq 0) { $npmPrefix.Stdout.Trim() } else { $null }
$expNpmPackages = @()
if ($npmList -and $npmList.Exit -eq 0) {
    $parsed = $null
    try { $parsed = $npmList.Stdout | ConvertFrom-Json } catch { $parsed = $null }
    $deps = Prop $parsed 'dependencies'
    if ($null -ne $deps) {
        foreach ($p in @($deps.PSObject.Properties)) {
            $expNpmPackages += [pscustomobject]@{ Name = $p.Name; Version = [string](Prop $p.Value 'version') }
        }
    }
}
$expNpmPackages = @($expNpmPackages | Sort-Object Name)
$expPython = $null
$expPipPrefix = $null
$expPipPackages = @()
if ($pipV -and $pipV.Exit -eq 0) {
    $pv = $pipV.Stdout.Trim()
    # **只取版本号本身**（决策 172 的复核期澄清）：`(python 3.12)` 的括号与 `python` 是 pip 的
    # 排版，不是版本字符串的一部分 —— 字段名是 `tool_version`，值必须是一个版本。
    $m2 = [regex]::Match($pv, '\(python ([0-9][0-9.]*)\)')
    if ($m2.Success) { $expPython = $m2.Groups[1].Value }
    # pip 的 prefix 由**路径形状**反推（`…\Lib\site-packages\pip` 去掉尾巴）：
    # 形状不认识就**不猜**（期望值留 `$null`，断言会走 skip 而不是编一个）。
    $mFrom = [regex]::Match($pv, 'from (.+) \(python ')
    if ($mFrom.Success) {
        $pipPath = $mFrom.Groups[1].Value.Trim()
        $suffix = '\Lib\site-packages\pip'
        if ($pipPath.ToLowerInvariant().EndsWith($suffix.ToLowerInvariant())) {
            $expPipPrefix = $pipPath.Substring(0, $pipPath.Length - $suffix.Length)
        }
    }
}
if ($pipList -and $pipList.Exit -eq 0) {
    $pl = $null
    try { $pl = $pipList.Stdout | ConvertFrom-Json } catch { $pl = $null }
    foreach ($p in @($pl)) { $expPipPackages += [pscustomobject]@{ Name = $p.name; Version = $p.version } }
    $expPipPackages = @($expPipPackages | Sort-Object Name)
}
Write-Host ("  独立枚举：node={0} npmPrefix={1} npm 包={2} 个；python={3} pip 包={4} 个" -f $expNode, $expPrefix, $expNpmPackages.Count, $expPython, $expPipPackages.Count)

# 缓存目录（决策 180 的固定表）。
$cacheTable = @(
    "$env:LOCALAPPDATA\pnpm\store",
    "$env:LOCALAPPDATA\npm-cache",
    "$env:LOCALAPPDATA\pip\Cache",
    "$env:LOCALAPPDATA\uv\cache",
    "$home2\.cache\codex-runtimes",
    "$home2\.m2\repository"
)
$presentCaches = @($cacheTable | Where-Object { Test-Path -LiteralPath $_ })
Write-Host ("  缓存目录存在 {0}/{1} 个" -f $presentCaches.Count, $cacheTable.Count)

# JetBrains：只认"产品 + 版本"目录。
$jbRoot = "$env:APPDATA\JetBrains"
$jbProducts = @()
if (Test-Path -LiteralPath $jbRoot) {
    $jbProducts = @(Get-ChildItem -LiteralPath $jbRoot -Directory -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^[A-Za-z]+\d{4}\.\d+$' })
}
$jbKeyFiles = @()
foreach ($p in $jbProducts) {
    foreach ($n in @('c.kdbx', 'c.pwd', 'idea.key')) {
        $f = Join-Path $p.FullName $n
        if (Test-Path -LiteralPath $f -PathType Leaf) { $jbKeyFiles += $f }
    }
}
Write-Host ("  JetBrains 产品目录 {0} 个；密钥材料文件 {1} 个" -f $jbProducts.Count, $jbKeyFiles.Count)

$sshKeys = @()
foreach ($n in @('id_rsa', 'id_dsa', 'id_ecdsa', 'id_ed25519')) {
    $f = "$home2\.ssh\$n"
    if (Test-Path -LiteralPath $f -PathType Leaf) { $sshKeys += $f }
}
$knownHosts = "$home2\.ssh\known_hosts"

# 零副作用的基线。
$beforePath = Get-PathSnapshot
$beforeEnv = Get-EnvBlockSnapshot
$beforeConfigHash = @{}
foreach ($c in $realConfigs) { $beforeConfigHash[$c.Path] = Get-FileSha $c.Path }
$beforeTuoenLocal = Get-TreeListing "$env:LOCALAPPDATA\tuoen"
$beforeTuoenRoaming = Get-TreeListing "$env:APPDATA\tuoen"
$beforeJb = Get-TreeListing $jbRoot 3

# ── 2. `capture --only configs` ─────────────────────────────────────────

Section '2. `tuoen capture --only configs`（真机）'

$cfgDir = Join-Path $root 'configs-only'
$r = Invoke-Tuoen @('capture', '--only', 'configs', '--out', $cfgDir, '--json')
Check 'capture --only configs 退出 0' ($r.Exit -eq 0) "exit=$($r.Exit)"
if ($r.Exit -ne 0) { Write-Host "    stderr: $($r.Stderr)" }
$payload = Get-Payload $r
Check '--json 成功载荷无 CJK（中文只进人类输出）' (Test-NoCjk $r.Stdout)
$c = Get-Bundle $cfgDir
Write-Host ("  落盘：{0}" -f ($c.files -join ', '))

Check '只写 configs.toml / schema.toml / skipped.toml' `
    ((($c.files | Sort-Object) -join ',') -eq 'configs.toml,schema.toml,skipped.toml') `
    ($c.files -join ',')
Check 'schema.sections 只有 configs' ((@($c.sections) -join ',') -eq 'configs') ($c.sections -join ',')

# **空真防线**：下面有一批"每一行都…"的断言，在**一行都没有**的时候会全部通过 ——
# 那是"什么都没比"的典型形状（`AGENTS.md`「会下结论的代码」规矩 5）。先钉住"有内容"。
Check 'configs.toml 至少有 1 行（否则下面那些"每一行都…"的断言是空真）' (@($c.config).Count -ge 1) "rows=$(@($c.config).Count)"

# ③ 本票最重要的证据。
$maven = FindLike $c.config 'path' '*settings.xml'
Check '.m2/settings.xml 有一条记录' ($null -ne $maven)
if ($maven) {
    Check '.m2/settings.xml 被跳过（captured = false）' ((Prop $maven 'captured') -eq $false)
    Check '.m2/settings.xml 的 skip_reason 具体 = contains-credential-shape' `
        ((Prop $maven 'skip') -eq 'contains-credential-shape') "skip_reason=$(Prop $maven 'skip')"
    Check '.m2/settings.xml 的行里没有 content_hash / bytes（跳过的行不许假装读过）' `
        (($null -eq (Prop $maven 'hash')) -and ($null -eq (Prop $maven 'bytes')))
}

# 每个真实存在的配置文件都要有一条记录，且哈希与脚本自己算的一致。
foreach ($cf in $presentConfigs) {
    $row = Find $c.config 'path' $cf.Path
    if ($null -eq $row) {
        # 路径可能以归一化形式出现（分隔符 / 大小写），再找一次。
        $row = @(@($c.config) | Where-Object { (Normalize-Path (Prop $_ 'path')) -eq (Normalize-Path $cf.Path) })
        if ($row.Count -gt 0) { $row = $row[0] } else { $row = $null }
    }
    $label = Split-Path -Leaf $cf.Path
    Check "配置文件有记录：$label" ($null -ne $row)
    if ($null -ne $row) {
        Check "$label 的 kind = $($cf.Kind)" ((Prop $row 'kind') -eq $cf.Kind) "kind=$(Prop $row 'kind')"
        $expectSkip = ($cf.Path -eq $settings)
        if ($expectSkip) {
            Check "$label 被跳过" ((Prop $row 'captured') -eq $false)
        } else {
            Check "$label 被捕获（captured = true）" ((Prop $row 'captured') -eq $true)
            Check "$label 的 content_hash 与脚本自己算的一致" `
                ((Prop $row 'hash') -eq ('sha256:' + (Get-FileSha $cf.Path))) "产品=$(Prop $row 'hash')"
            Check "$label 的 bytes 与磁盘一致" ((Prop $row 'bytes') -eq (Get-Item -LiteralPath $cf.Path).Length)
        }
    }
}

# git 的两层（决策 183）。
$sysRow = @(@($c.config) | Where-Object { (Normalize-Path (Prop $_ 'path')) -eq (Normalize-Path $sysGitPath) })
Check 'git 系统级配置文件也在清单里（只快照 ~/.gitconfig 会丢掉整个 Git 身份）' ($sysRow.Count -eq 1)
if ($sysRow.Count -eq 1) {
    Check 'git 系统级那一行标注 layer = system' ((Prop $sysRow[0] 'layer') -eq 'system') "layer=$(Prop $sysRow[0] 'layer')"
    Check 'git 系统级的 content_hash 与脚本自己算的一致' `
        ((Prop $sysRow[0] 'hash') -eq ('sha256:' + (Get-FileSha $sysGitPath)))
}
$globRow = Find $c.config 'path' "$home2\.gitconfig"
if ($null -ne $globRow) {
    Check 'git 全局级那一行标注 layer = global' ((Prop $globRow 'layer') -eq 'global') "layer=$(Prop $globRow 'layer')"
}

$git = Prop $c 'git'
Check 'configs.toml 里有 [git] 表' ($null -ne $git)
if ($null -ne $git) {
    Check "[git].identity_source = $expectedIdentity（脚本自己从 git 问出来的）" `
        ((Prop $git 'identity_source') -eq $expectedIdentity) "产品=$(Prop $git 'identity_source')"
    Check '[git].system_config 指向 git 自己报的那个文件' `
        ((Normalize-Path (Prop $git 'system_config')) -eq (Normalize-Path $sysGitPath)) "产品=$(Prop $git 'system_config')"
    Check '[git].global_config 指向 git 自己报的那个文件' `
        ((Normalize-Path (Prop $git 'global_config')) -eq (Normalize-Path $globGitPath)) "产品=$(Prop $git 'global_config')"
    if ($expectedIdentity -eq 'missing') {
        $gitKeys = @(PropNames $git)
        Check '[git] 缺身份时不出 user_name / user_email 键（缺失就明确说缺失）' `
            (($gitKeys -notcontains 'user_name') -and ($gitKeys -notcontains 'user_email')) `
            ($gitKeys -join ',')
    }
}

# ④ 缓存目录。
$skipRows = @(Prop $c 'skipped')
Check '跳过清单至少有 1 条（否则下面那些"每一条跳过项都…"的断言是空真）' ($skipRows.Count -ge 1) "rows=$($skipRows.Count)"
foreach ($cache in $presentCaches) {
    $hit = @($skipRows | Where-Object {
            (Prop $_ 'kind') -eq 'cache-directory' -and (Normalize-Path (Prop $_ 'name')) -eq (Normalize-Path $cache)
        })
    Check "缓存目录在跳过清单里：$(Split-Path -Leaf $cache)" ($hit.Count -ge 1)
}
Check '缓存目录的跳过项不许带体积（我们不走进 8 GB 的目录）' `
    (@($skipRows | Where-Object { (Prop $_ 'kind') -eq 'cache-directory' -and (Prop $_ 'reason') -match '\d+\s*(MB|GB|字节)' }).Count -eq 0)

# `~/.ssh` 与 JetBrains。
if (Test-Path -LiteralPath $knownHosts -PathType Leaf) {
    $kh = @($skipRows | Where-Object { (Prop $_ 'name') -like '*known_hosts*' })
    Check 'known_hosts 在跳过清单里' ($kh.Count -ge 1)
    if ($kh.Count -ge 1) {
        Check 'known_hosts 的 kind = host-keys-not-captured' ((Prop $kh[0] 'kind') -eq 'host-keys-not-captured') "kind=$(Prop $kh[0] 'kind')"
    }
}
foreach ($k in $sshKeys) {
    $hit = @($skipRows | Where-Object { (Prop $_ 'name') -like "*$(Split-Path -Leaf $k)*" })
    Check "ssh 私钥在跳过清单里：$(Split-Path -Leaf $k)" ($hit.Count -ge 1)
}
foreach ($k in $jbKeyFiles) {
    $hit = @($skipRows | Where-Object { (Prop $_ 'name') -like "*$(Split-Path -Leaf $k)*" })
    Check "JetBrains 密钥材料在跳过清单里：$(Split-Path -Leaf $k)" ($hit.Count -ge 1)
}
foreach ($p in $jbProducts) {
    $row = @(@($c.config) | Where-Object { (Normalize-Path (Prop $_ 'path')) -eq (Normalize-Path $p.FullName) })
    Check "JetBrains 产品目录有标记行：$($p.Name)" ($row.Count -eq 1)
    if ($row.Count -eq 1) {
        Check "标记行 $($p.Name)：captured = true 且没有 bytes / content_hash（目录不是文件）" `
            (((Prop $row[0] 'captured') -eq $true) -and ($null -eq (Prop $row[0] 'hash')) -and ($null -eq (Prop $row[0] 'bytes')))
        Check "标记行 $($p.Name)：kind = jetbrains" ((Prop $row[0] 'kind') -eq 'jetbrains') "kind=$(Prop $row[0] 'kind')"
    }
}

# 决策 184：captured 与 skip_reason 是两个互斥子集。
$badRows = @()
foreach ($row in @($c.config)) {
    $cap = Prop $row 'captured'
    $skip = Prop $row 'skip'
    $hash = Prop $row 'hash'
    $kind = Prop $row 'kind'
    if ($cap -eq $true) {
        if ($null -ne $skip) { $badRows += "captured+skip:$(Prop $row 'path')" }
        if ($null -eq $hash -and $kind -ne 'jetbrains') { $badRows += "captured 无 hash:$(Prop $row 'path')" }
    } elseif ($cap -eq $false) {
        if ($null -eq $skip) { $badRows += "跳过无原因:$(Prop $row 'path')" }
    } else {
        $badRows += "captured 不是 bool:$(Prop $row 'path')"
    }
}
Check '每一行都满足：captured ⟺ 有哈希（jetbrains 标记行除外）、跳过 ⟺ 有具体原因' ($badRows.Count -eq 0) ($badRows -join '; ')
Check '跳过的行都有中文原因（reason 字段）' `
    (@($skipRows | Where-Object { -not (Prop $_ 'reason') }).Count -eq 0)
Check '跳过项不许写材料（reason 里不出现 glpat- / 令牌片段）' `
    (@($skipRows | Where-Object { (Prop $_ 'reason') -match 'glpat-' }).Count -eq 0)

# ── 3. `capture --only globals` ─────────────────────────────────────────

Section '3. `tuoen capture --only globals`（真机）'

$globDir = Join-Path $root 'globals-only'
$r2 = Invoke-Tuoen @('capture', '--only', 'globals', '--out', $globDir, '--json')
Check 'capture --only globals 退出 0' ($r2.Exit -eq 0) "exit=$($r2.Exit)"
if ($r2.Exit -ne 0) { Write-Host "    stderr: $($r2.Stderr)" }
Check '--json 成功载荷无 CJK' (Test-NoCjk $r2.Stdout)
$g = Get-Bundle $globDir
Write-Host ("  落盘：{0}" -f ($g.files -join ', '))
Check '只写 globals.toml / schema.toml（globals 不产生跳过项，所以没有 skipped.toml）' `
    ((($g.files | Sort-Object) -join ',') -eq 'globals.toml,schema.toml') ($g.files -join ',')
Check 'schema.sections 只有 globals' ((@($g.sections) -join ',') -eq 'globals') ($g.sections -join ',')

$npmRow = Find $g.globals 'tool' 'npm'
Check 'globals.toml 至少有 1 行（空真防线）' (@($g.globals).Count -ge 1) "rows=$(@($g.globals).Count)"
Check 'globals.toml 里有 npm 行' ($null -ne $npmRow)
if ($null -ne $npmRow) {
    # 键在不在，要看 **TOML 原文里的键**（`keys` 是 Python 侧 `sorted(r.keys())`），
    # 不是看规范化后的对象：那个 dict 每个字段都在，`PropNames` 永远为真 = 空真。
    Check 'npm 行永远有 tool_version 键（按 TOML 原文的键判）' ((Prop $npmRow 'keys') -contains 'tool_version')
    Check "npm 的 tool_version = $expNode（脚本自己跑 node -v）" `
        ((Prop $npmRow 'tool_version') -eq $expNode) "产品=$(Prop $npmRow 'tool_version')"
    Check "npm 的 prefix = $expPrefix（脚本自己跑 npm config get prefix）" `
        ((Prop $npmRow 'prefix') -eq $expPrefix) "产品=$(Prop $npmRow 'prefix')"
    Check 'npm 的 prefix_inside_version_dir = true（本机 prefix 是符号链接，指向 …\nvm\v24.19.0）' `
        ((Prop $npmRow 'inside') -eq $true) "产品=$(Prop $npmRow 'inside')"
    Check 'npm 行没有 enumerate_error（本机枚举成功）' ($null -eq (Prop $npmRow 'error')) "error=$(Prop $npmRow 'error')"
    $prodPkgs = @(Prop $npmRow 'packages')
    $prodNames = @($prodPkgs | ForEach-Object { [string](Prop $_ 'name') })
    $expNames = @($expNpmPackages | ForEach-Object { $_.Name })
    Check "npm 包集合与脚本自己跑 npm ls -g 得到的一致（$($expNames.Count) 个）" `
        ((($prodNames | Sort-Object) -join ',') -eq (($expNames | Sort-Object) -join ',')) `
        "产品=$(($prodNames | Sort-Object) -join ',') 脚本=$(($expNames | Sort-Object) -join ',')"
    Check 'npm 包按 name 排序（确定性是幂等性的一部分）' `
        ((($prodNames -join ',') -eq (($prodNames | Sort-Object) -join ',')))
    $verBad = @()
    foreach ($p in $expNpmPackages) {
        $row = Find $prodPkgs 'name' $p.Name
        if ($null -eq $row) { $verBad += "$($p.Name) 缺失" }
        elseif ((Prop $row 'version') -ne $p.Version) { $verBad += "$($p.Name) 版本 $(Prop $row 'version') ≠ $($p.Version)" }
    }
    Check '每个 npm 包的版本与脚本自己解析出来的一致' ($verBad.Count -eq 0) ($verBad -join '; ')
}

$pipRow = Find $g.globals 'tool' 'pip'
$pipAvailable = ($null -ne $pipV -and $pipV.Exit -eq 0)
if ($null -eq $pipRow) {
    if (-not $pipAvailable) {
        Skip 'pip 行' '本机没有可用的 pip（脚本自己的 pip 探测也失败）'
    } else {
        Check 'globals.toml 里有 pip 行' $false `
            "脚本自己探测到了 pip（python $expPython，$($expPipPackages.Count) 个包），产品却没有这一行"
    }
} else {
    Check 'globals.toml 里有 pip 行' ($null -ne $pipRow)
    Check "pip 的 tool_version = $expPython（脚本自己从 pip --version 里取）" `
        ((Prop $pipRow 'tool_version') -eq $expPython) "产品=$(Prop $pipRow 'tool_version')"
    if ($null -eq $expPipPrefix) {
        Skip 'pip 的 prefix' '脚本没能从 pip --version 的路径形状反推出 prefix（形状不认识）'
    } else {
        Check "pip 的 prefix = $expPipPrefix（脚本自己从路径形状反推）" `
            ((Prop $pipRow 'prefix') -eq $expPipPrefix) "产品=$(Prop $pipRow 'prefix')"
        Check 'pip 的 prefix_inside_version_dir 与 prefix 同生共死（有 prefix 就有这个键）' `
            ($null -ne (Prop $pipRow 'inside'))
        # 本机事实：`…\Programs\Python\Python312` 既不是 reparse point（我逐个祖先查过），
        # 也没有"版本段"（`Python312` 不是 `v24.19.0` 那种形状）→ 四支全假 → false。
        Check 'pip 的 prefix_inside_version_dir = false（Python312 不是版本目录形状）' `
            ((Prop $pipRow 'inside') -eq $false) "产品=$(Prop $pipRow 'inside')"
    }
    $prodPip = @(Prop $pipRow 'packages')
    $prodPipNames = @($prodPip | ForEach-Object { [string](Prop $_ 'name') })
    $expPipNames = @($expPipPackages | ForEach-Object { $_.Name })
    Check "pip 包集合与脚本自己跑 pip list 得到的一致（$($expPipNames.Count) 个）" `
        ((($prodPipNames | Sort-Object) -join ',') -eq (($expPipNames | Sort-Object) -join ',')) `
        "产品=$(($prodPipNames | Sort-Object) -join ',') 脚本=$(($expPipNames | Sort-Object) -join ',')"
}

Check '只对 npm / pip 出清单（决策 171：pnpm / yarn 不在本票的工具表里）' `
    (@(@($g.globals) | Where-Object { (Prop $_ 'tool') -notin @('npm', 'pip') }).Count -eq 0) `
    (@(@($g.globals) | ForEach-Object { [string](Prop $_ 'tool') }) -join ',')

# 票据点名要对照的取证数字（7 个包）。**只是报告**：真正的判据是与脚本自己枚举的一致。
$ticketExpect = @('@deepseek-ai/dsh', '@openai/codex', 'billion-context', 'corepack', 'npm', 'pnpm', 'tokentracker-cli')
$actualNames = @()
if ($null -ne $npmRow) { $actualNames = @(Prop $npmRow 'packages' | ForEach-Object { [string](Prop $_ 'name') }) }
# `Compare-Object` 的任一侧为 `$null` 会**绑定失败**（不是报一条 FAIL，是让脚本中止）——
# 两侧都先 `@()` 成数组。
$left = @($ticketExpect | Sort-Object)
$right = @($actualNames | Sort-Object)
$diff = @(Compare-Object $left $right)
Check 'npm 清单与票据的取证报告一致（7 个包）' ($diff.Count -eq 0) `
    "票据=$($left -join ',') 实际=$($right -join ',')"

# ── 4. 六个 section 的全量捕获 + restore 的不可还原说明 ─────────────────

Section '4. 全量捕获（六个 section）与 restore 的说明'

$allDir = Join-Path $root 'all'
$r3 = Invoke-Tuoen @('capture', '--out', $allDir, '--json')
Check 'capture（全量）退出 0' ($r3.Exit -eq 0) "exit=$($r3.Exit)"
$a = Get-Bundle $allDir
Write-Host ("  落盘：{0}" -f ($a.files -join ', '))
$expectedFiles = @('configs.toml', 'env.toml', 'globals.toml', 'path.toml', 'schema.toml', 'skipped.toml', 'tools.toml', 'wsl.toml')
$expectedJoined = (($expectedFiles | Sort-Object) -join ',')
Check '全量捕获写出七个文件（六个 section + skipped）' `
    ((($a.files | Sort-Object) -join ',') -eq $expectedJoined) ($a.files -join ',')
$expectedSections = 'configs,env,globals,path,tools,wsl'
# **字典序，不是写入顺序** —— 决策 165 的复核期修正：`schema.toml` 里 `sections` 一直（#12 起）
# 就是排序后的，不是 `Section::ALL` 的顺序。我的脚本第一版按"顺序即写入顺序"断言 → **我错了、
# 产品对了**（这一票里第四次）。已经上线的行为不为一句文档措辞让路。
Check 'schema.sections 是六个 section 的字典序（决策 165 的复核期修正）' `
    ((@($a.sections) -join ',') -eq $expectedSections) ($a.sections -join ',')

# 不回归：#16 已验过的四个 section 的数字必须还在（新 section 不许改动旧输出）。
$toolRows = @(Arr (Prop $a 'tools') 'rows')
Check 'tools 仍然有 27 行（#16 的真机口径：新终端 PATH）' ($toolRows.Count -eq 1 -and $toolRows[0] -eq 27) "rows=$($toolRows -join ',')"
$pathRows = @(Arr (Prop $a 'path') 'rows')
Check 'path 仍然有 49 行' ($pathRows.Count -eq 1 -and $pathRows[0] -eq 49) "rows=$($pathRows -join ',')"
$envRows = @(Arr (Prop $a 'env') 'rows')
Check 'env 仍然有 32 行' ($envRows.Count -eq 1 -and $envRows[0] -eq 32) "rows=$($envRows -join ',')"

# 决策 183：restore 对不认识的 section 要说话，而不是静默忽略。
$snapDir = Join-Path $root 'snapshot-for-restore'
$r4 = Invoke-Tuoen @('capture', '--out', $snapDir, '--json')
$r5 = Invoke-Tuoen @('restore', $snapDir, '--json')
Check 'restore 能读带 globals/configs 的快照（退出 0）' ($r5.Exit -eq 0) "exit=$($r5.Exit)"
$rp = Get-Payload $r5
$unrestorable = @(Arr $rp 'unrestorable')
if ($unrestorable.Count -eq 0) {
    $unrestorable = @(Arr (Prop $rp 'summary') 'unrestorable')
}
Check 'restore 的载荷里说明有不可还原的 section（globals + configs）' `
    ((($unrestorable | Sort-Object) -join ',') -eq 'configs,globals') "unrestorable=$(($unrestorable | Sort-Object) -join ',')"
$rsections = @(Arr $rp 'sections')
Check 'restore 的 sections 仍然只有四条' ($rsections.Count -eq 4) "count=$($rsections.Count)"

$r5h = Invoke-Tuoen @('restore', $snapDir)
Check 'restore 的人类输出里提到不可还原（中文只进人类输出）' `
    ($r5h.Stdout -match 'globals' -and $r5h.Stdout -match 'configs') ''

# ── 5. 幂等与逐字节 ─────────────────────────────────────────────────────

Section '5. 幂等：两次捕获除时间戳外逐字节相同'

function Get-WithoutTimestamp {
    param([string]$Path)
    # 文件可能**根本不存在**（产品没写出来）——`$ErrorActionPreference = 'Stop'` 下
    # 对不存在的路径 `Get-Content` 是终止错误，脚本会以一段红字中止而不是打出一条 FAIL。
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return '<absent>' }
    (Get-Content -LiteralPath $Path -Raw) -split "`n" |
        Where-Object { $_ -notmatch '^\s*captured_at\s*=' } |
        ForEach-Object { $_ }
}

$idem1 = Join-Path $root 'idem1'
$idem2 = Join-Path $root 'idem2'
$null = Invoke-Tuoen @('capture', '--out', $idem1, '--json')
$null = Invoke-Tuoen @('capture', '--out', $idem2, '--json')
foreach ($f in @('globals.toml', 'configs.toml', 'skipped.toml')) {
    $t1 = (Get-WithoutTimestamp (Join-Path $idem1 $f)) -join "`n"
    $t2 = (Get-WithoutTimestamp (Join-Path $idem2 $f)) -join "`n"
    Check "两次捕获的 $f 逐字节相同（除 captured_at）" ($t1 -eq $t2)
}
$cfg1 = Get-Bundle $idem1
$cfg2 = Get-Bundle $idem2
$h1 = @($cfg1.config | ForEach-Object { "$(Prop $_ 'path')=$(Prop $_ 'hash')" }) -join '|'
$h2 = @($cfg2.config | ForEach-Object { "$(Prop $_ 'path')=$(Prop $_ 'hash')" }) -join '|'
Check 'content_hash 幂等：同一文件两次捕获同哈希' ($h1 -eq $h2)

# ── 6. 安全红线：材料不泄漏 ─────────────────────────────────────────────

Section '6. 安全红线：材料不泄漏（本票的第二个红线）'

$allOutputs = @()
foreach ($d in @($cfgDir, $globDir, $allDir, $idem1, $idem2)) {
    $allOutputs += @(Get-ChildItem -LiteralPath $d -File -ErrorAction SilentlyContinue)
}
$leak = @()
foreach ($f in $allOutputs) {
    $text = Get-Content -LiteralPath $f.FullName -Raw
    if ($patPrefix -and $text.Contains($patPrefix)) { $leak += "$($f.Name): 前 8 字符" }
    if ($text -match 'glpat-[A-Za-z0-9_\-]{8,}') { $leak += "$($f.Name): glpat- 形状" }
    if ($patLength -gt 0 -and $text -match ("glpat-[A-Za-z0-9_\-]{" + ($patLength - 6) + ",}")) { $leak += "$($f.Name): 全长窗口" }
}
Check '真实 PAT 的片段不出现在任何产出文件里（含前 8 字符与全长窗口）' ($leak.Count -eq 0) ($leak -join '; ')
# **不许断言"产出文件里没有 `glpat-` 这个串"**：按决策 178，跳过清单的 `reason` 是给人看的中文，
# 它会**刻意**说出形状的名字（"值形似 GitLab 个人访问令牌（`glpat-` 前缀）"）—— 那是判据不是材料。
# 断言那个串的**正文**（≥2 个字符）才是对的：形状名后面跟的是空格或右括号。
# （`pathdiff-cli` 在自己的契约测试里先踩过这个坑：旧判据在产品做对的时候红。）
Check '产出文件里没有任何 glpat- 的正文（形状名后面只许跟标点/空格）' `
    (@($allOutputs | Where-Object { (Get-Content -LiteralPath $_.FullName -Raw) -match 'glpat-[A-Za-z0-9_\-]{2,}' }).Count -eq 0)
$repoHits = @(Select-String -Path (Join-Path $repo 'crates\*\src\*.rs'), (Join-Path $repo 'crates\*\src\*\*.rs'), (Join-Path $repo 'crates\*\tests\*.rs'), (Join-Path $repo 'fixtures\*\*\*') `
        -Pattern 'glpat-[A-Za-z0-9_\-]{10,}' -ErrorAction SilentlyContinue)
Check '仓库里没有 glpat- 字面量（GitHub push protection 防线）' ($repoHits.Count -eq 0) "$($repoHits.Count) 处"

# ── 7. 零副作用 ─────────────────────────────────────────────────────────

Section '7. 零副作用：这台机器一个字节都没变'

$afterPath = Get-PathSnapshot
Check 'HKCU\Environment\Path 的原文与类型逐字未变' `
    (($afterPath.UserRaw -eq $beforePath.UserRaw) -and ($afterPath.UserKind -eq $beforePath.UserKind)) `
    "sha $(Get-Sha16 $afterPath.UserRaw) vs $(Get-Sha16 $beforePath.UserRaw)"
Check 'HKLM\…\Path 的原文与类型逐字未变' `
    (($afterPath.MachRaw -eq $beforePath.MachRaw) -and ($afterPath.MachKind -eq $beforePath.MachKind))
$afterEnv = Get-EnvBlockSnapshot
Check '整个 HKCU\Environment 逐字未变（值名 + 类型 + 原文）' `
    ((($afterEnv -join '|') -eq ($beforeEnv -join '|'))) "值数 $($afterEnv.Count) vs $($beforeEnv.Count)"
$cfgChanged = @()
foreach ($cf in $realConfigs) {
    if ((Get-FileSha $cf.Path) -ne $beforeConfigHash[$cf.Path]) { $cfgChanged += $cf.Path }
}
Check '所有真实配置文件的哈希逐字未变' ($cfgChanged.Count -eq 0) ($cfgChanged -join '; ')
$afterTuoenLocal = Get-TreeListing "$env:LOCALAPPDATA\tuoen"
$afterTuoenRoaming = Get-TreeListing "$env:APPDATA\tuoen"
Check '%LOCALAPPDATA%\tuoen 目录树逐项相同' ((($afterTuoenLocal -join '|') -eq ($beforeTuoenLocal -join '|')))
Check '%APPDATA%\tuoen 目录树逐项相同' ((($afterTuoenRoaming -join '|') -eq ($beforeTuoenRoaming -join '|')))
$afterJb = Get-TreeListing $jbRoot 3
Check 'JetBrains 目录树逐项相同（我们只读它）' ((($afterJb -join '|') -eq ($beforeJb -join '|')))
Check '没有在用户目录里留下 globals.toml / configs.toml' `
    ((-not (Test-Path -LiteralPath "$home2\globals.toml")) -and (-not (Test-Path -LiteralPath "$home2\configs.toml")))
# `setx` 是禁用的（1024 截断 + 永久展开 `%VAR%`）。**两段判据，分开的理由**：
# ① 产品源码里不许出现带引号的 `setx` 字面量（老脚本就是这么扫的，零命中）；
# ② 脚本里 `setx` 只许出现在**守卫自己的文字**里 —— 因为"扫 `setx` 字面量"这件事本身必须
#    在脚本里写出这个字面量（#15/#16 的老脚本扫 `crates` 才没有自伤，我这一版加扫 `scripts`
#    于是自己撞上自己）。断言的是"没有一条**会执行** setx 的语句"。
# 正则写成 `[s]etx` 是**故意的**：这段文字里没有 `setx` 这个子串，所以它不会自己命中自己。
$setxProduct = @(Select-String -Path (Join-Path $repo 'crates\*\src\*.rs'), (Join-Path $repo 'crates\*\src\*\*.rs') `
        -Pattern '["'']setx' -ErrorAction SilentlyContinue)
Check '产品源码里没有带引号的 setx 字面量' ($setxProduct.Count -eq 0) "$($setxProduct.Count) 处"
$setxScript = @(Select-String -Path (Join-Path $repo 'scripts\*.ps1') -Pattern '[s]etx' -ErrorAction SilentlyContinue)
# 判据是"**会执行**的语句"：先去掉注释，再看 `setx` 后面是不是跟着一个参数的样子
# （`setx FOO bar` / `setx /M …` / `setx $env:FOO …`）。纯文字里提到这个命令不算 —— 守卫自己
# 必须能写出它禁止的东西，否则这段代码没法存在。`[s]etx` 这个写法保证它不命中自己。
$setxSuspect = @($setxScript | Where-Object {
        # 判据落在"**会执行**的语句"上：先把**字符串字面量**与注释挖掉，再看 `setx` 后面
        # 是否跟着参数的样子。纯文字里提到这个命令不算 —— 守卫自己必须能写出它禁止的东西。
        # 挖字符串这一步是补的：L2 的验收脚本里有一行节标题 `Section '8. 红线：setx / …'`，
        # 去掉注释后仍然匹配 `setx\s+/`，于是**别的脚本**的标题把这条守卫弄红了
        # （#17 那条"守卫会自己命中自己"的同一个坑，只是这次跨文件）。
        $bare = ($_.Line -replace "'[^']*'", ' ') -replace '"[^"]*"', ' '
        # setx.exe 与 setx $env:FOO … 也要算（前者带扩展名、后者第一个参数是变量）——[%$/A-Za-z]。
        (($bare -replace '#.*$', '') -match '[s]etx(\.exe)?\s+[%$/A-Za-z]')
    })
Check '脚本里没有一条会执行 setx 的语句（这个命令只许出现在守卫自己的文字里）' `
    ($setxSuspect.Count -eq 0) (($setxSuspect | ForEach-Object { "$(Split-Path -Leaf $_.Path):$($_.LineNumber)" }) -join ', ')

# ── 8. 参数面与错误路径 ─────────────────────────────────────────────────

Section '8. 参数面与错误路径'

$bad = Invoke-Tuoen @('capture', '--only', 'nonsense', '--out', (Join-Path $root 'bad'))
Check '--only nonsense → 退出 2（拼错的 section 必须当场被拒）' ($bad.Exit -eq 2) "exit=$($bad.Exit)"
$help = Invoke-Tuoen @('capture', '--help')
Check '--help 列出 globals 与 configs' (($help.Stdout -match 'globals') -and ($help.Stdout -match 'configs'))
$two = Invoke-Tuoen @('capture', '--only', 'globals', '--only', 'configs', '--out', (Join-Path $root 'two'), '--json')
Check '--only globals --only configs 可以同时给' ($two.Exit -eq 0) "exit=$($two.Exit)"
$twoBundle = Get-Bundle (Join-Path $root 'two')
Check '两个 section 同时捕获时只写这两个 + skipped' `
    ((($twoBundle.files | Sort-Object) -join ',') -eq 'configs.toml,globals.toml,schema.toml,skipped.toml') ($twoBundle.files -join ',')

# 决策 174 复核期补充（`--no-version` 的不对称）：pip 的 prefix 来自 `pip --version` 那条路径的
# 形状，所以那个开关一开，**pip 行连 prefix 一起没有**；npm 的 prefix 来自另一条命令，照旧。
# 期望值不是我推的：产品侧有用例 `no_version_skips_the_probes_and_keeps_the_enumeration` 钉着。
$nvDir = Join-Path $root 'globals-no-version'
$nv = Invoke-Tuoen @('capture', '--only', 'globals', '--no-version', '--out', $nvDir, '--json')
Check '--no-version 退出 0' ($nv.Exit -eq 0) "exit=$($nv.Exit)"
$nvG = Get-Bundle $nvDir
$nvNpm = Find $nvG.globals 'tool' 'npm'
$nvPip = Find $nvG.globals 'tool' 'pip'
if ($null -eq $nvNpm) {
    Check '--no-version 下仍有 npm 行' $false 'globals.toml 里没有 npm 行'
} else {
    $nvNpmKeys = @(Prop $nvNpm 'keys')
    Check '--no-version 下 npm 的 tool_version 键仍在、值是 unknown（决策 172：键没有"省略"这个选项）' `
        (($nvNpmKeys -contains 'tool_version') -and ((Prop $nvNpm 'tool_version') -eq 'unknown')) `
        "键=$($nvNpmKeys -contains 'tool_version') 值=$(Prop $nvNpm 'tool_version')"
    Check '--no-version 下 npm 的 prefix 照旧（它不来自版本探测）' `
        ((Prop $nvNpm 'prefix') -eq $expPrefix) "产品=$(Prop $nvNpm 'prefix')"
    Check '--no-version 下 npm 的包清单照旧（包枚举不是版本探测）' `
        ((@(Prop $nvNpm 'packages')).Count -eq @($expNpmPackages).Count) `
        "产品=$(@(Prop $nvNpm 'packages').Count) 脚本=$(@($expNpmPackages).Count)"
}
if ($null -eq $nvPip) {
    Skip '--no-version 下的 pip 行' '本机这一次没有 pip 行'
} else {
    # **键在不在只看 `keys`**（TOML 原文的键名）。第一版我拿 `PropNames` 判，而它是 Python 侧
    # 规范化后那个 dict 的成员 —— 每个字段都在，于是这条断言**永远红**（产品其实是对的：
    # 落盘的 globals.toml 里 pip 那一段确实没有 prefix / prefix_inside_version_dir）。
    $nvPipKeys = @(Prop $nvPip 'keys')
    Check '--no-version 下 pip 的 tool_version 键仍在、值是 unknown' `
        (($nvPipKeys -contains 'tool_version') -and ((Prop $nvPip 'tool_version') -eq 'unknown')) `
        "键=$($nvPipKeys -contains 'tool_version') 值=$(Prop $nvPip 'tool_version')"
    Check '--no-version 下 pip 的 prefix 整键消失（它来自 pip --version）' `
        (-not ($nvPipKeys -contains 'prefix')) "键=$($nvPipKeys -join ',')"
    Check '--no-version 下 pip 的 prefix_inside_version_dir 一起消失（同生共死）' `
        (-not ($nvPipKeys -contains 'prefix_inside_version_dir')) "键=$($nvPipKeys -join ',')"
    Check '--no-version 下 pip 的包清单照旧（pip list 不是版本探测）' `
        ((@(Prop $nvPip 'packages')).Count -eq @($expPipPackages).Count) `
        "产品=$(@(Prop $nvPip 'packages').Count) 脚本=$(@($expPipPackages).Count)"
}

# ── 9. 收尾 ─────────────────────────────────────────────────────────────

Section '9. 收尾'

if ($SelfTest) {
    Check '自检：这一条必须失败（-SelfTest 用）' $false '故意失败'
}

$verdict = if ($script:Failed -eq 0) { 'PASS' } else { 'FAIL' }
Write-Host ''
Write-Host "SUMMARY checks_passed=$($script:Passed) checks_failed=$($script:Failed) checks_skipped=$($script:SkipCount) verdict=$verdict" -ForegroundColor $(if ($verdict -eq 'PASS') { 'Green' } else { 'Red' })
if ($script:Failed -gt 0) {
    Write-Host '失败项：' -ForegroundColor Red
    foreach ($f in $script:Failures) { Write-Host "  - $f" -ForegroundColor Red }
}
Write-Host "工件目录：$root"

exit $(if ($script:Failed -gt 0) { 1 } else { 0 })
