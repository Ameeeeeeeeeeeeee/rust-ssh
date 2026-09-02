$ErrorActionPreference = "Stop"

Add-Type -AssemblyName System.Drawing

function New-RustSshPng {
    param([int]$Size)

    $scale = $Size / 64.0
    $bitmap = [System.Drawing.Bitmap]::new(
        $Size,
        $Size,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.Clear([System.Drawing.Color]::Transparent)

    $background = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $left = 8.0 * $scale
    $top = 4.0 * $scale
    $width = 48.0 * $scale
    $height = 56.0 * $scale
    $radius = 12.0 * $scale
    $diameter = $radius * 2.0
    $background.AddArc($left, $top, $diameter, $diameter, 180, 90)
    $background.AddArc($left + $width - $diameter, $top, $diameter, $diameter, 270, 90)
    $background.AddArc($left + $width - $diameter, $top + $height - $diameter, $diameter, $diameter, 0, 90)
    $background.AddArc($left, $top + $height - $diameter, $diameter, $diameter, 90, 90)
    $background.CloseFigure()

    $backgroundBrush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 20, 32, 49))
    $graphics.FillPath($backgroundBrush, $background)

    $lineWidth = [Math]::Max(1.0, 2.5 * $scale)
    $border = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 52, 207, 185), $lineWidth)
    $border.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $terminal = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $terminalLeft = 17.0 * $scale
    $terminalTop = 16.0 * $scale
    $terminalWidth = 30.0 * $scale
    $terminalHeight = 32.0 * $scale
    $terminalRadius = 5.0 * $scale
    $terminalDiameter = $terminalRadius * 2.0
    $terminal.AddArc($terminalLeft, $terminalTop, $terminalDiameter, $terminalDiameter, 180, 90)
    $terminal.AddArc($terminalLeft + $terminalWidth - $terminalDiameter, $terminalTop, $terminalDiameter, $terminalDiameter, 270, 90)
    $terminal.AddArc($terminalLeft + $terminalWidth - $terminalDiameter, $terminalTop + $terminalHeight - $terminalDiameter, $terminalDiameter, $terminalDiameter, 0, 90)
    $terminal.AddArc($terminalLeft, $terminalTop + $terminalHeight - $terminalDiameter, $terminalDiameter, $terminalDiameter, 90, 90)
    $terminal.CloseFigure()
    $graphics.DrawPath($border, $terminal)

    $prompt = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 235, 248, 246), [Math]::Max(1.0, 3.5 * $scale))
    $prompt.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $prompt.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $graphics.DrawLine($prompt, 23.0 * $scale, 27.0 * $scale, 29.0 * $scale, 32.0 * $scale)
    $graphics.DrawLine($prompt, 29.0 * $scale, 32.0 * $scale, 23.0 * $scale, 37.0 * $scale)

    $cursor = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 104, 225, 146), [Math]::Max(1.0, 3.5 * $scale))
    $cursor.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $cursor.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $graphics.DrawLine($cursor, 35.0 * $scale, 39.0 * $scale, 43.0 * $scale, 39.0 * $scale)

    $stream = [System.IO.MemoryStream]::new()
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $bytes = $stream.ToArray()

    $cursor.Dispose()
    $prompt.Dispose()
    $terminal.Dispose()
    $border.Dispose()
    $backgroundBrush.Dispose()
    $background.Dispose()
    $graphics.Dispose()
    $bitmap.Dispose()
    $stream.Dispose()
    return ,$bytes
}

$sizes = @(16, 32, 48, 64, 128, 256)
$images = [System.Collections.Generic.List[byte[]]]::new()
foreach ($size in $sizes) {
    [void]$images.Add((New-RustSshPng -Size $size))
}

$outputPath = Join-Path $PSScriptRoot "rust-ssh.ico"
$stream = [System.IO.File]::Open($outputPath, [System.IO.FileMode]::Create)
$writer = [System.IO.BinaryWriter]::new($stream)
$writer.Write([UInt16]0)
$writer.Write([UInt16]1)
$writer.Write([UInt16]$sizes.Count)

$offset = 6 + (16 * $sizes.Count)
for ($index = 0; $index -lt $sizes.Count; $index++) {
    $size = $sizes[$index]
    $dimension = if ($size -eq 256) { [byte]0 } else { [byte]$size }
    $writer.Write($dimension)
    $writer.Write($dimension)
    $writer.Write([byte]0)
    $writer.Write([byte]0)
    $writer.Write([UInt16]1)
    $writer.Write([UInt16]32)
    $writer.Write([UInt32]$images[$index].Length)
    $writer.Write([UInt32]$offset)
    $offset += $images[$index].Length
}
foreach ($image in $images) {
    $writer.Write($image)
}

$writer.Dispose()
$stream.Dispose()
Write-Host "Created $outputPath"
