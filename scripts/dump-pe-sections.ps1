# PE 节表转储 + 槽位定位。
#
# 起因：release 模板的槽位在**文件里**被成功改写（自检读回来是新的），
# 但运行时 `&SLOT` 读到的仍是原始值 —— 于是 shim 自称"模板"。
# 唯一能解释这件事的是：被改写的那些字节**不在节的 raw data 窗口里**，
# 也就是加载器根本不会把文件里的那些字节映射到该虚拟地址上。
#
# 这个脚本把节表打出来，并回答一个问题：槽位落在哪个节、在不在 raw data 里。

param([Parameter(Mandatory)][string]$Path)

$b = [System.IO.File]::ReadAllBytes($Path)

function U16($at) { [BitConverter]::ToUInt16($b, $at) }
function U32($at) { [BitConverter]::ToUInt32($b, $at) }

$peOffset = U32 0x3C
if ($b[$peOffset] -ne 0x50 -or $b[$peOffset+1] -ne 0x45) { throw "不是 PE 文件" }

$machine = U16 ($peOffset + 4)
$numSections = U16 ($peOffset + 6)
$optSize = U16 ($peOffset + 20)
$optAt = $peOffset + 24

$magic = U16 $optAt
$sectionAlignment = U32 ($optAt + 32)
$fileAlignment = U32 ($optAt + 36)
$sizeOfImage = U32 ($optAt + 56)
$sizeOfHeaders = U32 ($optAt + 60)

Write-Output "===== $Path（$($b.Length) 字节）====="
Write-Output ("PE32+={0}  节数={1}  SectionAlignment=0x{2:X}  FileAlignment=0x{3:X}" -f ($magic -eq 0x20B), $numSections, $sectionAlignment, $fileAlignment)
Write-Output ("SizeOfImage=0x{0:X}  SizeOfHeaders=0x{1:X}" -f $sizeOfImage, $sizeOfHeaders)
Write-Output ""
Write-Output ("{0,-10} {1,10} {2,10} {3,10} {4,10} {5,12}" -f '节名','VirtualSize','VirtAddr','RawSize','RawPtr','RVA-Raw差')
$sections = @()
for ($i = 0; $i -lt $numSections; $i++) {
  $at = $optAt + $optSize + $i * 40
  $name = ([System.Text.Encoding]::ASCII.GetString($b, $at, 8)).TrimEnd([char]0)
  $vsize = U32 ($at + 8)
  $vaddr = U32 ($at + 12)
  $rawsize = U32 ($at + 16)
  $rawptr = U32 ($at + 20)
  $sections += [pscustomobject]@{ Name=$name; VSize=$vsize; VAddr=$vaddr; RawSize=$rawsize; RawPtr=$rawptr }
  Write-Output ("{0,-10} {1,10:X} {2,10:X} {3,10:X} {4,10:X} {5,12:X}" -f $name, $vsize, $vaddr, $rawsize, $rawptr, ($vaddr - $rawptr))
}

Write-Output ""
$magicBytes = [byte[]](0x54,0x55,0x4F,0x45,0x4E,0x53,0x48,0x49,0x4D,0x00,0x76,0x31,0x00,0x00,0x00,0x00)
$SLOT = 2076
$hits = @()
for ($i = 0; $i -le $b.Length - 16; $i++) {
  $ok = $true
  for ($j = 0; $j -lt 16; $j++) { if ($b[$i+$j] -ne $magicBytes[$j]) { $ok = $false; break } }
  if ($ok) { $hits += $i }
}
Write-Output "魔数出现 $($hits.Count) 次：$($hits -join ', ')"
$fitsCount = 0
$pristineCount = 0
foreach ($h in $hits) {
  $sec = $sections | Where-Object { $h -ge $_.RawPtr -and $h -lt ($_.RawPtr + $_.RawSize) }
  $tail = $h + $SLOT
  $end = if ($sec) { $sec.RawPtr + $sec.RawSize } else { 0 }
  $fits = $sec -and ($tail -le $end)
  $va = if ($sec) { $sec.VAddr + ($h - $sec.RawPtr) } else { 0 }
  Write-Output ("  文件偏移 0x{0:X}  节 {1}  映射到的 RVA 0x{2:X}  槽位末端 0x{3:X}" -f $h, ($(if ($sec) { $sec.Name } else { '<不在任何节的 raw data 里>' })), $va, $tail)
  if ($sec) {
    Write-Output ("     该节 raw data 窗口: 0x{0:X} .. 0x{1:X}（差 {2} 字节）→ 整个槽位都在 raw data 里 = {3}" -f $sec.RawPtr, $end, ($end - $tail), $fits)
    if ($fits) { $fitsCount++ }
  }
  # "未烘过" = 魔数 + 长度与保留字段为 0 + 哨兵正确 —— 也就是整段等于 baked_slot()。
  $clean = $true
  if ($tail -gt $b.Length) {
    $clean = $false
  } else {
    for ($k = $h + 16; $k -lt $h + 2072; $k++) { if ($b[$k] -ne 0) { $clean = $false; break } }
    if ($clean -and ($b[$h+2072] -ne 0x5A -or $b[$h+2073] -ne 0x5A -or $b[$h+2074] -ne 0x5A -or $b[$h+2075] -ne 0x5A)) { $clean = $false }
  }
  Write-Output ("     是「未烘过」的槽位 = {0}" -f $clean)
  if ($clean) { $pristineCount++ }
}

# 机器可读的结论行。
#
# **加这一行的理由**：验收脚本第一版去正则匹配一句这个脚本从来没打印过的中文
# （"真槽位 N 处"），于是给出了一次**假失败**。解析别人输出的脚本，
# 必须先有一个明确的、约定好的契约行 —— 而不是从散文里捞数字。
Write-Output ""
Write-Output ("SUMMARY sections={0} magic_hits={1} pristine={2} slots_inside_raw={3}" -f `
    $numSections, $hits.Count, $pristineCount, $fitsCount)
