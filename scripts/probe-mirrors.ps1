# 探测内置镜像源在这台机器/这个网络上的真实可用性。
#
# 为什么要有这个脚本：镜像列表如果只是"抄来的 URL 清单"，它会在真实网络上
# 静默失效（超时、403、404），而用户看到的只是"下载失败"。
# 探测结果进 docs/acceptance/，作为"排序不是拍脑袋定的"的证据。
#
# 只发 range GET（`-r 0-0`，取 1 字节），所以不会真的下载几百 MB 的包；
# 但它走的是真实的 GET 语义，能区分"HEAD 被拒"与"真的取不到"。
#
# 用法：
#   pwsh -File scripts\probe-mirrors.ps1
#   pwsh -File scripts\probe-mirrors.ps1 -Json   # 给 CI 或文档用

[CmdletBinding()]
param(
    [switch]$Json,
    [int]$TimeoutSeconds = 15
)

$ErrorActionPreference = 'Stop'

$curl = Join-Path $env:SystemRoot 'System32\curl.exe'
if (-not (Test-Path -LiteralPath $curl)) {
    throw "系统 curl.exe 不存在：$curl"
}

# 每个工具一组候选源。`Official` 与 `Cn` 分开标，因为选源策略要用到这个区分。
$sources = @(
    # ── Node.js ──────────────────────────────────────────────────────────────
    @{ Tool = 'node'; Id = 'cn-npmmirror'; Region = 'cn'; Url = 'https://cdn.npmmirror.com/binaries/node/v24.19.0/node-v24.19.0-win-x64.zip' }
    @{ Tool = 'node'; Id = 'cn-tencent';   Region = 'cn'; Url = 'https://mirrors.cloud.tencent.com/nodejs-release/v24.19.0/node-v24.19.0-win-x64.zip' }
    @{ Tool = 'node'; Id = 'official';     Region = 'global'; Url = 'https://nodejs.org/dist/v24.19.0/node-v24.19.0-win-x64.zip' }
    @{ Tool = 'node'; Id = 'cn-tuna';      Region = 'cn'; Url = 'https://mirrors.tuna.tsinghua.edu.cn/nodejs-release/v24.19.0/node-v24.19.0-win-x64.zip' }
    @{ Tool = 'node'; Id = 'cn-ustc';      Region = 'cn'; Url = 'https://mirrors.ustc.edu.cn/nodejs-release/v24.19.0/node-v24.19.0-win-x64.zip' }
    # ── Temurin JDK（Adoptium 的目录结构在各镜像上并不统一）──────────────────
    @{ Tool = 'temurin'; Id = 'official-api'; Region = 'global'; Url = 'https://api.adoptium.net/v3/binary/latest/21/ga/windows/x64/jdk/hotspot/normal/eclipse' }
    @{ Tool = 'temurin'; Id = 'cn-tuna';      Region = 'cn'; Url = 'https://mirrors.tuna.tsinghua.edu.cn/Adoptium/21/jdk/x64/windows/OpenJDK21U-jdk_x64_windows_hotspot_21.0.8_9.zip' }
    @{ Tool = 'temurin'; Id = 'cn-bfsu';      Region = 'cn'; Url = 'https://mirrors.bfsu.edu.cn/Adoptium/21/jdk/x64/windows/OpenJDK21U-jdk_x64_windows_hotspot_21.0.8_9.zip' }
    @{ Tool = 'temurin'; Id = 'cn-aliyun';    Region = 'cn'; Url = 'https://mirrors.aliyun.com/adoptium/21/jdk/x64/windows/OpenJDK21U-jdk_x64_windows_hotspot_21.0.8_9.zip' }
)

$results = foreach ($source in $sources) {
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    # `-r 0-0` 取 1 字节；`--max-time` 兜住挂住的镜像。
    $raw = & $curl -sS -L --max-time $TimeoutSeconds -r 0-0 -o NUL `
        -w '%{http_code} %{size_download} %{time_total}' $source.Url 2>&1
    $watch.Stop()

    $line = ($raw | Select-Object -Last 1)
    $parts = "$line" -split '\s+'
    $code = if ($parts.Count -ge 1 -and $parts[0] -match '^\d+$') { [int]$parts[0] } else { 0 }

    [pscustomobject]@{
        Tool      = $source.Tool
        Id        = $source.Id
        Region    = $source.Region
        HttpCode  = $code
        Reachable = ($code -in 200, 206)
        WallMs    = [int]$watch.Elapsed.TotalMilliseconds
    }
}

if ($Json) {
    $results | ConvertTo-Json -Depth 3
} else {
    $results |
        Sort-Object Tool, @{ Expression = 'Reachable'; Descending = $true }, WallMs |
        Format-Table -AutoSize
}
