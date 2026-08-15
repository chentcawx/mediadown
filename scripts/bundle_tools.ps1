# bundle_tools.ps1 — 把混流工具同步进绿色版目录
#
# 用途：cargo build 之后运行，确保 MediaDown-x86/tools/ 始终带有混流工具，
#       避免「重编后漏拷 tools」导致下载完成不自动混流（静默 skip）。
#
# 工具来源（按优先级）：
#   1. $env:MD_FFMPEG / $env:MD_MKVMERGE 显式指定
#   2. 仓库根 tools/ 下已有的 ffmpeg.exe / mkvmerge.exe
#   3. 系统 PATH 中可解析到的 ffmpeg.exe / mkvmerge.exe（仅本地复制，不下载）
#
# 本脚本只做本地文件复制，绝不联网下载第三方二进制。
# 若三处都找不到，仅打印指引并退出 0（不阻断主构建）。

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path          # .../mediadown
$GreenTools = Join-Path $Root 'MediaDown-x86' 'tools'
$SrcTools   = Join-Path $Root 'tools'

function Find-Tool([string]$name) {
    # 1) 环境变量
    $envVar = if ($name -eq 'ffmpeg.exe') { 'MD_FFMPEG' } else { 'MD_MKVMERGE' }
    if ($env:$envVar) {
        if (Test-Path $env:$envVar) { return $env:$envVar }
    }
    # 2) 仓库根 tools/
    $c = Join-Path $SrcTools $name
    if (Test-Path $c) { return $c }
    # 3) PATH
    $onPath = Get-Command $name -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }
    return $null
}

$candidates = @('ffmpeg.exe', 'mkvmerge.exe')
New-Item -ItemType Directory -Force -Path $GreenTools | Out-Null

$foundAny = $false
foreach ($name in $candidates) {
    $src = Find-Tool $name
    if (-not $src) {
        Write-Host "[bundle-tools] 未找到 $name（如需自动混流请放到 tools/ 或加入 PATH）"
        continue
    }
    $dst = Join-Path $GreenTools $name
    # 仅在源更新时复制，避免每次构建都拷 175MB
    $copy = $true
    if (Test-Path $dst) {
        $s = Get-Item $src; $d = Get-Item $dst
        if ($s.Length -eq $d.Length -and $s.LastWriteTime -le $d.LastWriteTime) { $copy = $false }
    }
    if ($copy) {
        Copy-Item $src $dst -Force
        Write-Host "[bundle-tools] 已复制 $name -> $dst"
    } else {
        Write-Host "[bundle-tools] $name 已是最新，跳过"
    }
    $foundAny = $true
}

if (-not $foundAny) {
    Write-Host "[bundle-tools] 未发现任何混流工具；下载完成后将跳过自动混流（仅提示）。"
    Write-Host "[bundle-tools] 获取方式：将 ffmpeg.exe（或 mkvmerge.exe）放入 $SrcTools 即可。"
}
