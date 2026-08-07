#!/bin/zsh
# Generates AppIcon.icns, the macOS app icon.
# The artwork matches the tray icons (the shapes in src/tray/art.rs): a white camera on a green
# rounded background, drawn in Swift (CoreGraphics). Keep the numbers below in sync with art.rs,
# which renders the very same picture for the macOS menu bar.
# The generated file is committed, so there is no need to rerun this unless the
# artwork changes.
# Usage: packaging/make-icns.sh
set -euo pipefail

SCRIPT_DIR="${0:A:h}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

swift - "$WORK/icon-1024.png" <<'SWIFT'
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let size = 1024
let ctx = CGContext(
    data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
    space: CGColorSpace(name: CGColorSpace.sRGB)!,
    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!

// Flip so that y=0 is the top, and draw in the same 32-unit coordinate system as tray/windows.rs
ctx.translateBy(x: 0, y: CGFloat(size))
ctx.scaleBy(x: 1, y: -1)
let margin: CGFloat = 100        // padding that matches the macOS icon grid
let scale = (CGFloat(size) - margin * 2) / 32
ctx.translateBy(x: margin, y: margin)
ctx.scaleBy(x: scale, y: scale)

func rounded(_ x0: CGFloat, _ y0: CGFloat, _ x1: CGFloat, _ y1: CGFloat, _ r: CGFloat) -> CGPath {
    CGPath(roundedRect: CGRect(x: x0, y: y0, width: x1 - x0, height: y1 - y0),
           cornerWidth: r, cornerHeight: r, transform: nil)
}

let green = CGColor(srgbRed: 46 / 255, green: 160 / 255, blue: 67 / 255, alpha: 1)
let white = CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 1)

// Rounded background (matching the corner ratio of the Big Sur and later icon shape)
ctx.setFillColor(green)
ctx.addPath(rounded(0, 0, 32, 32, 7.2))
ctx.fillPath()

// Shrink the camera around its own center (16, 16.5) so the green reads as a background
// (GLYPH_SCALE in art.rs)
ctx.translateBy(x: 16, y: 16)
ctx.scaleBy(x: 0.75, y: 0.75)
ctx.translateBy(x: -16, y: -16.5)

// Camera body and viewfinder (same coordinates as camera_ink in tray/art.rs)
ctx.setFillColor(white)
ctx.addPath(rounded(2, 10, 30, 28, 3))
ctx.addPath(rounded(8, 5, 16, 11, 1.5))
ctx.fillPath()

// Punch out the lens ring in the background color (centered at (16, 19.5), ring from radius 4.2 to 6.2)
ctx.setStrokeColor(green)
ctx.setLineWidth(2.0)
ctx.strokeEllipse(in: CGRect(x: 16 - 5.2, y: 19.5 - 5.2, width: 10.4, height: 10.4))

let image = ctx.makeImage()!
let url = URL(fileURLWithPath: CommandLine.arguments[1]) as CFURL
let dest = CGImageDestinationCreateWithURL(url, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(dest, image, nil)
guard CGImageDestinationFinalize(dest) else { fatalError("failed to write PNG") }
SWIFT

ICONSET="$WORK/AppIcon.iconset"
mkdir "$ICONSET"
for px in 16 32 64 128 256 512 1024; do
    sips -z $px $px "$WORK/icon-1024.png" --out "$WORK/icon-$px.png" >/dev/null
done
cp "$WORK/icon-16.png"   "$ICONSET/icon_16x16.png"
cp "$WORK/icon-32.png"   "$ICONSET/icon_16x16@2x.png"
cp "$WORK/icon-32.png"   "$ICONSET/icon_32x32.png"
cp "$WORK/icon-64.png"   "$ICONSET/icon_32x32@2x.png"
cp "$WORK/icon-128.png"  "$ICONSET/icon_128x128.png"
cp "$WORK/icon-256.png"  "$ICONSET/icon_128x128@2x.png"
cp "$WORK/icon-256.png"  "$ICONSET/icon_256x256.png"
cp "$WORK/icon-512.png"  "$ICONSET/icon_256x256@2x.png"
cp "$WORK/icon-512.png"  "$ICONSET/icon_512x512.png"
cp "$WORK/icon-1024.png" "$ICONSET/icon_512x512@2x.png"

iconutil -c icns "$ICONSET" -o "$SCRIPT_DIR/AppIcon.icns"
echo "Built: $SCRIPT_DIR/AppIcon.icns"
