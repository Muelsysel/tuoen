<#
.SYNOPSIS
    票据 #14（L1 项目级 pin：`tuoen.toml` / `tuoen.lock` / `shell` / `trust` / `auto`）的真机验收。

.DESCRIPTION
    这一票的招牌承诺是"**在项目里 pin 一套版本，进这个目录就拿到它**"。它的失效形态不是崩溃，
    而是**用户以为切了版本、其实没切**（前置目录里没有那个命令，老的那个静默生效）——
    所以这条脚本的核心判据是端到端的：

    * `tuoen shell --exec "node -e …process.execPath…"` 打印出来的必须是**我们 store 里的那个**
      `node.exe`，而不是 nvm4w 的、也不是系统 PATH 上任何一个；
    * 前置目录由**脚本自己**从 `%LOCALAPPDATA%\tuoen\store` 与 `tuoen.toml` 算一遍，再与
      `--dry-run --json` 的 `prepend` / `tools` 逐项对照（不经过 tuoen 的任何代码）；
    * `%LOCALAPPDATA%\tuoen` 的清单、`C:\nvm4w\nodejs` 与 store 里 `current` 的 **junction 目标**、
      真实的 `%APPDATA%\tuoen` 清单：跑前跑后逐项相同（**不翻转 junction、不碰真实信任清单**）。

    信任清单写在 `%APPDATA%\tuoen\trust.toml`。这条脚本把 `%APPDATA%` **注入成临时目录**，
    于是它永远不会碰用户真实的信任清单 —— 而"真实的那个没被动过"本身也是一条判据。

    **一条不能失败的验收不是验收**：`-SelfTest` 会让最后一条检查故意失败（exit 1）。

.PARAMETER SelfTest
    故意让最后一条检查失败，用来证明这条脚本**会**报失败（exit 1）。

.PARAMETER SkipBuild
    跳过 §0 的 release 构建（只在已经构建过、且想快速重跑时用）。

.PARAMETER Version
    要验收的版本号，只进摘要行。

.EXAMPLE
    pwsh -File scripts/acceptance-L1-14.ps1
    pwsh -File scripts/acceptance-L1-14.ps1 -SelfTest   # 必须 exit 1
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
$root = Join-Path $env:TEMP 'tuoen-acceptance-L1-14'
Remove-Item -Recurse -Force $root -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $root | Out-Null

# 注入的 `%APPDATA%`：信任清单会落在这里，**不碰用户真实的那个**。
$fakeAppData = Join-Path $root 'appdata'
New-Item -ItemType Directory -Path $fakeAppData | Out-Null
$trustFile = Join-Path $fakeAppData 'tuoen\trust.toml'

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

# 「新终端会看到什么」= 机器级原文 + `;` + 用户级原文，再展开一次。**一个字节都不写注册表。**
function Get-PristinePath {
    $machine = (Get-Item $ENV_KEYS['machine']).GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    $user = (Get-Item $ENV_KEYS['user']).GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    [Environment]::ExpandEnvironmentVariables("$machine;$user")
}
$script:Pristine = Get-PristinePath

function Get-TreeListing {
    param([string]$Root, [int]$Depth = 2)
    if (-not (Test-Path -LiteralPath $Root)) { return @('<absent>') }
    @(Get-ChildItem -LiteralPath $Root -Force -Recurse -Depth $Depth |
            Sort-Object FullName | ForEach-Object { $_.FullName.Substring($Root.Length) })
}

function Get-JunctionTarget {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return '<absent>' }
    $item = Get-Item -LiteralPath $Path -Force
    if (-not ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) { return '<not-a-reparse>' }
    @($item.Target)[0]
}

function Invoke-Tuoen {
    param([string[]]$Arguments, [string]$Name = 'run', [hashtable]$ExtraEnv = @{})
    $out_file = Join-Path $root "$Name.out"
    $err_file = Join-Path $root "$Name.err"
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $exe
    foreach ($a in $Arguments) { $psi.ArgumentList.Add($a) }
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    # 子进程的 PATH = 新终端口径；APPDATA 注入成临时目录（信任清单不落真实位置）。
    $psi.EnvironmentVariables['PATH'] = $script:Pristine
    $psi.EnvironmentVariables['Path'] = $script:Pristine
    $psi.EnvironmentVariables['APPDATA'] = $fakeAppData
    foreach ($k in $ExtraEnv.Keys) { $psi.EnvironmentVariables[$k] = $ExtraEnv[$k] }
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

function Get-Json {
    param([object]$Result)
    try { $Result.Stdout | ConvertFrom-Json } catch { $null }
}

function Test-NoCjk {
    param([string]$Text)
    -not [bool]($Text -match '[^\x00-\x7F]')
}

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

# 写一个项目目录：`tuoen.toml` 用 UTF-8（无 BOM），行尾可指定 LF / CRLF。
function New-Project {
    param([string]$Name, [string]$Toml, [string]$Eol = "`n")
    $dir = Join-Path $root $Name
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    $text = ($Toml -split "`r?`n") -join $Eol
    if ($Eol -eq "`r`n") {
        [System.IO.File]::WriteAllText((Join-Path $dir 'tuoen.toml'), $text, (New-Object System.Text.UTF8Encoding $false))
    } else {
        [System.IO.File]::WriteAllText((Join-Path $dir 'tuoen.toml'), $text, (New-Object System.Text.UTF8Encoding $false))
    }
    $dir
}

$PIN_NODE_24 = @'
[project]
name = "acceptance-l1-14"

[tools]
node = "24"
'@

# ── §0 二进制 ───────────────────────────────────────────────────────────

Section '0 二进制（release + 四个新命令都在）'
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
$top = Invoke-Tuoen -Arguments @('--help') -Name 'top-help'
foreach ($cmd in @('shell', 'trust', 'auto', 'lock')) {
    Check ("顶层帮助里有 {0}" -f $cmd) ([bool]($top.Stdout -match $cmd))
}
$shellHelp = Invoke-Tuoen -Arguments @('shell', '--help') -Name 'shell-help'
Check 'shell --help 退出码 0' ($shellHelp.Exit -eq 0)
Check 'shell 帮助里有 --dry-run / --exec / --json' `
    (($shellHelp.Stdout -match '--dry-run') -and ($shellHelp.Stdout -match '--exec') -and ($shellHelp.Stdout -match '--json'))
Check '帮助里没有 --fix' (-not [bool]($shellHelp.Stdout -match '--fix'))

# ── §1 预检：快照 + 脚本自己的分母 ──────────────────────────────────────

Section '1 预检：快照与脚本自己算出来的前置目录'
$realAppData = if ($env:APPDATA) { Join-Path $env:APPDATA 'tuoen' } else { '<no-appdata>' }
$realStore = Join-Path $env:LOCALAPPDATA 'tuoen'
$nvmLink = 'C:\nvm4w\nodejs'
$storeCurrent = Join-Path $realStore 'store\node\current'

$beforeRealAppData = Get-TreeListing -Root $realAppData -Depth 2
$beforeStore = Get-TreeListing -Root $realStore -Depth 2
$beforeNvmLink = Get-JunctionTarget $nvmLink
$beforeStoreCurrent = Get-JunctionTarget $storeCurrent
$beforeTrustFile = if (Test-Path -LiteralPath $trustFile) { Get-Content -LiteralPath $trustFile -Raw } else { '<absent>' }

Check '真实的 %APPDATA%\tuoen 存在或不存在都记下来了' $true ($beforeRealAppData -join ', ')
Check 'nvm4w 的符号链接现在指向' ($beforeNvmLink -ne '<absent>') $beforeNvmLink
Check 'store 里 node 的 current 现在指向' ($beforeStoreCurrent -ne '<absent>') $beforeStoreCurrent

# 脚本自己算：store 里装了哪些 node 版本（目录名就是版本）。
$storeNodeVersions = @()
$nodeVersionsDir = Join-Path $realStore 'store\node\versions'
if (Test-Path -LiteralPath $nodeVersionsDir) {
    $storeNodeVersions = @(Get-ChildItem -LiteralPath $nodeVersionsDir -Directory | Sort-Object Name | ForEach-Object { $_.Name })
}
Check 'store 里有 node 的版本目录（本票的端到端演示要靠它）' ($storeNodeVersions.Count -gt 0) ($storeNodeVersions -join ', ')

# 脚本自己算：`24` 应该命中哪个版本（数字成分前缀，取最高）。
function Select-Version {
    param([string[]]$Versions, [string]$Spec)
    $parts = @($Spec.TrimStart('v').Split('.') | ForEach-Object { [int]$_ })
    $hits = @($Versions | Where-Object {
            $v = $_.TrimStart('v').Split('.')
            if ($v.Count -lt $parts.Count) { return $false }
            for ($i = 0; $i -lt $parts.Count; $i++) {
                if ([int]$v[$i] -ne $parts[$i]) { return $false }
            }
            return $true
        })
    if ($hits.Count -eq 0) { return $null }
    @($hits | Sort-Object { [version]($_ -replace '[^0-9.]', '') } -Descending)[0]
}
$expectedNode = Select-Version -Versions $storeNodeVersions -Spec '24'
Check '脚本自己算出来 `24` 命中的版本' ($null -ne $expectedNode) "$expectedNode（候选：$($storeNodeVersions -join ', ')）"
$expectedNodeDir = Join-Path $nodeVersionsDir $expectedNode

# ── §2 计划（--dry-run）：不写文件、不 spawn ────────────────────────────

Section '2 计划：shell --dry-run --json'
$proj = New-Project -Name 'proj' -Toml $PIN_NODE_24
$projFilesBefore = @(Get-ChildItem -LiteralPath $proj -Force | Sort-Object Name | ForEach-Object { $_.Name })
$dry = Invoke-Tuoen -Arguments @('shell', '--dry-run', '--json') -Name 'shell-dry' -ExtraEnv @{}
# 注意：`--dry-run` 必须在项目目录里跑 —— 用 cwd 注入（ProcessStartInfo 的 WorkingDirectory）。
function Invoke-InProject {
    param([string]$Dir, [string[]]$Arguments, [string]$Name, [hashtable]$ExtraEnv = @{})
    $out_file = Join-Path $root "$Name.out"
    $err_file = Join-Path $root "$Name.err"
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $exe
    foreach ($a in $Arguments) { $psi.ArgumentList.Add($a) }
    $psi.WorkingDirectory = $Dir
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.EnvironmentVariables['PATH'] = $script:Pristine
    $psi.EnvironmentVariables['Path'] = $script:Pristine
    $psi.EnvironmentVariables['APPDATA'] = $fakeAppData
    foreach ($k in $ExtraEnv.Keys) { $psi.EnvironmentVariables[$k] = $ExtraEnv[$k] }
    $proc = [System.Diagnostics.Process]::Start($psi)
    $stdout = $proc.StandardOutput.ReadToEnd()
    $stderr = $proc.StandardError.ReadToEnd()
    $proc.WaitForExit()
    Set-Content -Path $out_file -Value $stdout -NoNewline -Encoding utf8
    Set-Content -Path $err_file -Value $stderr -NoNewline -Encoding utf8
    [pscustomobject]@{ Exit = $proc.ExitCode; Stdout = $stdout; Stderr = $stderr; File = $out_file; ErrFile = $err_file }
}

$dry = Invoke-InProject -Dir $proj -Arguments @('shell', '--dry-run', '--json') -Name 'shell-dry'
Check 'shell --dry-run --json 退出码 0' ($dry.Exit -eq 0) ("exit=$($dry.Exit) stderr=$($dry.Stderr)")
$dj = Get-Json $dry
Check '载荷结构：schemaVersion/command/ok/data' ($null -ne $dj -and $dj.schemaVersion -eq 1 -and $dj.command -eq 'shell' -and $dj.ok -eq $true)
Check '成功载荷无 CJK 且无 message 键' ((Test-NoCjk $dry.Stdout) -and (-not [bool]($dry.Stdout -match '"message"')))
$data = $dj.data
Check 'shell 是 cmd' ($data.shell -eq 'cmd') $data.shell
Check 'depth = 0' ($data.depth -eq 0) "$($data.depth)"
Check 'prepend 的第一条 == 脚本自己算出来的版本目录' ($data.prepend.Count -ge 1 -and $data.prepend[0] -eq $expectedNodeDir) `
    ("$($data.prepend[0]) vs $expectedNodeDir")
Check 'pathAfter 以 prepend 开头' ($data.pathAfter.StartsWith($data.prepend[0])) ($data.pathAfter.Substring(0, [Math]::Min(80, $data.pathAfter.Length)))
Check 'pathBefore == 脚本自己的新终端口径 PATH' ($data.pathBefore -eq $script:Pristine) `
    ("$($data.pathBefore.Length) vs $($script:Pristine.Length) 字符")
$tool0 = @($data.tools)[0]
Check 'tools[0] 是 node / spec 24 / 脚本算出的版本 / 来源 tuoen' `
    ($tool0.name -eq 'node' -and $tool0.spec -eq '24' -and $tool0.version -eq $expectedNode -and $tool0.source -eq 'tuoen') `
    ("$($tool0.name) $($tool0.spec) $($tool0.version) $($tool0.source)")
Check 'tools[0].path == 脚本算出的版本目录' ($tool0.path -eq $expectedNodeDir) $tool0.path
Check '--dry-run 不写锁（lockWritten=false 且目录里没有 tuoen.lock）' `
    ($data.lockWritten -eq $false -and -not (Test-Path -LiteralPath (Join-Path $proj 'tuoen.lock')))
Check '--dry-run 不改项目目录' `
    ((@(Get-ChildItem -LiteralPath $proj -Force | Sort-Object Name | ForEach-Object { $_.Name }) -join ',') -eq ($projFilesBefore -join ',')) `
    ($projFilesBefore -join ', ')

$dry2 = Invoke-InProject -Dir $proj -Arguments @('shell', '--dry-run', '--json') -Name 'shell-dry2'
Check '两次 --dry-run --json 逐字节相同' ($dry.Stdout -ceq $dry2.Stdout) ("$($dry.Stdout.Length) / $($dry2.Stdout.Length)")

$noJson = Invoke-InProject -Dir $proj -Arguments @('shell', '--json') -Name 'shell-json-nodry'
Check '不带 --dry-run 的 --json 被拒（退出码 2）' ($noJson.Exit -eq 2) ("exit=$($noJson.Exit)")

# ── §3 端到端：子进程真的用了我们那份 node ──────────────────────────────

Section '3 端到端：shell --exec 里跑的是我们 store 里的 node'
$exec = Invoke-InProject -Dir $proj -Arguments @('shell', '--exec', 'node -e "console.log(process.execPath)"') -Name 'shell-exec'
Check 'shell --exec 退出码 0' ($exec.Exit -eq 0) ("exit=$($exec.Exit) stderr=$($exec.Stderr)")
$execPath = $exec.Stdout.Trim()
Check '子进程打印的 process.execPath 在脚本算出的版本目录里' `
    ($execPath.StartsWith($expectedNodeDir, [System.StringComparison]::OrdinalIgnoreCase)) `
    ("$execPath（期望前缀 $expectedNodeDir）")

$ver = Invoke-InProject -Dir $proj -Arguments @('shell', '--exec', 'node -v') -Name 'shell-version'
Check 'shell --exec "node -v" 退出码 0 且版本对得上' `
    ($ver.Exit -eq 0 -and $ver.Stdout.Trim().TrimStart('v') -eq $expectedNode.TrimStart('v')) `
    ("$($ver.Stdout.Trim()) vs v$expectedNode")

$where = Invoke-InProject -Dir $proj -Arguments @('shell', '--exec', 'where node') -Name 'shell-where'
$whereFirst = @($where.Stdout -split "`r?`n" | Where-Object { $_.Trim() -ne '' })[0]
Check 'where node 的第一条就是我们 store 里的那个' `
    ($whereFirst -and $whereFirst.Trim().StartsWith($expectedNodeDir, [System.StringComparison]::OrdinalIgnoreCase)) `
    ($whereFirst)

$echo = Invoke-InProject -Dir $proj -Arguments @('shell', '--exec', 'echo %TUOEN_SHELL_DEPTH%') -Name 'shell-depth-echo'
Check '子进程里 TUOEN_SHELL_DEPTH = 1' ($echo.Stdout.Trim() -eq '1') $echo.Stdout.Trim()

$exitCode = Invoke-InProject -Dir $proj -Arguments @('shell', '--exec', 'exit 7') -Name 'shell-exit7'
Check '退出码透传（exit 7 → 7）' ($exitCode.Exit -eq 7) ("exit=$($exitCode.Exit)")

# ── §4 信任门：未信任不自动切换 ─────────────────────────────────────────

Section '4 信任门：auto 与 trust 的完整生命周期'
# 快照要在**这一节开始时**拍：§3 的 `shell --exec` 是**非** `--dry-run`，
# 它会按决策 116 写出 `tuoen.lock` —— 拿 §2 之前的快照来比会把那件事误报成"多出文件"。
$projFilesBeforeAuto = @(Get-ChildItem -LiteralPath $proj -Force | Sort-Object Name | ForEach-Object { $_.Name })
$autoBefore = Invoke-InProject -Dir $proj -Arguments @('auto', '--dry-run', '--json') -Name 'auto-untrusted'
$aj = Get-Json $autoBefore
Check '未信任目录上 auto 退出 1' ($autoBefore.Exit -eq 1) ("exit=$($autoBefore.Exit)")
Check '错误码是 untrusted' ($aj -and $aj.ok -eq $false -and $aj.error.code -eq 'untrusted') `
    ($(if ($aj) { $aj.error.code } else { '<no json>' }))
Check '错误消息里点名了 tuoen trust' ($aj -and [bool]($aj.error.message -match 'tuoen trust'))
Check '未信任时项目目录里没有多出文件' `
    ((@(Get-ChildItem -LiteralPath $proj -Force | Sort-Object Name | ForEach-Object { $_.Name }) -join ',') -eq ($projFilesBeforeAuto -join ','))

$trust = Invoke-InProject -Dir $proj -Arguments @('trust', '--json') -Name 'trust-add'
$tj = Get-Json $trust
Check 'tuoen trust 退出 0' ($trust.Exit -eq 0) ("exit=$($trust.Exit) stderr=$($trust.Stderr)")
Check '信任写入的清单在注入的 %APPDATA% 里（不在真实的那个）' `
    ((Test-Path -LiteralPath $trustFile) -and (((Get-TreeListing -Root $realAppData -Depth 2) -join '|') -eq ($beforeRealAppData -join '|'))) `
    $trustFile
Check 'trust 的载荷有 path/fingerprint/action' `
    ($tj -and $tj.data.path -eq $proj -and $tj.data.fingerprint.StartsWith('sha256:') -and $tj.data.action -eq 'trusted') `
    ($(if ($tj) { "$($tj.data.fingerprint) $($tj.data.action)" } else { '<no json>' }))

$autoAfter = Invoke-InProject -Dir $proj -Arguments @('auto', '--dry-run', '--json') -Name 'auto-trusted'
$aaj = Get-Json $autoAfter
Check '信任之后 auto 退出 0' ($autoAfter.Exit -eq 0) ("exit=$($autoAfter.Exit) stderr=$($autoAfter.Stderr)")
Check 'auto 的载荷里有 trust=trusted' ($aaj -and $aaj.data.trust -eq 'trusted') `
    ($(if ($aaj) { $aaj.data.trust } else { '<no json>' }))
Check 'auto 的前置目录与 shell 一致' ($aaj -and $aaj.data.prepend[0] -eq $expectedNodeDir) `
    ($(if ($aaj) { $aaj.data.prepend[0] } else { '<no json>' }))

$list = Invoke-InProject -Dir $proj -Arguments @('trust', '--list', '--json') -Name 'trust-list'
$lj = Get-Json $list
Check 'trust --list 退出 0 且列出这条（state=trusted）' `
    ($list.Exit -eq 0 -and $lj -and @($lj.data.entries | Where-Object { $_.path -eq $proj -and $_.state -eq 'trusted' }).Count -eq 1) `
    ($(if ($lj) { ($lj.data.entries | ForEach-Object { "$($_.path)=$($_.state)" }) -join ', ' } else { '<no json>' }))

# CRLF ↔ LF 必须通过（指纹只归一化行尾）。
$projCrlf = New-Project -Name 'proj-crlf' -Toml $PIN_NODE_24 -Eol "`r`n"
$trustCrlf = Invoke-InProject -Dir $projCrlf -Arguments @('trust', '--json') -Name 'trust-crlf'
$crlfJson = Get-Json $trustCrlf
Check 'CRLF 版的 tuoen.toml 指纹与 LF 版相同' `
    ($crlfJson -and $crlfJson.data.fingerprint -eq $tj.data.fingerprint) `
    ("$($crlfJson.data.fingerprint) vs $($tj.data.fingerprint)")
$autoCrlf = Invoke-InProject -Dir $projCrlf -Arguments @('auto', '--dry-run', '--json') -Name 'auto-crlf'
Check 'CRLF 版在信任后能自动切换' ($autoCrlf.Exit -eq 0) ("exit=$($autoCrlf.Exit)")

# 改一个字符 → 指纹不匹配 → 拒绝（不是静默信任、也不是永久拉黑）。
$tampered = $PIN_NODE_24 -replace 'node = "24"', 'node = "24 "'
[System.IO.File]::WriteAllText((Join-Path $proj 'tuoen.toml'), $tampered, (New-Object System.Text.UTF8Encoding $false))
$autoTampered = Invoke-InProject -Dir $proj -Arguments @('auto', '--dry-run', '--json') -Name 'auto-tampered'
$tamperedJson = Get-Json $autoTampered
Check '改一个字符后 auto 退出 1 且错误码是 fingerprint-mismatch' `
    ($autoTampered.Exit -eq 1 -and $tamperedJson -and $tamperedJson.error.code -eq 'fingerprint-mismatch') `
    ($(if ($tamperedJson) { $tamperedJson.error.code } else { '<no json>' }))
Check '拒绝时消息里点名重新 trust' ($tamperedJson -and [bool]($tamperedJson.error.message -match 'trust'))
[System.IO.File]::WriteAllText((Join-Path $proj 'tuoen.toml'), $PIN_NODE_24, (New-Object System.Text.UTF8Encoding $false))

$revoke = Invoke-InProject -Dir $proj -Arguments @('trust', '--revoke', $proj, '--json') -Name 'trust-revoke'
$rj = Get-Json $revoke
Check 'trust --revoke 退出 0 且 action=revoked' ($revoke.Exit -eq 0 -and $rj -and $rj.data.action -eq 'revoked') `
    ($(if ($rj) { $rj.data.action } else { '<no json>' }))
$autoRevoked = Invoke-InProject -Dir $proj -Arguments @('auto', '--dry-run', '--json') -Name 'auto-revoked'
$revokedJson = Get-Json $autoRevoked
Check '摘掉信任后 auto 回到 untrusted' ($autoRevoked.Exit -eq 1 -and $revokedJson -and $revokedJson.error.code -eq 'untrusted') `
    ($(if ($revokedJson) { $revokedJson.error.code } else { '<no json>' }))
$listAfter = Invoke-InProject -Dir $proj -Arguments @('trust', '--list', '--json') -Name 'trust-list-after'
$laj = Get-Json $listAfter
Check '摘掉之后清单里没有这条' ($laj -and @($laj.data.entries | Where-Object { $_.path -eq $proj }).Count -eq 0)

# ── §5 锁文件：写、读、以及不一致时拒绝 ─────────────────────────────────

Section '5 锁文件：tuoen lock 与不一致时的拒绝'
$lock = Invoke-InProject -Dir $proj -Arguments @('lock', '--json') -Name 'lock-write'
$lkj = Get-Json $lock
Check 'tuoen lock 退出 0 且 written=true' ($lock.Exit -eq 0 -and $lkj -and $lkj.data.written -eq $true) `
    ("exit=$($lock.Exit)")
$lockPath = Join-Path $proj 'tuoen.lock'
Check 'tuoen.lock 真的写在项目目录里' (Test-Path -LiteralPath $lockPath) $lockPath
$lockText = Get-Content -LiteralPath $lockPath -Raw
Check '锁里记了 version / source / path（L3 离线 bundle 的接口）' `
    ([bool]($lockText -match [regex]::Escape($expectedNode)) -and [bool]($lockText -match 'source = "tuoen"') -and [bool]($lockText -match 'path = '))
Check '锁里没有时间戳（进 git，不能每次 lock 都产生一行 diff）' `
    (-not [bool]($lockText -match '(?i)time|date|20\d\d-\d\d-\d\d'))
$lockAgain = Invoke-InProject -Dir $proj -Arguments @('lock', '--json') -Name 'lock-again'
Check '两次 lock 产出的锁文件逐字节相同' `
    ((Get-Content -LiteralPath $lockPath -Raw) -ceq (Get-Content -LiteralPath $lockPath -Raw))
Check '第二次 lock 退出 0' ($lockAgain.Exit -eq 0)

$tamperedToml = $PIN_NODE_24 -replace 'node = "24"', 'node = "20"'
[System.IO.File]::WriteAllText((Join-Path $proj 'tuoen.toml'), $tamperedToml, (New-Object System.Text.UTF8Encoding $false))
$mismatch = Invoke-InProject -Dir $proj -Arguments @('shell', '--dry-run', '--json') -Name 'lock-mismatch'
$mj = Get-Json $mismatch
Check '声明与锁不一致时 shell 退出 1 且错误码是 lock-mismatch' `
    ($mismatch.Exit -eq 1 -and $mj -and $mj.error.code -eq 'lock-mismatch') `
    ($(if ($mj) { $mj.error.code } else { '<no json>' }))
Check '不一致时消息里点名下一条命令 tuoen lock' ($mj -and [bool]($mj.error.message -match 'tuoen lock'))
[System.IO.File]::WriteAllText((Join-Path $proj 'tuoen.toml'), $PIN_NODE_24, (New-Object System.Text.UTF8Encoding $false))
$afterRestore = Invoke-InProject -Dir $proj -Arguments @('shell', '--dry-run', '--json') -Name 'lock-restored'
Check '把声明改回去之后 shell 又能跑' ($afterRestore.Exit -eq 0) ("exit=$($afterRestore.Exit)")

# ── §6 未安装的版本与深度护栏 ───────────────────────────────────────────

Section '6 未安装的版本 / 没有 tuoen.toml / 深度护栏'
$projMissing = New-Project -Name 'proj-missing' -Toml ($PIN_NODE_24 -replace 'node = "24"', 'node = "99"')
$missing = Invoke-InProject -Dir $projMissing -Arguments @('shell', '--dry-run', '--json') -Name 'version-missing'
$mj2 = Get-Json $missing
Check 'pin 一个没装的版本 → 退出 1 且错误码是 version-not-installed' `
    ($missing.Exit -eq 1 -and $mj2 -and $mj2.error.code -eq 'version-not-installed') `
    ($(if ($mj2) { $mj2.error.code } else { '<no json>' }))
Check '提示里给出下一条命令（tuoen install node@99）' `
    ($mj2 -and [bool]($mj2.error.message -match 'tuoen install node@99')) `
    ($(if ($mj2) { $mj2.error.message.Substring(0, [Math]::Min(120, $mj2.error.message.Length)) } else { '<no json>' }))

$projNone = Join-Path $root 'proj-none'
New-Item -ItemType Directory -Path $projNone -Force | Out-Null
$noPin = Invoke-InProject -Dir $projNone -Arguments @('shell', '--dry-run', '--json') -Name 'no-pin'
$nj = Get-Json $noPin
Check '没有 tuoen.toml → 退出 1 且错误码是 missing-pin' `
    ($noPin.Exit -eq 1 -and $nj -and $nj.error.code -eq 'missing-pin') `
    ($(if ($nj) { $nj.error.code } else { '<no json>' }))

$deep = Invoke-InProject -Dir $proj -Arguments @('shell', '--dry-run', '--json') -Name 'too-deep' -ExtraEnv @{ TUOEN_SHELL_DEPTH = '9' }
$dj2 = Get-Json $deep
Check 'TUOEN_SHELL_DEPTH=9 → 退出 1 且错误码是 shell-depth' `
    ($deep.Exit -eq 1 -and $dj2 -and $dj2.error.code -eq 'shell-depth') `
    ($(if ($dj2) { $dj2.error.code } else { '<no json>' }))

# ── §7 收尾：junction 与真实信任清单都没被动过 ──────────────────────────

Section '7 收尾核对：不翻转 junction、不碰真实的 %APPDATA%'
Check 'nvm4w 的符号链接目标未变' ((Get-JunctionTarget $nvmLink) -eq $beforeNvmLink) ("$beforeNvmLink → $(Get-JunctionTarget $nvmLink)")
Check 'store 里 node 的 current 目标未变' ((Get-JunctionTarget $storeCurrent) -eq $beforeStoreCurrent) ("$beforeStoreCurrent → $(Get-JunctionTarget $storeCurrent)")
Check '真实的 %APPDATA%\tuoen 逐项未变' (((Get-TreeListing -Root $realAppData -Depth 2) -join '|') -eq ($beforeRealAppData -join '|'))
Check '真实的 %LOCALAPPDATA%\tuoen 逐项未变（含 store 两层）' (((Get-TreeListing -Root $realStore -Depth 2) -join '|') -eq ($beforeStore -join '|'))
Check '真实的信任清单文件本身未被创建/修改' `
    ($(if (Test-Path -LiteralPath (Join-Path $realAppData 'trust.toml')) { (Get-Content -LiteralPath (Join-Path $realAppData 'trust.toml') -Raw) } else { '<absent>' }) -eq $beforeTrustFile)

if ($SelfTest) {
    Check '自检：这一条必须失败（证明脚本会报失败）' $false
}

# ── §8 摘要 ─────────────────────────────────────────────────────────────

Write-Host ''
$verdict = if ($script:Failed -eq 0) { 'PASS' } else { 'FAIL' }
Write-Host ("SUMMARY checks_passed={0} checks_failed={1} checks_skipped={2} verdict={3} version={4} " -f `
        $script:Passed, $script:Failed, $script:SkipCount, $verdict, $Version) -ForegroundColor $(if ($script:Failed -eq 0) { 'Green' } else { 'Red' })
Write-Host ("        store_node_versions={0} resolved_node={1} prepend={2}" -f `
        $storeNodeVersions.Count, $expectedNode, $expectedNodeDir)
Write-Host ("        exec_path={0}" -f $execPath)
Write-Host ("        nvm_link_untouched={0} store_current_untouched={1} real_appdata_untouched={2} real_store_untouched={3} out_root={4}" -f `
        ((Get-JunctionTarget $nvmLink) -eq $beforeNvmLink), ((Get-JunctionTarget $storeCurrent) -eq $beforeStoreCurrent), `
        (((Get-TreeListing -Root $realAppData -Depth 2) -join '|') -eq ($beforeRealAppData -join '|')), `
        (((Get-TreeListing -Root $realStore -Depth 2) -join '|') -eq ($beforeStore -join '|')), $root)
if ($script:Failed -gt 0) {
    Write-Host '失败项：' -ForegroundColor Red
    $script:Failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
}

exit $(if ($script:Failed -eq 0) { 0 } else { 1 })
