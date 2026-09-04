// Does font smoothing actually change glyph weight on this machine, in the
// exact context configuration the SDK's Metal path uses?
//
// Build and run:
//   clang -o /tmp/measure-glyph-smoothing scripts/measure-glyph-smoothing.m \
//       -framework Foundation -framework CoreGraphics -framework CoreText -framework AppKit
//   /tmp/measure-glyph-smoothing src/fonts/JetBrainsMonoNLNerdFontMono-Regular.ttf 13 2
//
// Args: <font.ttf> [point-size=13] [backing-scale=2]
//
// This exists because phux-cockpit-aht ("all terminal text reads too faint")
// survived three confident diagnoses that were each argued from grep rather
// than measurement, and each was wrong. Re-run this before believing anything
// about glyph weight, including the numbers recorded on that issue.
//
// The current pinned SDK explicitly requests smoothing in appkit_host.m. The
// no-smoothing rows below are COUNTERFACTUAL controls: they show that this
// knob can move pixels on this host, not that any shipped Cockpit binary ever
// had smoothing disabled. Compare real SDK commits with
// scripts/host-raster-compare.sh before claiming a visual fix.
//
// The SDK rasterizes every text-bearing command into a CGBitmapContext created
// with kCGImageAlphaPremultipliedLast (a TRANSPARENT backing), sets
// CGContextSetShouldAntialias(true), and (in the current pin) explicitly asks
// for font smoothing. This measures what each toggle costs, if anything.
//
// Four configurations, same font, same size, same string:
//   A. transparent + antialias + no smoothing   <- counterfactual control
//   B. transparent + antialias + smoothing      <- current host intent
//   C. opaque bg   + antialias + smoothing      <- smoothing on a ground
//   D. opaque bg   + antialias + no smoothing   <- counterfactual control
//
// Reported per config: mean coverage over the drawn band and the count of
// pixels above half coverage. Thicker stems => higher mean, more solid pixels.

#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <CoreText/CoreText.h>

typedef struct { double mean; long solid; long lit; } Stats;

static Stats measure(const unsigned char *px, size_t w, size_t h, int opaque) {
    // Coverage of the glyph: for the transparent configs read the alpha
    // channel; for the opaque config read luminance of white-on-black.
    double sum = 0; long solid = 0, lit = 0;
    for (size_t i = 0; i < w * h; i++) {
        unsigned char v = opaque ? px[i * 4 + 0] : px[i * 4 + 3];
        sum += v;
        if (v > 127) solid++;
        if (v > 8) lit++;
    }
    Stats s = { sum / (double)(w * h), solid, lit };
    return s;
}

static Stats run(CTFontRef font, const char *text, size_t w, size_t h,
                 CGFloat scale, int smooth, int opaque) {
    size_t pw = (size_t)(w * scale), ph = (size_t)(h * scale);
    size_t stride = pw * 4;
    unsigned char *buf = calloc(1, stride * ph);
    CGColorSpaceRef cs = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
    CGContextRef ctx = CGBitmapContextCreate(
        buf, pw, ph, 8, stride, cs,
        (opaque ? kCGImageAlphaNoneSkipLast : kCGImageAlphaPremultipliedLast)
            | kCGBitmapByteOrder32Big);

    if (opaque) { // black ground, as the terminal background is #090b0f
        CGContextSetRGBFillColor(ctx, 0.035, 0.043, 0.059, 1.0);
        CGContextFillRect(ctx, CGRectMake(0, 0, pw, ph));
    }
    CGContextScaleCTM(ctx, scale, scale);
    CGContextSetAllowsAntialiasing(ctx, true);
    CGContextSetShouldAntialias(ctx, true);
    CGContextSetAllowsFontSmoothing(ctx, true);
    CGContextSetShouldSmoothFonts(ctx, smooth ? true : false);
    CGContextSetRGBFillColor(ctx, 0.957, 0.969, 0.984, 1.0); // #f4f7fb

    CFStringRef s = CFStringCreateWithCString(NULL, text, kCFStringEncodingUTF8);
    CFStringRef keys[] = { kCTFontAttributeName, kCTForegroundColorFromContextAttributeName };
    CFTypeRef vals[] = { font, kCFBooleanTrue };
    CFDictionaryRef attrs = CFDictionaryCreate(NULL, (const void **)keys, (const void **)vals, 2,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
    CFAttributedStringRef as = CFAttributedStringCreate(NULL, s, attrs);
    CTLineRef line = CTLineCreateWithAttributedString(as);
    CGContextSetTextPosition(ctx, 4, 8);
    CTLineDraw(line, ctx);

    Stats st = measure(buf, pw, ph, opaque);
    CFRelease(line); CFRelease(as); CFRelease(attrs); CFRelease(s);
    CGContextRelease(ctx); CGColorSpaceRelease(cs); free(buf);
    return st;
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        const char *fontPath = argv[1];
        CGFloat size = argc > 2 ? atof(argv[2]) : 13.0;
        CGFloat scale = argc > 3 ? atof(argv[3]) : 2.0;

        CFStringRef p = CFStringCreateWithCString(NULL, fontPath, kCFStringEncodingUTF8);
        CFURLRef url = CFURLCreateWithFileSystemPath(NULL, p, kCFURLPOSIXPathStyle, false);
        CGDataProviderRef dp = CGDataProviderCreateWithURL(url);
        CGFontRef cg = CGFontCreateWithDataProvider(dp);
        if (!cg) { fprintf(stderr, "could not load font %s\n", fontPath); return 1; }
        CTFontRef font = CTFontCreateWithGraphicsFont(cg, size, NULL, NULL);

        const char *text = "the quick brown fox $ ls -la | grep foo";
        printf("font=%s size=%.1f scale=%.1f\n", fontPath, size, scale);
        printf("%-34s %10s %10s %10s\n", "configuration", "mean", "solid>127", "lit>8");

        Stats a = run(font, text, 320, 20, scale, 0, 0);
        printf("%-34s %10.4f %10ld %10ld\n", "A transparent, no smoothing (control)", a.mean, a.solid, a.lit);
        Stats b = run(font, text, 320, 20, scale, 1, 0);
        printf("%-34s %10.4f %10ld %10ld\n", "B transparent, smoothing on", b.mean, b.solid, b.lit);
        Stats c = run(font, text, 320, 20, scale, 1, 1);
        printf("%-34s %10.4f %10ld %10ld\n", "C opaque bg, smoothing on", c.mean, c.solid, c.lit);
        Stats d = run(font, text, 320, 20, scale, 0, 1);
        printf("%-34s %10.4f %10ld %10ld\n", "D opaque bg, no smoothing", d.mean, d.solid, d.lit);

        printf("\nB vs A mean delta: %+.2f%%\n", (b.mean - a.mean) / a.mean * 100.0);
        printf("C vs D mean delta: %+.2f%%  (smoothing effect on an opaque ground)\n",
               (c.mean - d.mean) / d.mean * 100.0);
        return 0;
    }
}
