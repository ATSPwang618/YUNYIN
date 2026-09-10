<#
  YUNYIN 曲库体检 —— 在 PC 上先看清楚「播放器会怎么读你的标签」，再去修。

  判定规则与 Vita 上的播放器完全一致（native/media.rs）：
    * 只读文件开头 1MB + 64KB 的元数据前缀
    * MP3   : ID3v2 的 TIT2 / TPE1 / TALB / APIC / USLT
              编码字节 0(Latin-1) 与 3(UTF-8) 都按 UTF-8 解，1 / 2 按 UTF-16 小端解
              → 国内工具常写 enc=0 却塞 GBK/Big5 字节，播放器就会显示乱码（本脚本会指出来并给出正确文字）
    * FLAC  : Vorbis comment（TITLE / ARTIST / ALBUM / LYRICS）+ PICTURE
    * OGG / OPUS : Vorbis comment
    * 封面  : 只认内嵌的 JPEG / PNG，且不超过 1MB，超了播放器直接忽略
    * WAV   : 播放器不读标签，只用文件名
    * ID3v1（写在文件尾的那种）播放器不读

  用法：
    powershell -ExecutionPolicy Bypass -File check-tags.ps1 -Path "D:\Music"
    powershell -ExecutionPolicy Bypass -File check-tags.ps1 -Path "D:\Music" -Csv report.csv -Playlist music-to-fix.m3u8

  参数：
    -Path           音乐文件夹（必填）
    -Csv            导出明细 CSV
    -Playlist       把「需修复」的文件写成一个 m3u8，拖进 MusicBrainz Picard 即可一次性载入
    -OnlyProblems   只在控制台列需要处理的文件
    -MaxFiles       最多扫描多少首（默认 2000）
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)][string]$Path,
    [string]$Csv,
    [string]$Playlist,
    [switch]$OnlyProblems,
    [int]$MaxFiles = 2000
)

$ErrorActionPreference = 'Stop'

$AUDIO_EXT   = @('.mp3', '.flac', '.ogg', '.oga', '.opus', '.wav')
$PREFIX_CAP  = 1024 * 1024 + 65536      # 与 media.rs 的 PREFIX_CAP 一致
$MAX_ART     = 1024 * 1024              # 封面超过 1MB 播放器会忽略
$PICARD_URL  = 'https://picard.musicbrainz.org/'

if (-not (Test-Path -LiteralPath $Path)) {
    Write-Host "路径不存在：$Path" -ForegroundColor Red
    exit 1
}

# ---------------------------------------------------------------------------
# 字节读取
# ---------------------------------------------------------------------------
function Read-Prefix([string]$file, [int]$cap) {
    $fs = [IO.File]::OpenRead($file)
    try {
        $len = [int][Math]::Min([int64]$cap, $fs.Length)
        if ($len -le 0) { return (New-Object byte[] 0) }
        $buf = New-Object byte[] $len
        $read = 0
        while ($read -lt $len) {
            $n = $fs.Read($buf, $read, $len - $read)
            if ($n -le 0) { break }
            $read += $n
        }
        if ($read -lt $len) { return [byte[]]$buf[0..($read - 1)] }
        return $buf
    } finally { $fs.Dispose() }
}

function Read-Tail([string]$file, [int]$count) {
    $fs = [IO.File]::OpenRead($file)
    try {
        $len = [int][Math]::Min([int64]$count, $fs.Length)
        if ($len -le 0) { return (New-Object byte[] 0) }
        $fs.Seek(([int64](-1) * $len), [IO.SeekOrigin]::End) | Out-Null
        $buf = New-Object byte[] $len
        $read = 0
        while ($read -lt $len) {
            $n = $fs.Read($buf, $read, $len - $read)
            if ($n -le 0) { break }
            $read += $n
        }
        return $buf
    } finally { $fs.Dispose() }
}

# ---------------------------------------------------------------------------
# 解码：复刻播放器的做法，另外猜一下真实编码
# ---------------------------------------------------------------------------
function Test-Utf8([byte[]]$b) {
    try {
        $enc = New-Object Text.UTF8Encoding($false, $true)
        $null = $enc.GetString($b)
        return $true
    } catch { return $false }
}

function Try-CodePage([byte[]]$b, [int]$cp) {
    try { return [Text.Encoding]::GetEncoding($cp).GetString($b) } catch { return $null }
}

function Translate-Id3Text([byte[]]$data) {
    # 返回 @{ app; fixed; broken; note }
    if ($null -eq $data -or $data.Length -lt 1) {
        return @{ app = ''; fixed = ''; broken = $false; note = '' }
    }
    $enc = $data[0]
    $bytes = if ($data.Length -gt 1) { [byte[]]$data[1..($data.Length - 1)] } else { New-Object byte[] 0 }

    if ($enc -eq 1 -or $enc -eq 2) {
        # 播放器两种都按小端解
        $app = [Text.Encoding]::Unicode.GetString($bytes)
        $app = ($app -split "`0")[0].Trim()
        $be = [Text.Encoding]::BigEndianUnicode.GetString($bytes)
        $be = ($be -split "`0")[0].Trim()
        $appHasCjk = $app -match '[\u3040-\u30ff\u3400-\u9fff\uff01-\uff5e]'
        $beHasCjk = $be -match '[\u3040-\u30ff\u3400-\u9fff\uff01-\uff5e]'
        if (($bytes.Length -ge 2 -and $bytes[0] -eq 0xFE -and $bytes[1] -eq 0xFF) -or (-not $appHasCjk -and $beHasCjk)) {
            return @{ app = $app; fixed = $be; broken = $true; note = 'UTF-16 大端标签：播放器按小端解，会乱码' }
        }
        return @{ app = $app; fixed = $app; broken = $false; note = '' }
    }

    # enc 0 / 3：播放器一律按 UTF-8 解
    $app = [Text.Encoding]::UTF8.GetString($bytes)
    $app = ($app -split "`0")[0].Trim()
    if ($bytes.Length -eq 0 -or (Test-Utf8 $bytes)) {
        return @{ app = $app; fixed = $app; broken = $false; note = '' }
    }
    foreach ($cp in @(936, 950, 932)) {
        $guess = Try-CodePage $bytes $cp
        if ($null -ne $guess -and $guess -match '[\u3040-\u30ff\u3400-\u9fff\uff01-\uff5e]') {
            $label = switch ($cp) { 936 { 'GBK/GB18030' } 950 { 'Big5' } 932 { 'Shift-JIS' } }
            return @{ app = $app; fixed = $guess.Trim(); broken = $true; note = "编码像 $label，播放器会显示乱码" }
        }
    }
    return @{ app = $app; fixed = $null; broken = $true; note = '标签不是合法 UTF-8，播放器会显示乱码' }
}

# ---------------------------------------------------------------------------
# MP3 / ID3v2
# ---------------------------------------------------------------------------
function Get-Synchsafe([byte[]]$b, [int]$o) {
    return (([int]$b[$o] -band 0x7F) * 2097152) + (([int]$b[$o + 1] -band 0x7F) * 16384) + (([int]$b[$o + 2] -band 0x7F) * 128) + ([int]$b[$o + 3] -band 0x7F)
}

function Get-Be32([byte[]]$b, [int]$o) {
    return ([int]$b[$o] * 16777216) + ([int]$b[$o + 1] * 65536) + ([int]$b[$o + 2] * 256) + [int]$b[$o + 3]
}

function Get-Le32([byte[]]$b, [int]$o) {
    return [int]$b[$o] + ([int]$b[$o + 1] * 256) + ([int]$b[$o + 2] * 65536) + ([int]$b[$o + 3] * 16777216)
}

function Get-Id3v2([byte[]]$bytes) {
    if ($bytes.Length -lt 10) { return $null }
    if (-not ($bytes[0] -eq 0x49 -and $bytes[1] -eq 0x44 -and $bytes[2] -eq 0x33)) { return $null }
    $major = $bytes[3]
    $flags = $bytes[5]
    $tagSize = Get-Synchsafe $bytes 6
    $end = [Math]::Min(10 + $tagSize, $bytes.Length)
    $frames = @{}
    if ($major -eq 2) {
        return @{ version = 2; size = $tagSize; frames = $frames; readable = $false }
    }
    $pos = 10
    if (($flags -band 0x40) -ne 0 -and ($pos + 4) -le $end) {
        $ext = if ($major -ge 4) { Get-Synchsafe $bytes $pos } else { Get-Be32 $bytes $pos }
        $pos = [Math]::Min($pos + [Math]::Max($ext, 4), $end)
    }
    while (($pos + 10) -le $end) {
        if ($bytes[$pos] -eq 0) { break }
        $id = [Text.Encoding]::ASCII.GetString($bytes, $pos, 4)
        $fsz = if ($major -ge 4) { Get-Synchsafe $bytes ($pos + 4) } else { Get-Be32 $bytes ($pos + 4) }
        $pos += 10
        if ($fsz -le 0 -or ($pos + $fsz) -gt $end) { break }
        if (-not $frames.ContainsKey($id)) {
            $frames[$id] = [byte[]]$bytes[$pos..($pos + $fsz - 1)]
        }
        $pos += $fsz
    }
    return @{ version = $major; size = $tagSize; frames = $frames; readable = $true }
}

function Find-Image([byte[]]$b) {
    # 播放器也是在 APIC / PICTURE 数据里搜 JPEG / PNG 魔数
    if ($null -eq $b) { return $null }
    for ($i = 0; $i + 3 -lt $b.Length; $i++) {
        if ($b[$i] -eq 0xFF -and $b[$i + 1] -eq 0xD8) {
            return @{ kind = 'JPEG'; size = $b.Length - $i }
        }
        if ($b[$i] -eq 0x89 -and $b[$i + 1] -eq 0x50 -and $b[$i + 2] -eq 0x4E -and $b[$i + 3] -eq 0x47) {
            return @{ kind = 'PNG'; size = $b.Length - $i }
        }
    }
    return $null
}

# ---------------------------------------------------------------------------
# FLAC / Vorbis comment
# ---------------------------------------------------------------------------
function Get-FlacInfo([byte[]]$bytes) {
    if ($bytes.Length -lt 8) { return $null }
    if (-not ($bytes[0] -eq 0x66 -and $bytes[1] -eq 0x4C -and $bytes[2] -eq 0x61 -and $bytes[3] -eq 0x43)) { return $null }
    $pos = 4
    $comment = $null
    $pic = $null
    while (($pos + 4) -le $bytes.Length) {
        $h = $bytes[$pos]
        $last = ($h -band 0x80) -ne 0
        $type = $h -band 0x7F
        $len = ([int]$bytes[$pos + 1] * 65536) + ([int]$bytes[$pos + 2] * 256) + [int]$bytes[$pos + 3]
        $pos += 4
        if ($len -lt 0 -or ($pos + $len) -gt $bytes.Length) { break }
        $block = [byte[]]$bytes[$pos..($pos + $len - 1)]
        if ($type -eq 4) { $comment = $block }
        elseif ($type -eq 6 -and $null -eq $pic) {
            # PICTURE: type(4) mimeLen(4) mime descLen(4) desc w(4) h(4) depth(4) colors(4) dataLen(4) data
            if ($block.Length -gt 32) {
                $mimeLen = Get-Be32 $block 4
                $o = 8 + $mimeLen
                if (($o + 4) -le $block.Length) {
                    $descLen = Get-Be32 $block $o
                    $o = $o + 4 + $descLen + 16
                    if (($o + 4) -le $block.Length) {
                        $dataLen = Get-Be32 $block $o
                        $pic = @{ kind = 'FLAC PICTURE'; size = $dataLen }
                    }
                }
            }
        }
        $pos += $len
        if ($last) { break }
    }
    return @{ comment = $comment; picture = $pic }
}

function Get-VorbisComments([byte[]]$body) {
    # Vorbis comment 结构：vendorLength(LE32) + vendor + count(LE32) + count × [len(LE32) + "KEY=value"]
    # 键名大小写不敏感（libvorbis 写的是小写 title=，播放器也是 eq_ignore_ascii_case）
    $out = @{}
    if ($null -eq $body -or $body.Length -lt 8) { return $out }
    $vendor = Get-Le32 $body 0
    $p = 4 + $vendor
    if (($p + 4) -gt $body.Length) { return $out }
    $count = Get-Le32 $body $p
    $p += 4
    if ($count -gt 4096) { $count = 4096 }
    for ($i = 0; $i -lt $count; $i++) {
        if (($p + 4) -gt $body.Length) { break }
        $n = Get-Le32 $body $p
        $p += 4
        if ($n -lt 0 -or ($p + $n) -gt $body.Length) { break }
        $comment = [Text.Encoding]::UTF8.GetString($body, $p, $n)
        $p += $n
        $eq = $comment.IndexOf('=')
        if ($eq -gt 0) {
            $k = $comment.Substring(0, $eq).ToUpper()
            if (-not $out.ContainsKey($k)) { $out[$k] = $comment.Substring($eq + 1).Trim() }
        }
    }
    return $out
}

function Get-OggComments([byte[]]$bytes) {
    # 在 Ogg 数据里找 comment header（"\x03vorbis"），并把后面的 comment 数据交给解析器
    if ($null -eq $bytes) { return @{} }
    for ($i = 0; $i + 7 -lt $bytes.Length; $i++) {
        if ($bytes[$i] -eq 0x03 -and $bytes[$i + 1] -eq 0x76 -and $bytes[$i + 2] -eq 0x6F -and
            $bytes[$i + 3] -eq 0x72 -and $bytes[$i + 4] -eq 0x62 -and $bytes[$i + 5] -eq 0x69 -and
            $bytes[$i + 6] -eq 0x73) {
            $rest = [byte[]]$bytes[($i + 7)..($bytes.Length - 1)]
            return Get-VorbisComments $rest
        }
    }
    return @{}
}

# ---------------------------------------------------------------------------
# 单个文件体检
# ---------------------------------------------------------------------------
function Test-Track([string]$file) {
    $ext = [IO.Path]::GetExtension($file).ToLower()
    $name = [IO.Path]::GetFileNameWithoutExtension($file)
    $bytes = Read-Prefix $file $PREFIX_CAP
    # 用 PSCustomObject（而不是 OrderedDictionary），Export-Csv / Select-Object 才认这些字段
    $row = [pscustomobject][ordered]@{
        File = $file; Format = $ext.TrimStart('.').ToUpper()
        Title = ''; Artist = ''; Album = ''
        Cover = ''; Lyrics = ''
        Status = ''; Notes = @()
    }
    $notes = New-Object System.Collections.ArrayList
    $brokenEncoding = $false

    if ($ext -eq '.mp3') {
        $id3 = Get-Id3v2 $bytes
        # ID3v1 写在文件尾（播放器不读），无论有没有 ID3v2 都先看一眼
        $tail = Read-Tail $file 128
        $hasV1 = ($tail.Length -ge 128 -and $tail[0] -eq 0x54 -and $tail[1] -eq 0x41 -and $tail[2] -eq 0x47)
        if ($null -eq $id3) {
            if ($hasV1) {
                [void]$notes.Add('只有 ID3v1 标签（播放器不读，会退回文件名）')
            } else {
                [void]$notes.Add('没有 ID3v2 标签（播放器会拿文件名当标题）')
            }
        } elseif (-not $id3.readable) {
            [void]$notes.Add('ID3v2.2 标签：帧格式太老，播放器读不了，建议用 Picard 重存')
        } else {
            foreach ($pair in @(@('TIT2', 'Title'), @('TPE1', 'Artist'), @('TALB', 'Album'))) {
                if ($id3.frames.ContainsKey($pair[0])) {
                    $r = Translate-Id3Text $id3.frames[$pair[0]]
                    $row[$pair[1]] = $r.app
                    if ($r.broken) {
                        $brokenEncoding = $true
                        $fix = if ($r.fixed) { "（正确内容应为「" + $r.fixed + "」）" } else { '' }
                        [void]$notes.Add($pair[1] + '：' + $r.note + $fix)
                    }
                }
            }
            if ($id3.frames.ContainsKey('APIC')) {
                $img = Find-Image $id3.frames['APIC']
                if ($null -ne $img) {
                    if ($img.size -gt $MAX_ART) {
                        $row.Cover = "有(" + $img.kind + " >1MB)"
                        [void]$notes.Add('内嵌封面超过 1MB：播放器会忽略，请压到 1MB 以内')
                    } else {
                        $row.Cover = '有(' + $img.kind + ')'
                    }
                } else {
                    $row.Cover = '有(格式不支持)'
                    [void]$notes.Add('内嵌封面不是 JPEG/PNG：播放器认不出来')
                }
            }
            $lyr = ''
            if ($id3.frames.ContainsKey('USLT')) {
                $lyr = (Translate-Id3Text $id3.frames['USLT']).app
            }
            if (-not $lyr.Trim() -and $id3.frames.ContainsKey('TXXX')) {
                $t = (Translate-Id3Text $id3.frames['TXXX']).app
                if ($t -match '(?i)lyrics') { $lyr = $t }
            }
            if ($lyr.Trim()) { $row.Lyrics = '有' }
            if ($hasV1 -and -not $row.Title -and -not $row.Artist -and -not $row.Album) {
                [void]$notes.Add('ID3v2 里没有歌名/歌手/专辑，只有文件尾的 ID3v1（播放器不读）')
            }
        }
    }
    elseif ($ext -eq '.flac') {
        $flac = Get-FlacInfo $bytes
        if ($null -eq $flac) {
            [void]$notes.Add('不是标准 FLAC 头，播放器可能读不到标签')
        } else {
            $f = Get-VorbisComments $flac.comment
            if ($f.ContainsKey('TITLE')) { $row.Title = $f['TITLE'] }
            if ($f.ContainsKey('ARTIST')) { $row.Artist = $f['ARTIST'] }
            if ($f.ContainsKey('ALBUM')) { $row.Album = $f['ALBUM'] }
            if ($f.ContainsKey('LYRICS')) { $row.Lyrics = '有' }
            if ($null -ne $flac.picture) {
                if ($flac.picture.size -gt $MAX_ART) {
                    $row.Cover = '有(>1MB)'
                    [void]$notes.Add('内嵌封面超过 1MB：播放器会忽略')
                } else {
                    $row.Cover = '有(' + $flac.picture.kind + ')'
                }
            }
        }
    }
    elseif ($ext -eq '.ogg' -or $ext -eq '.oga' -or $ext -eq '.opus') {
        $f = Get-OggComments $bytes
        if ($f.ContainsKey('TITLE')) { $row.Title = $f['TITLE'] }
        if ($f.ContainsKey('ARTIST')) { $row.Artist = $f['ARTIST'] }
        if ($f.ContainsKey('ALBUM')) { $row.Album = $f['ALBUM'] }
        if ($f.ContainsKey('LYRICS')) { $row.Lyrics = '有' }
        if ($f.ContainsKey('METADATA_BLOCK_PICTURE') -or $f.ContainsKey('COVERART')) { $row.Cover = '有(OGG 内嵌)' }
    }
    elseif ($ext -eq '.wav') {
        [void]$notes.Add('WAV：播放器不读标签，只用文件名')
    }

    if (-not $row.Title) { $row.Title = '（缺，用文件名）' }
    if (-not $row.Artist) { $row.Artist = '（缺 → Local）' }
    if (-not $row.Album) { $row.Album = '（缺 → Unknown）' }
    if (-not $row.Cover) { $row.Cover = '无' }
    if (-not $row.Lyrics) { $row.Lyrics = '无' }

    $missing = New-Object System.Collections.ArrayList
    if ($row.Title -like '（缺*') { [void]$missing.Add('标题') }
    if ($row.Artist -like '（缺*') { [void]$missing.Add('歌手') }
    if ($row.Album -like '（缺*') { [void]$missing.Add('专辑') }
    if ($row.Cover -eq '无') { [void]$missing.Add('封面') }
    if ($row.Lyrics -eq '无') { [void]$missing.Add('歌词') }

    if ($brokenEncoding) {
        $row.Status = '乱码'
    } elseif ($missing.Count -ge 4) {
        $row.Status = '建议整理'
    } elseif ($missing.Count -ge 1) {
        $row.Status = '可改善'
    } else {
        $row.Status = '完整'
    }
    if ($missing.Count -gt 0) {
        [void]$notes.Insert(0, '缺：' + ($missing -join ' / '))
    }
    $row.Notes = ($notes -join '；')
    return $row
}

# ---------------------------------------------------------------------------
# 主流程
# ---------------------------------------------------------------------------
$root = (Resolve-Path -LiteralPath $Path).Path
$allFiles = Get-ChildItem -LiteralPath $root -Recurse -File -ErrorAction SilentlyContinue |
    Where-Object { $AUDIO_EXT -contains $_.Extension.ToLower() }
$skipped = (Get-ChildItem -LiteralPath $root -Recurse -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Extension -match '^\.(m4a|aac|wma|aiff|aif|mp4|flv|ape|wv)$' }).Count
if ($allFiles.Count -gt $MaxFiles) { $allFiles = $allFiles | Select-Object -First $MaxFiles }

Write-Host ''
Write-Host ('YUNYIN 曲库体检  ' + $root) -ForegroundColor Cyan
Write-Host ('扫描 ' + $allFiles.Count + ' 首' + $(if ($skipped -gt 0) { '（另有 ' + $skipped + ' 个播放器不支持的格式已跳过）' } else { '' })) -ForegroundColor DarkGray
Write-Host ''

$rows = New-Object System.Collections.ArrayList
$i = 0
foreach ($f in $allFiles) {
    $i++
    Write-Progress -Activity '读取标签' -Status $f.Name -PercentComplete ([int](100 * $i / [Math]::Max(1, $allFiles.Count)))
    try { [void]$rows.Add((Test-Track $f.FullName)) }
    catch { Write-Host ('  读取失败：' + $f.FullName + ' —— ' + $_.Exception.Message) -ForegroundColor Yellow }
}
Write-Progress -Activity '读取标签' -Completed

$problems = $rows | Where-Object { $_.Status -ne '完整' }
$show = if ($OnlyProblems) { $problems } else { $rows }

foreach ($r in $show) {
    $color = switch ($r.Status) { '完整' { 'Green' } '可改善' { 'Yellow' } '建议整理' { 'Yellow' } '乱码' { 'Red' } }
    Write-Host ('[' + $r.Status.PadRight(4) + '] ' + [IO.Path]::GetFileName($r.File)) -ForegroundColor $color
    if ($r.Status -ne '完整') {
        Write-Host ('        ' + $r.Notes) -ForegroundColor DarkGray
    }
}

# 注意：@() 不能省 —— 只匹配到一条时 Where-Object 返回单个对象，
# 对它取 .Count 会得到字典的键数（9），不是命中数量。
$okCount = @($rows | Where-Object { $_.Status -eq '完整' }).Count
$badCount = @($problems).Count
$garbled = @($rows | Where-Object { $_.Status -eq '乱码' }).Count
$noTitle = @($rows | Where-Object { $_.Title -like '（缺*' }).Count
$noArtist = @($rows | Where-Object { $_.Artist -like '（缺*' }).Count
$noAlbum = @($rows | Where-Object { $_.Album -like '（缺*' }).Count
$noCover = @($rows | Where-Object { $_.Cover -eq '无' }).Count
$noLyrics = @($rows | Where-Object { $_.Lyrics -eq '无' }).Count

Write-Host ''
Write-Host '-----------------------------------------------' -ForegroundColor DarkGray
Write-Host ('完整 ' + $okCount + ' 首 / 需处理 ' + $badCount + ' 首' + $(if ($garbled -gt 0) { '（其中乱码 ' + $garbled + ' 首）' } else { '' })) -ForegroundColor Cyan
Write-Host ('缺标题 ' + $noTitle + ' · 缺歌手 ' + $noArtist + ' · 缺专辑 ' + $noAlbum + ' · 无封面 ' + $noCover + ' · 无歌词 ' + $noLyrics) -ForegroundColor DarkGray
Write-Host '-----------------------------------------------' -ForegroundColor DarkGray
Write-Host ('整理工具：MusicBrainz Picard  ' + $PICARD_URL) -ForegroundColor DarkGray
Write-Host '  1) Add Folder 或直接拖入本脚本生成的 m3u8' -ForegroundColor DarkGray
Write-Host '  2) 全选 → Lookup（按声学指纹匹配，不认识中文也能自动认）' -ForegroundColor DarkGray
Write-Host '  3) Save —— 标签会被重写成规范的 UTF-8 / UTF-16，乱码一并解决' -ForegroundColor DarkGray
Write-Host '  4) 想要封面/歌词：Options → Metadata 里勾上 Cover Art、Lyrics' -ForegroundColor DarkGray

if ($Csv) {
    $csvPath = [IO.Path]::GetFullPath($Csv)
    $rows | Select-Object File, Format, Title, Artist, Album, Cover, Lyrics, Status, Notes |
        Export-Csv -LiteralPath $csvPath -NoTypeInformation -Encoding UTF8
    Write-Host ('明细已导出：' + $csvPath) -ForegroundColor Green
}

if ($Playlist) {
    $plPath = [IO.Path]::GetFullPath($Playlist)
    $lines = New-Object System.Collections.ArrayList
    [void]$lines.Add('#EXTM3U')
    foreach ($r in $problems) { [void]$lines.Add($r.File) }
    [IO.File]::WriteAllLines($plPath, $lines, (New-Object Text.UTF8Encoding($false)))
    Write-Host ('待修清单已导出：' + $plPath + '（' + $badCount + ' 首，拖进 Picard 即可）') -ForegroundColor Green
}

Write-Host ''
if ($badCount -eq 0) {
    Write-Host '全部歌曲的标签都能被播放器正常读取。' -ForegroundColor Green
} else {
    Write-Host ('有 ' + $badCount + ' 首需要处理，照上面的 Picard 步骤走一遍即可。') -ForegroundColor Yellow
}
