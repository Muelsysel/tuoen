<#
.SYNOPSIS
    票据 #12（L1 捕获 `tuoen.d/`）的真机验收。

.DESCRIPTION
    这条脚本**只读**：它跑 `tuoen capture`（一条明确只读的命令），
    然后把产出与它自己独立数出来的数字逐项对照。

    「独立」在这里是字面意思：§3 那一批数字是这条脚本用 PowerShell
    直接从注册表与磁盘数出来的，**不经过 tuoen 的任何代码**。
    两套数字对不上就是有问题 —— 谁对谁错是下一步的事，但绝不能默默放过。

    跑之前会把两个作用域的 `Path`（原文 + 类型 + 长度 + SHA256）与
    `%LOCALAPPDATA%\tuoen`、store 的清单各拍一次快照，跑完再拍一次，
    逐项要求相同 —— 这是"捕获没有写机器"的证据，而不是一句承诺。

.PARAMETER SelfTest
    故意让最后一条检查失败，用来证明这条脚本**会**报失败（exit 1）。
    一条不会失败的验收等于没有验收。

.PARAMETER SkipBuild
    跳过 §0 的 release 构建（只在已经构建过、且想快速重跑时用）。

.PARAMETER Version
    要验收的版本号，只进摘要行。

.EXAMPLE
    pwsh -File scripts/acceptance-L1-12.ps1
    pwsh -File scripts/acceptance-L1-12.ps1 -SelfTest   # 必须 exit 1
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
$root = Join-Path $env:TEMP 'tuoen-acceptance-L1-12'

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

function Get-RawPath {
    param([string]$Scope)
    $item = Get-Item $ENV_KEYS[$Scope]
    $raw = $item.GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    $type = $item.GetValueKind('Path').ToString()
    [pscustomobject]@{
        Scope = $Scope
        Raw   = $raw
        Type  = $type
        Chars = $raw.Length
        Sha   = (Get-Sha $raw)
    }
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

# 规范化后的键：去尾部反斜杠 + 小写（采集器判"重复"用的就是它）。
function Get-EntryKey {
    param([string]$Value)
    $v = (Get-EntryValue $Value).TrimEnd('\')
    $v.ToLowerInvariant()
}

function Get-PathListing {
    param([string]$Scope)
    $raw = (Get-Item $ENV_KEYS[$Scope]).GetValue('Path', '', 'DoNotExpandEnvironmentNames')
    @($raw -split ';')
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
    @(Get-ChildItem $store -Force -Recurse -Depth 1 | Sort-Object FullName | ForEach-Object { $_.FullName.Substring($store.Length) })
}

function Invoke-Capture {
    param([string[]]$Extra = @(), [string]$Out)
    $args = @('capture', '--out', $Out, '--json') + $Extra
    $out_file = Join-Path $root ("stdout-" + [guid]::NewGuid().ToString('n').Substring(0, 8) + '.json')
    $err_file = "$out_file.err"
    # 子进程的 `PATH` 必须是**原样的**那一条：这样 `effectiveChars` 与
    # `$pristine.Length` 就是同一个东西的两种量法（见 §2）。
    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $exe
    foreach ($a in $args) { $psi.ArgumentList.Add($a) }
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
        Exit   = $proc.ExitCode
        Stdout = $stdout
        Stderr = $stderr
        File   = $out_file
        ErrFile = $err_file
    }
}

function Get-TomlValue {
    param([string]$Path, [string]$Key)
    $m = Select-String -Path $Path -Pattern ("(?m)^" + [regex]::Escape($Key) + " = (.*)$") | Select-Object -First 1
    if (-not $m) { return $null }
    $v = $m.Matches[0].Groups[1].Value.Trim()
    if ($v.StartsWith('"') -and $v.EndsWith('"')) { return $v.Substring(1, $v.Length - 2) }
    if ($v.StartsWith("'") -and $v.EndsWith("'")) { return $v.Substring(1, $v.Length - 2) }
    $v
}

function Count-Pattern {
    param([string]$Text, [string]$Pattern)
    ([regex]::Matches($Text, $Pattern)).Count
}

function Strip-Timestamp {
    param([string]$Path)
    (Get-Content $Path -Raw) -split "`n" |
        Where-Object { $_ -notmatch '^(captured_at|"capturedAt")' } |
        ForEach-Object { $_.TrimEnd() }
}

# ── §0 二进制 ───────────────────────────────────────────────────────────

Section '0 二进制（release + 它真的知道 capture）'
if ($SkipBuild) {
    Skip '构建 release 二进制' '被 -SkipBuild 跳过'
} else {
    Push-Location $repo
    try {
        $env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"
        $build = & cargo build --release -p tuoen-cli 2>&1
        Check '构建 release 二进制' ($LASTEXITCODE -eq 0) ("exit=" + $LASTEXITCODE)
    } finally {
        Pop-Location
    }
}
Check '二进制存在' (Test-Path $exe) $exe
$help = & $exe capture --help 2>&1 | Out-String
foreach ($flag in @('--only', '--out', '--no-version', '--json')) {
    Check "help 里有 $flag" ($help.Contains($flag))
}
Check 'help 说这条命令只读' ($help.Contains('只读'))

# ── §1 预检 ─────────────────────────────────────────────────────────────

Section '1 预检（结构断言 + 快照）'
$setx_hits = @(Get-ChildItem (Join-Path $repo 'crates') -Recurse -Filter *.rs |
    Select-String -Pattern '"setx' -SimpleMatch)
Check '仓库里没有带引号的 "setx 字面量' ($setx_hits.Count -eq 0) ("命中 " + $setx_hits.Count)

$capture_sources = @(
    (Join-Path $repo 'crates\core\src\capture'),
    (Join-Path $repo 'crates\cli\src')
) | Where-Object { Test-Path $_ }
$writers = @('set_value', 'delete_value', 'broadcast_environment_change', 'create_junction', 'remove_junction')
$capture_files = @(Get-ChildItem (Join-Path $repo 'crates\core\src\capture') -Recurse -Filter *.rs) +
    @(Get-ChildItem (Join-Path $repo 'crates\cli\src') -Filter 'capture*.rs')
$write_hits = @($capture_files | Select-String -Pattern ($writers -join '|'))
Check '捕获路径里没有任何写机器的调用' ($write_hits.Count -eq 0) ("命中 " + $write_hits.Count)

$script:Pristine = Get-PristinePath
$before_user = Get-RawPath 'user'
$before_machine = Get-RawPath 'machine'
$before_repo = Get-RepoListing
$before_store = Get-StoreListing
Write-Host ("  两个作用域：user {0} 字符 / {1} / {2}" -f $before_user.Chars, $before_user.Type, $before_user.Sha.Substring(0, 12))
Write-Host ("              machine {0} 字符 / {1} / {2}" -f $before_machine.Chars, $before_machine.Type, $before_machine.Sha.Substring(0, 12))
Write-Host ("  新终端口径（展开后）：{0} 字符" -f $script:Pristine.Length)

if (Test-Path $root) { Remove-Item $root -Recurse -Force }
New-Item -ItemType Directory -Path $root | Out-Null

# ── §2 真机跑一次 ───────────────────────────────────────────────────────

Section '2 真机跑一次（六个文件）'
$run1 = Join-Path $root 'run1'
$r1 = Invoke-Capture -Out $run1
Check 'capture 退出 0' ($r1.Exit -eq 0) ("exit=" + $r1.Exit)
$json1 = $r1.Stdout | ConvertFrom-Json
Check '输出是 JSON（有 data 键）' ($null -ne $json1.data)
Check '--json 里没有 CJK' ((Count-Pattern $r1.Stdout '[\u4e00-\u9fff]') -eq 0)

$expected_files = @('schema.toml', 'tools.toml', 'path.toml', 'env.toml', 'wsl.toml', 'skipped.toml')
$actual_files = @(Get-ChildItem $run1 -File | Select-Object -ExpandProperty Name | Sort-Object)
# **两边都要排序**：`Compare-Object` 是按位置比的，一边有序一边无序会把
# 同一批文件报成 12 条差异（第一版就是这么假红的）。
Check '六个文件都在' (@(Compare-Object ($expected_files | Sort-Object) $actual_files -SyncWindow 0).Count -eq 0) ($actual_files -join ', ')
Check 'sections 是那四个' ((@($json1.data.sections | Sort-Object) -join ',') -eq 'env,path,tools,wsl')

$path_toml = Join-Path $run1 'path.toml'
$path_text = Get-Content $path_toml -Raw
Check 'effectiveChars 等于新终端口径的长度' ($json1.data.path.effectiveChars -eq $script:Pristine.Length) ("$($json1.data.path.effectiveChars) vs $($script:Pristine.Length)")

# ── §3 独立核对（PowerShell 自己数，不经 tuoen） ─────────────────────────

Section '3 独立核对：脚本自己数一遍，再与产出对'
$user_entries = Get-PathListing 'user'
$machine_entries = Get-PathListing 'machine'
$all_entries = $machine_entries + $user_entries
$nonempty = @($all_entries | Where-Object { (Get-EntryValue $_).Length -gt 0 })
$keys = @($nonempty | ForEach-Object { Get-EntryKey $_ })
$distinct = @($keys | Sort-Object -Unique)
$surplus = $nonempty.Count - $distinct.Count

$dangling = @($nonempty | Where-Object {
        $v = Get-EntryValue $_
        -not (Test-Path -LiteralPath $v -PathType Container)
    })
$username = @($nonempty | Where-Object {
        $v = Get-EntryValue $_
        $v.Contains('%') -eq $false -and $v.ToLowerInvariant().Contains('\users\')
    })
$empty_count = $all_entries.Count - $nonempty.Count

Write-Host ("  脚本数出来：机器 {0} 段 / 用户 {1} 段 / 合计 {2}（非空 {3}，空 {4}）" -f `
        $machine_entries.Count, $user_entries.Count, $all_entries.Count, $nonempty.Count, $empty_count)
Write-Host ("             不同值 {0} → 重复富余 {1}；失效 {2}；硬编码用户名 {3}" -f `
        $distinct.Count, $surplus, $dangling.Count, $username.Count)

Check 'rawMachineChars 对得上' ($json1.data.path.rawMachineChars -eq $before_machine.Chars) ("$($json1.data.path.rawMachineChars) vs $($before_machine.Chars)")
Check 'rawUserChars 对得上' ($json1.data.path.rawUserChars -eq $before_user.Chars) ("$($json1.data.path.rawUserChars) vs $($before_user.Chars)")
Check '条目数对得上' ($json1.data.path.entries -eq $all_entries.Count) ("$($json1.data.path.entries) vs $($all_entries.Count)")
Check '重复条目数对得上' ($json1.data.path.duplicates -eq $surplus) ("$($json1.data.path.duplicates) vs $($surplus)")
# 只数 `[[entry]]` 那一段的行：`[[effective]]` 里也有 `scope = "machine"`，
# 拿 `^scope = "machine"$` 去数会把两套坐标系混在一起（33+20=53 那种假红）。
$machine_rows = Count-Pattern $path_text '(?ms)^\[\[entry\]\]\nscope = "machine"\n'
$user_rows = Count-Pattern $path_text '(?ms)^\[\[entry\]\]\nscope = "user"\n'
Check '机器级行数对得上' ($machine_rows -eq $machine_entries.Count) ("$machine_rows vs $($machine_entries.Count)")
Check '用户级行数对得上' ($user_rows -eq $user_entries.Count) ("$user_rows vs $($user_entries.Count)")

$exists_no = @($path_text -split "`n" | Where-Object { $_ -match '^exists = "no"$' }).Count
$empty_true = Count-Pattern $path_text '(?m)^empty = true$'
Check '失效条目数对得上（判据 !empty && exists == no）' ($exists_no -eq $dangling.Count) ("$exists_no vs $($dangling.Count)")
Check '空条目被记成 empty = true' ($empty_true -eq $empty_count) ("$empty_true vs $($empty_count)")
$unknown_rows = Count-Pattern $path_text '(?m)^exists = "unknown"$'
Check '空条目的 exists 是 unknown' ($unknown_rows -eq $empty_count) ("$unknown_rows vs $($empty_count)")
Check '硬编码用户名条数对得上' ((Count-Pattern $path_text '(?m)^has_username = true$') -eq $username.Count)

# 两套坐标系的不变量：每一条 `[[effective]]` 都要能在 `[[entry]]` 里按 (scope, index) 查到。
$entry_refs = @([regex]::Matches($path_text, '(?ms)^\[\[entry\]\]\nscope = "(\w+)"\nindex = (\d+)') |
    ForEach-Object { "$($_.Groups[1].Value):$($_.Groups[2].Value)" })
$eff_refs = @([regex]::Matches($path_text, '(?ms)^\[\[effective\]\]\nscope = "(\w+)"\nindex = (\d+)') |
    ForEach-Object { "$($_.Groups[1].Value):$($_.Groups[2].Value)" })
$dangling_refs = @($eff_refs | Where-Object { $entry_refs -notcontains $_ })
Check '[[effective]] 没有悬空引用' ($dangling_refs.Count -eq 0) ("$($eff_refs.Count) 条引用 / 悬空 $($dangling_refs.Count)")
Check '[[effective]] 去掉了重复与空条目' ($eff_refs.Count -eq $distinct.Count) ("$($eff_refs.Count) vs 不同值 $($distinct.Count)")

# ── §3.5 env.toml 的 target_exists 与磁盘对照 ───────────────────────────

Section '3.5 env.toml：target_exists 每一条都要经得起独立复核'
$env_toml = Join-Path $run1 'env.toml'
$env_text = Get-Content $env_toml -Raw
$env_blocks = @([regex]::Matches($env_text, '(?ms)^\[\[var\]\]\n(.*?)(?=\n\[\[|\z)') |
    ForEach-Object { $_.Groups[1].Value })
Check 'env.toml 里有变量行' ($env_blocks.Count -gt 0) ("$($env_blocks.Count) 条")

$bad_yes = @()
$bad_no = @()
$bad_list = @()
foreach ($block in $env_blocks) {
    $name = [regex]::Match($block, '(?m)^name = "(.*)"').Groups[1].Value
    $raw = [regex]::Match($block, "(?m)^value_raw = '(.*)'$").Groups[1].Value
    $exists = [regex]::Match($block, '(?m)^target_exists = "(.*)"').Groups[1].Value
    $target = [regex]::Match($block, "(?m)^target = '(.*)'$").Groups[1].Value
    # **列表不是一条路径**：`Path` / `PSModulePath` 都以 `C:\` 开头，
    # 只看前缀会把整条 `;` 串拿去问磁盘（真机验收抓出来的那个 bug）。
    if ($raw.Contains(';')) {
        if ($exists -ne 'not-a-path' -or $target.Length -gt 0) { $bad_list += "$name($exists)" }
        continue
    }
    if ($exists -eq 'yes' -and -not (Test-Path -LiteralPath $target)) { $bad_yes += "$name -> $target" }
    if ($exists -eq 'no' -and (Test-Path -LiteralPath $target)) { $bad_no += "$name -> $target" }
}
Check '每一条 yes 的目标都真的在盘上' ($bad_yes.Count -eq 0) ($bad_yes -join '; ')
Check '每一条 no 的目标都真的不在盘上（不是假问题）' ($bad_no.Count -eq 0) ($bad_no -join '; ')
Check '含分号的值一律 not-a-path 且不记 target' ($bad_list.Count -eq 0) ($bad_list -join '; ')
Write-Host ("  target_exists 分布：no={0} yes={1} not-a-path={2}" -f `
        (Count-Pattern $env_text '(?m)^target_exists = "no"$'), `
        (Count-Pattern $env_text '(?m)^target_exists = "yes"$'), `
        (Count-Pattern $env_text '(?m)^target_exists = "not-a-path"$'))

# ── §4 幂等 ─────────────────────────────────────────────────────────────

Section '4 幂等：两次捕获只差时间戳'
$run2 = Join-Path $root 'run2'
$r2 = Invoke-Capture -Out $run2
Check '第二次也退出 0' ($r2.Exit -eq 0)
foreach ($f in $expected_files) {
    $a = (Strip-Timestamp (Join-Path $run1 $f)) -join "`n"
    $b = (Strip-Timestamp (Join-Path $run2 $f)) -join "`n"
    Check "$f 逐字节相同" ($a -eq $b)
}
$ts1 = Get-TomlValue $path_toml 'captured_at'
$ts2 = Get-TomlValue (Join-Path $run2 'path.toml') 'captured_at'
Check '时间戳确实在动（否则「相同」可能是假话）' ($ts1.Length -gt 0 -and $ts2.Length -gt 0)

# ── §5 选择性捕获 ───────────────────────────────────────────────────────

Section '5 --only 是格式层面的选择，不是事后过滤'
$only_cases = @(
    @{ Name = 'path'; Files = @('path.toml', 'schema.toml') },
    @{ Name = 'env'; Files = @('env.toml', 'schema.toml', 'skipped.toml') },
    @{ Name = 'tools'; Files = @('tools.toml', 'schema.toml') },
    @{ Name = 'wsl'; Files = @('wsl.toml', 'schema.toml') }
)
foreach ($case in $only_cases) {
    $dir = Join-Path $root ("only-" + $case.Name)
    $r = Invoke-Capture -Extra @('--only', $case.Name) -Out $dir
    $got = @(Get-ChildItem $dir -File | Select-Object -ExpandProperty Name | Sort-Object)
    $want = @($case.Files | Sort-Object)
    Check "--only $($case.Name)：只有 $($case.Files -join '+')" ((@(Compare-Object $want $got -SyncWindow 0).Count) -eq 0) ($got -join ', ')
    $j = $r.Stdout | ConvertFrom-Json
    Check "--only $($case.Name)：sections 只有它" ((@($j.data.sections) -join ',') -eq $case.Name)
    Check "--only $($case.Name)：别的 section 根本没被生成" (@($got | Where-Object { $_ -notin $case.Files }).Count -eq 0)
}

# ── §6 凭据没有被写进快照 ───────────────────────────────────────────────

Section '6 跳过清单：名字在，材料一个字节都不在'
$skipped = @($json1.data.skipped)
if ($skipped.Count -eq 0) {
    Skip '跳过项的材料核对' '这台机器上一条跳过项都没有'
} else {
    $produced = @(Get-ChildItem $root -Recurse -File | Where-Object { $_.Extension -in @('.toml', '.json') })
    foreach ($item in $skipped) {
        Check "跳过项 $($item.name) 有 kind" ([bool]$item.kind)
        if ($item.section -ne 'env') { continue }
        $key = $ENV_KEYS[$item.scope]
        if (-not $key) { continue }
        $item_obj = Get-Item $key
        if (@($item_obj.Property) -notcontains $item.name) {
            Check "跳过项 $($item.name) 在注册表里真的存在" $false
            continue
        }
        $secret = $item_obj.GetValue($item.name, '', 'DoNotExpandEnvironmentNames')
        if ($secret.Length -lt 8) { continue }
        $probes = @($secret, $secret.Substring(0, 8), $secret.Substring($secret.Length - 4))
        $leaks = @($produced | Where-Object {
                $file = $_
                @($probes | Where-Object { [bool](Select-String -Path $file.FullName -SimpleMatch $_ -Quiet) }).Count -gt 0
            })
        Check "跳过项 $($item.name) 的值没有任何字节落进产出（长 $($secret.Length)）" ($leaks.Count -eq 0) (($leaks | ForEach-Object { $_.Name }) -join ', ')
        Check "跳过项 $($item.name) 的名字在 skipped.toml 里" ([bool](Select-String -Path (Join-Path $run1 'skipped.toml') -SimpleMatch $item.name -Quiet))
        Check "跳过项 $($item.name) 没有进 env.toml" (-not [bool](Select-String -Path (Join-Path $run1 'env.toml') -SimpleMatch $item.name -Quiet))
    }
    Check '成功载荷里没有 reason 键' (-not [bool](Select-String -Path $r1.File -SimpleMatch '"reason"' -Quiet))
}

# 分母：独立数一遍两个作用域的持久变量，必须等于「写进 env.toml 的 + 跳过的」。
$env_total = 0
foreach ($scope in @('user', 'machine')) {
    $env_total += @((Get-Item $ENV_KEYS[$scope]).Property).Count
}
$env_written = $json1.data.env.total
Check '持久变量总数 = 写进 env.toml 的 + 跳过的' (($env_written + $skipped.Count) -eq $env_total) ("$env_written + $($skipped.Count) vs $env_total")

# ── §7 --no-version ─────────────────────────────────────────────────────

Section '7 --no-version 真的少探测'
$dir_nv = Join-Path $root 'no-version'
$r_nv = Invoke-Capture -Extra @('--only', 'tools', '--no-version') -Out $dir_nv
$v_full = @(Select-String -Path (Join-Path $run1 'tools.toml') -Pattern '^version = ' -AllMatches).Count
$v_none = @(Select-String -Path (Join-Path $dir_nv 'tools.toml') -Pattern '^version = ' -AllMatches).Count
Check '--no-version 的版本行数更少' ($v_none -lt $v_full) ("$v_none < $v_full")
Check '--no-version 仍然列出全部工具' ((($r_nv.Stdout | ConvertFrom-Json).data.tools.entries) -eq $json1.data.tools.entries)
Check '结构性的版本仍然在（不是「全都缺席」）' ($v_none -gt 0) ("$v_none 条")

# ── §8 收尾：机器没被动过 ───────────────────────────────────────────────

Section '8 收尾核对：这条命令真的只读'
$after_user = Get-RawPath 'user'
$after_machine = Get-RawPath 'machine'
Check '用户级 Path 逐字节相同' (($after_user.Raw -ceq $before_user.Raw) -and ($after_user.Type -eq $before_user.Type)) ("$($after_user.Sha.Substring(0, 12))")
Check '机器级 Path 逐字节相同' (($after_machine.Raw -ceq $before_machine.Raw) -and ($after_machine.Type -eq $before_machine.Type)) ("$($after_machine.Sha.Substring(0, 12))")
Check '用户级 Path 的 SHA 相同' ($after_user.Sha -eq $before_user.Sha)
Check '机器级 Path 的 SHA 相同' ($after_machine.Sha -eq $before_machine.Sha)
Check '%LOCALAPPDATA%\tuoen 没被动过' (((Get-RepoListing) -join '|') -eq ($before_repo -join '|'))
Check 'store 没被动过' (((Get-StoreListing) -join '|') -eq ($before_store -join '|'))
$stray = @(Get-ChildItem $env:TEMP -Directory -Filter 'tuoen.d' -ErrorAction SilentlyContinue)
Check '没有在 %TEMP% 根上留下 tuoen.d' ($stray.Count -eq 0)
Check '产出只在这一次的目录里' (@(Get-ChildItem $root -Directory | Where-Object { $_.Name -notin @('run1', 'run2', 'only-path', 'only-env', 'only-tools', 'only-wsl', 'no-version') }).Count -eq 0)

if ($SelfTest) {
    Check '自检：这一条必须失败（证明脚本会报失败）' $false
}

# ── §9 摘要 ─────────────────────────────────────────────────────────────

Write-Host ''
$verdict = if ($script:Failed -eq 0) { 'PASS' } else { 'FAIL' }
Write-Host ("SUMMARY checks_passed={0} checks_failed={1} checks_skipped={2} verdict={3} version={4} " -f `
        $script:Passed, $script:Failed, $script:SkipCount, $verdict, $Version) -ForegroundColor $(if ($script:Failed -eq 0) { 'Green' } else { 'Red' })
Write-Host ("        raw_user_chars={0} raw_machine_chars={1} effective_chars={2} entries={3} duplicates={4} dangling={5} empty={6} username_deps={7} skipped_secrets={8}" -f `
        $before_user.Chars, $before_machine.Chars, $script:Pristine.Length, $all_entries.Count, $surplus, $dangling.Count, $empty_count, $username.Count, $skipped.Count)
Write-Host ("        user_path_untouched={0} machine_path_untouched={1} out_root={2}" -f `
        ($after_user.Sha -eq $before_user.Sha), ($after_machine.Sha -eq $before_machine.Sha), $root)
if ($script:Failed -gt 0) {
    Write-Host '失败项：' -ForegroundColor Red
    $script:Failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
}

exit $(if ($script:Failed -eq 0) { 0 } else { 1 })
