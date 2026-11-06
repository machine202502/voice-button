Add-Type -AssemblyName System.Drawing

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$pngPath = Join-Path $root "assets\icon.png"
$icoPath = Join-Path $root "assets\icon.ico"

function New-RoundedSquarePath {
    param([float]$Size, [float]$Radius)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = $Radius
    $d = $r * 2
    $path.AddArc(0, 0, $d, $d, 180, 90)
    $path.AddArc($Size - $d, 0, $d, $d, 270, 90)
    $path.AddArc($Size - $d, $Size - $d, $d, $d, 0, 90)
    $path.AddArc(0, $Size - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    return $path
}

function New-RoundedBarPath {
    param([float]$X, [float]$Y, [float]$W, [float]$H)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    if ($W -ge $H) {
        $path.AddArc($X, $Y, $H, $H, 90, 180)
        $path.AddArc($X + $W - $H, $Y, $H, $H, 270, 180)
    } else {
        $path.AddArc($X, $Y, $W, $W, 180, 180)
        $path.AddArc($X, $Y + $H - $W, $W, $W, 0, 180)
    }
    $path.CloseFigure()
    return $path
}

function New-MicBitmap {
    param([int]$Size)
    $bmp = New-Object System.Drawing.Bitmap($Size, $Size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = "AntiAlias"
    $k = $Size / 512.0

    $bg = New-RoundedSquarePath -Size $Size -Radius (96 * $k)
    $brush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 34, 42, 56))
    $g.FillPath($brush, $bg)

    $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 240, 244, 248))
    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 240, 244, 248)), (42 * $k)
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round

    $capsule = New-RoundedBarPath -X (192 * $k) -Y (76 * $k) -W (128 * $k) -H (240 * $k)
    $g.FillPath($white, $capsule)

    $g.DrawArc($pen, (156 * $k), (160 * $k), (200 * $k), (200 * $k), 0, 180)

    $stemW = 46 * $k
    $stemX = ($Size - $stemW) / 2.0
    $g.FillRectangle($white, $stemX, (360 * $k), $stemW, (60 * $k))

    $foot = New-RoundedBarPath -X (161 * $k) -Y (428 * $k) -W (190 * $k) -H (42 * $k)
    $g.FillPath($white, $foot)

    $g.Dispose()
    return $bmp
}

$base = New-MicBitmap -Size 512
$base.Save($pngPath, [System.Drawing.Imaging.ImageFormat]::Png)

$sizes = 16, 24, 32, 48, 64, 128, 256
$pngBytes = @()
foreach ($s in $sizes) {
    $resized = New-MicBitmap -Size $s
    $ms = New-Object System.IO.MemoryStream
    $resized.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngBytes += ,@{ Size = $s; Data = $ms.ToArray() }
    $ms.Dispose()
    $resized.Dispose()
}

$ico = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter($ico)
$bw.Write([UInt16]0)
$bw.Write([UInt16]1)
$bw.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
foreach ($entry in $pngBytes) {
    $sz = if ($entry.Size -ge 256) { 0 } else { $entry.Size }
    $bw.Write([Byte]$sz)
    $bw.Write([Byte]$sz)
    $bw.Write([Byte]0)
    $bw.Write([Byte]0)
    $bw.Write([UInt16]1)
    $bw.Write([UInt16]32)
    $bw.Write([UInt32]$entry.Data.Length)
    $bw.Write([UInt32]$offset)
    $offset += $entry.Data.Length
}
foreach ($entry in $pngBytes) {
    $bw.Write($entry.Data)
}
$bw.Flush()
[System.IO.File]::WriteAllBytes($icoPath, $ico.ToArray())
$bw.Dispose()
$ico.Dispose()
$base.Dispose()

Write-Output "written: $pngPath and $icoPath"
