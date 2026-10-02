# 在"一个全新终端会拿到的环境"里跑一条命令。
#
# 为什么需要这个：`PATH` 的变更只对**之后新开的**进程生效 —— Windows 的环境块是在
# `CreateProcess` 时从父进程复制的，注册表只在 shell 建立自己的环境时被读一次。
# 所以"在当前这个 PowerShell 里再跑一次 `where node`"**证明不了任何事**：
# 它拿到的还是它启动时那份旧环境。
#
# 本脚本从**注册表**重新读一遍两个作用域的 `Path`（不展开），拼成
# `机器级 + ";" + 用户级`，再把子进程的 `PATH` 换成这一条。这正是登录时
# explorer.exe 做的事。
#
# 它**只影响这个子进程**，一个字节都不写注册表。
#
# 不还原的地方（诚实说明）：
#   · MSIX 包激活时由启动器注入的条目（本机 PowerShell 会注入它自己的别名目录，
#     而且**排在机器级之前**）—— 本脚本不注入，所以顺序上会与真实的 PowerShell 终端不同。
#   · 祖先进程自己改过的环境（比如从 cargo 里开出来的终端会多几条）。
# 这两条都只会**增加**条目，不会改变"机器级在用户级之前"这个顺序。
#
# `-RemoveEntry` / `-AppendEntry` 是唯一两个能让结果偏离"真实登录环境"的开关，
# 用它们的时候必须在结论里说清楚模拟了什么。它们**只改这个子进程的环境块**，
# 注册表一个字节都不写。

[CmdletBinding()]
param(
    # 要在这个环境里跑的命令行，交给 cmd.exe /c。
    [Parameter(Mandatory, Position = 0)]
    [string]$Command,

    # 模拟"用户自己把这条目录从 PATH 里去掉之后会怎样"。
    # 匹配规则：去掉首尾空白与结尾反斜杠之后**大小写不敏感**地相等。
    # 这是唯一一条能让本脚本偏离"真实登录环境"的开关，用它的时候必须说明。
    [string[]]$RemoveEntry = @(),

    # 模拟"这条目录已经在 `PATH` 上"（比如真的跑过一次 `tuoen path add`）。
    # **追加在末尾** —— 与 `tuoen path add` 的行为一致（它只追加、不 prepend，决策 74）。
    # 这里不做去重：真实那份 `PATH` 本来就允许同一个目录出现多次。
    [string[]]$AppendEntry = @(),

    # 把最终生效的 PATH 逐条打出来（调试用）。
    [switch]$ListPath
)

$ErrorActionPreference = 'Stop'

$machineKey = 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
$userKey = 'HKCU:\Environment'

# **不展开**读：展开是不可逆的，而这里要原样看到用户写了什么。
$machineRaw = (Get-Item $machineKey).GetValue('Path', '', 'DoNotExpandEnvironmentNames')
$userRaw = (Get-Item $userKey).GetValue('Path', '', 'DoNotExpandEnvironmentNames')

if ($null -eq $machineRaw) { $machineRaw = '' }
if ($null -eq $userRaw) { $userRaw = '' }

function Normalize-Entry([string]$value) {
    $t = $value.Trim()
    if ($t.Length -gt 3) { $t = $t.TrimEnd('\') }
    return $t.ToLowerInvariant()
}

$fresh = ($machineRaw, $userRaw | Where-Object { $_ -ne '' }) -join ';'
# 登录时 `REG_EXPAND_SZ` 的值会被展开一次；`REG_SZ` 的值不受影响。
# 本机的两个作用域都是 `REG_SZ` 且不含 `%`，展开是恒等变换，但这一步不能省。
$fresh = [Environment]::ExpandEnvironmentVariables($fresh)

$entries = $fresh -split ';'
$removed = @()
if ($RemoveEntry.Count -gt 0) {
    $wanted = $RemoveEntry | ForEach-Object { Normalize-Entry $_ }
    $kept = foreach ($entry in $entries) {
        if ($entry -eq '') { $entry; continue }
        if ($wanted -contains (Normalize-Entry $entry)) { $removed += $entry; continue }
        $entry
    }
    $entries = @($kept)
    $fresh = ($entries -join ';')
}

if ($removed.Count -eq 0 -and $RemoveEntry.Count -gt 0) {
    Write-Output "REMOVED=0"
    Write-Output "警告：-RemoveEntry 一条都没匹配上 —— 这个模拟没有改变任何东西。"
    exit 3
}

if ($AppendEntry.Count -gt 0) {
    foreach ($entry in $AppendEntry) {
        if ($entries.Count -gt 0 -and $entries[-1] -eq '') { $entries[-1] = $entry }
        else { $entries += $entry }
    }
    $fresh = ($entries -join ';')
    Write-Output ("APPENDED={0}  {1}" -f $AppendEntry.Count, ($AppendEntry -join ' | '))
}

Write-Output ("PATH_CHARS={0}" -f $fresh.Length)
Write-Output ("PATH_ENTRIES={0}" -f (@($entries | Where-Object { $_ -ne '' }).Count))
if ($removed.Count -gt 0) {
    Write-Output ("REMOVED={0}  {1}" -f $removed.Count, ($removed -join ' | '))
}
if ($ListPath) {
    $i = 0
    foreach ($entry in $entries) {
        Write-Output ("  [{0,2}] {1}" -f $i, $entry)
        $i++
    }
}

$psi = [System.Diagnostics.ProcessStartInfo]::new()
$psi.FileName = $env:ComSpec
$psi.Arguments = '/c ' + $Command
$psi.UseShellExecute = $false
$psi.RedirectStandardOutput = $true
$psi.RedirectStandardError = $true
# 继承当前进程的环境，然后把 PATH 换成"新终端那一份"。
$psi.EnvironmentVariables['PATH'] = $fresh

$proc = [System.Diagnostics.Process]::Start($psi)
$stdout = $proc.StandardOutput.ReadToEnd()
$stderr = $proc.StandardError.ReadToEnd()
$proc.WaitForExit()

Write-Output ("EXIT={0}" -f $proc.ExitCode)
Write-Output '--- stdout ---'
Write-Output $stdout.TrimEnd()
if ($stderr.Trim().Length -gt 0) {
    Write-Output '--- stderr ---'
    Write-Output $stderr.TrimEnd()
}
exit $proc.ExitCode
